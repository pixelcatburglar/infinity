//! NPC standings and loyalty points (standingMgr, LPSvc, LPStoreMgr;
//! eve/client/script/ui/services/standingsvc.py, eve/common/script/util/standingUtil.py,
//! station/loyaltyPointStore).
//!
//! Standings (game.standings) are what NPC agents, corporations and factions think of a
//! character, from -10 to +10. Every production change goes through the shared
//! `mutation::apply_tx` writer, which validates the range, locks the character,
//! journals each change to game.standing_history and queues `OnStandingSet`
//! inside the caller's transaction, so value, history and notification commit
//! or roll back together. `standingMgr.GetStandingTransactions` serves that
//! journal; `services::standings::gm` applies it for bulk GM raises. Completing
//! an agent's mission raises the agent's, its corporation's and its faction's;
//! declining or quitting one costs a little with the agent and its corporation.
//! Gains and losses move a standing part of the way to +10 or -10, so they
//! shrink as it approaches the limit. Agents above level 1 need an effective
//! standing (the best of the three after the `bonuses` skill adjustment, unless
//! any is -2 or worse) of 1, 3, 5 or 7.
//! Mission gain and decline rates are operator-configured.
//!
//! Loyalty balances use game.lp_balances for both characters and corporations. Imported
//! game.lp_offers are authoritative; only a globally empty catalogue uses synthesized
//! faction offers derived from retail prices. Analysis Kredits cannot be spent.
//! Positive natural character mission LP rewards journal LoyaltyPointsEarned with the balance
//! mutation, using transactional organization/ship data; corporate LP and other reasons are excluded.
//! `services::standings::loyalty` owns the GM administration path used by the slash
//! commands: absolute sets compare-and-set the observed balance, corporation writes
//! re-check membership under the organization locks, and neither journals an earning.

use super::{db_err, notify_error, Call, CallError, CallResult};
use crate::client::Client;
use crate::db::Db;
use crate::db::executor::Executor;
use crate::db::outbox::Outbox;
use infinity_core::messages;
use crate::lua::events::{GameEvent, resources::{FactContext, FactOrigin, LoyaltyReason}};

mod standing_mgr;
mod lp_svc;
mod lp_store_mgr;
pub(crate) mod gm;
pub(crate) mod mutation;
pub(crate) mod loyalty;
pub(crate) mod bonuses;

/// Client standings convention: absolute standing limit.
pub(crate) const STANDING_LIMIT: f64 = 10.0;
/// Client standingsvc agent access: required standing for levels 2 through 5.
const AGENT_LEVEL_STANDINGS: [f64; 4] = [1.0, 3.0, 5.0, 7.0];
/// Client standingsvc agent access: level two.
const AGENT_LEVEL_TWO: i64 = 2;
/// Client standingsvc agent access: level three.
const AGENT_LEVEL_THREE: i64 = 3;
/// Client standingsvc agent access: level four.
const AGENT_LEVEL_FOUR: i64 = 4;

pub async fn standing(db: &(impl Executor + ?Sized), char_id: i64, from: i64) -> Result<f64, CallError> {
    Ok(crate::dao::characters::standings::standing(db, char_id, from).await.map_err(db_err)?.unwrap_or(0.0))
}


/// The effective standing an agent of `level` asks for.
pub fn required_for_level(level: i64) -> f64 {
    match level {
        ..=1 => f64::NEG_INFINITY,
        AGENT_LEVEL_TWO => AGENT_LEVEL_STANDINGS[0],
        AGENT_LEVEL_THREE => AGENT_LEVEL_STANDINGS[1],
        AGENT_LEVEL_FOUR => AGENT_LEVEL_STANDINGS[2],
        _ => AGENT_LEVEL_STANDINGS[3],
    }
}



// ---------------------------------------------------------------- loyalty points

pub async fn lp_balance(db: &(impl Executor + ?Sized), char_id: i64, corp: i64) -> Result<i64, CallError> {
    Ok(crate::dao::wallets::loyalty::balance(db, char_id, corp).await.map_err(db_err)?.unwrap_or(0))
}

/// Positive loyalty balances held by a character.
pub(crate) async fn lp_balances(db: &Db, char_id: i64) -> Result<Vec<(i64, i64)>, CallError> {
    Ok(crate::dao::wallets::loyalty::list(db, char_id).await.map_err(db_err)?
        .into_iter().map(|row| (row.corp_id, row.amount)).collect())
}
pub(crate) struct LoyaltyMutation {
    pub reason: LoyaltyReason,
    pub context: FactContext,
    pub occurred_at: i64,
}


/// Change an owner's LP and queue the appropriate post-commit notification.
/// Returns the resulting balance; `None` means the operation was a no-op
/// (zero amount or missing issuer) and nothing was written.
pub(super) async fn change_lp<X: Send>(db: &(impl Executor + ?Sized), outbox: &mut Outbox<X>, owner: i64, issuer: i64, amount: i64, corporation: bool, mut mutation: LoyaltyMutation) -> Result<Option<i64>, CallError> {
    if amount == 0 || issuer == 0 { return Ok(None); }
    let Some(after) = crate::dao::wallets::loyalty::add(db, owner, issuer, amount).await.map_err(db_err)? else {
        let before = lp_balance(db, owner, issuer).await?;
        return Err(notify_error(&messages::loyalty_balance_message(before)));
    };
    if !corporation && amount > 0 && issuer != infinity_core::consts::CORP_HERALDRY && mutation.reason == LoyaltyReason::MissionReward && mutation.context.origin == FactOrigin::Natural {
        let org = crate::dao::characters::members::organization(db, owner).await.map_err(db_err)?;
        mutation.context.corporation_id = org.as_ref().map(|o| o.corporation_id);
        mutation.context.alliance_id = org.and_then(|o| o.alliance_id);
        if let Some(ship) = mutation.context.ship_id {
            mutation.context.ship_type_id = crate::dao::items::loading::load_items(db, crate::dao::items::loading::ItemQuery::Id(ship)).await.map_err(db_err)?.first().map(|item| item.type_id);
        }
        outbox.event(GameEvent::LoyaltyPointsEarned { char_id: owner, issuer_id: issuer, amount, reason: mutation.reason, occurred_at: mutation.occurred_at, context: mutation.context });
    }
    loyalty::notify_change(db, outbox, owner, issuer, after - amount, after, corporation).await?;
    Ok(Some(after))
}

/// Add or take character loyalty points.
pub(crate) async fn add_lp<X: Send>(db: &(impl Executor + ?Sized), outbox: &mut Outbox<X>, char_id: i64, corp: i64, amount: i64, mutation: LoyaltyMutation) -> Result<(), CallError> {
    change_lp(db, outbox, char_id, corp, amount, false, mutation).await.map(|_| ())
}

pub(crate) fn initialize(sde: &crate::sde::Sde) {
    if sde.lp_offers.is_empty() {
        tracing::warn!("LP offer catalogue is empty; using synthetic retail-derived offers");
    }
}

pub async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    match call.service {
        "standingMgr" => standing_mgr::dispatch(c, call).await,
        "LPSvc" => lp_svc::dispatch(c, call).await,
        "LPStoreMgr" => lp_store_mgr::dispatch(c, call).await,
        _ => None,
    }
}

#[cfg(all(test, feature = "tests-social"))]
mod tests;
