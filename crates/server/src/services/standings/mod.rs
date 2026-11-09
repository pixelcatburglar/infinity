//! NPC standings and loyalty points (standingMgr, LPSvc, LPStoreMgr;
//! eve/client/script/ui/services/standingsvc.py, station/loyaltyPointStore).
//!
//! Standings (game.standings) are what NPC agents, corporations and factions think of a
//! character, from -10 to +10. Completing an agent's mission raises the agent's, its
//! corporation's and its faction's; declining or quitting one costs a little with the agent
//! and its corporation. Gains and losses move a standing part of the way to +10 or -10, so
//! they shrink as it approaches the limit. Agents above level 1 need an effective standing
//! (the best of the three, unless any is -2 or worse) of 1, 3, 5 or 7. Mission
//! changes use the caller's executor and outbox so completion dispatches after commit.
//! Mission gain and decline rates are operator-configured.
//!
//! Loyalty balances use game.lp_balances for both characters and corporations. Imported
//! game.lp_offers are authoritative; only a globally empty catalogue uses synthesized
//! faction offers derived from retail prices. Analysis Kredits cannot be spent.
//! Positive natural character mission LP rewards journal LoyaltyPointsEarned with the balance
//! mutation, using transactional organization/ship data; corporate LP and other reasons are excluded.

use super::{db_err, notify_error, Call, CallError, CallResult};
use crate::client::Client;
use crate::db::Db;
use crate::db::executor::Executor;
use crate::db::outbox::Outbox;
use infinity_core::messages;
use crate::lua::events::{GameEvent, resources::{FactContext, FactOrigin, LoyaltyReason}};
use marshal::Value;

mod standing_mgr;
mod lp_svc;
mod lp_store_mgr;

/// Client standings convention: absolute standing limit.
const STANDING_LIMIT: f64 = 10.0;
/// Client standingsvc.GetEffectiveStandingWithAgent rejection threshold.
const AGENT_STANDING_REJECTION: f64 = -2.0;
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

/// Set an absolute NPC standing within the client standing range.
pub(crate) async fn set(db: &(impl Executor + ?Sized), char_id: i64, from: i64, value: f64) -> Result<(), CallError> {
    if !value.is_finite() || !(-STANDING_LIMIT..=STANDING_LIMIT).contains(&value) {
        return Err(notify_error(messages::standings::STANDING_OUT_OF_RANGE));
    }
    crate::dao::characters::standings::set(db, char_id, from, value).await.map_err(db_err)?;
    Ok(())
}

/// Queue the client notification for an absolute standing change.
pub(crate) fn notify_set<X: Send>(outbox: &mut Outbox<X>, char_id: i64, from: i64, value: f64) {
    outbox.notify_pilot(char_id, "OnStandingSet", vec![Value::Int(from), Value::Int(char_id), Value::Float(value)]);
}

/// Move `from`'s standing toward the character `change` of the way to +10 (or -10 if negative).
async fn modify(db: &(impl Executor + ?Sized), char_id: i64, from: i64, change: f64) -> Result<Option<f64>, CallError> {
    if from == 0 || change == 0.0 {
        return Ok(None);
    }
    let old = standing(db, char_id, from).await?;
    let new = if change > 0.0 { old + (STANDING_LIMIT - old) * change } else { old + (STANDING_LIMIT + old) * change }.clamp(-STANDING_LIMIT, STANDING_LIMIT);
    set(db, char_id, from, new).await?;
    Ok(Some(new))
}

/// standingsvc.GetEffectiveStandingWithAgent: the best of the agent's, its corporation's and
/// its faction's standing, unless one of them is -2 or worse.
pub async fn effective(db: &Db, char_id: i64, agent: i64, corp: i64, faction: i64) -> Result<f64, CallError> {
    let s = [standing(db, char_id, faction).await?, standing(db, char_id, corp).await?, standing(db, char_id, agent).await?];
    let min = s.iter().copied().fold(f64::INFINITY, f64::min);
    Ok(if min <= AGENT_STANDING_REJECTION { min } else { s.iter().copied().fold(f64::NEG_INFINITY, f64::max) })
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

/// A mission was completed for an agent of `level`.
pub async fn mission_completed<X: Send>(db: &(impl Executor + ?Sized), outbox: &mut Outbox<X>, char_id: i64, agent: i64, corp: i64, faction: i64, level: i64) -> Result<(), CallError> {
    let policy = &crate::config::game().standings;
    let gain = policy.mission_corp_gain * level.max(1) as f64;
    let changes = [(agent, gain * policy.mission_agent_gain_multiplier), (corp, gain), (faction, gain / policy.mission_faction_gain_divisor)];
    apply_mission_standings(db, outbox, char_id, &changes).await
}

/// A mission was declined or quit.
pub async fn mission_declined<X: Send>(db: &(impl Executor + ?Sized), outbox: &mut Outbox<X>, char_id: i64, agent: i64, corp: i64) -> Result<(), CallError> {
    let loss = crate::config::game().standings.decline_loss;
    apply_mission_standings(db, outbox, char_id, &[(agent, loss), (corp, loss)]).await
}

async fn apply_mission_standings<X: Send, const N: usize>(db: &(impl Executor + ?Sized), outbox: &mut Outbox<X>, char_id: i64, changes: &[(i64, f64); N]) -> Result<(), CallError> {
    let mut order: [usize; N] = std::array::from_fn(|i| i);
    order.sort_unstable_by_key(|&i| (changes[i].0, i));
    let mut values = [None; N];
    for i in order {
        values[i] = modify(db, char_id, changes[i].0, changes[i].1).await?;
    }
    for (i, value) in values.into_iter().enumerate() {
        if let Some(new) = value {
            notify_set(outbox, char_id, changes[i].0, new);
        }
    }
    Ok(())
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
pub(super) async fn change_lp<X: Send>(db: &(impl Executor + ?Sized), outbox: &mut Outbox<X>, owner: i64, issuer: i64, amount: i64, corporation: bool, mut mutation: LoyaltyMutation) -> Result<(), CallError> {
    if amount == 0 || issuer == 0 { return Ok(()); }
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
    if corporation {
        for member in crate::dao::characters::members::active_ids(db, owner).await.map_err(db_err)? {
            outbox.notify_pilot(member, "OnCorpLPChange", vec![Value::str("lp"), Value::Int(issuer)]);
        }
    } else {
        outbox.notify_pilot(owner, "OnLPChange", vec![Value::str("lp"), Value::Int(issuer), Value::Int(after - amount), Value::Int(after)]);
    }
    Ok(())
}

/// Add or take character loyalty points.
pub(crate) async fn add_lp<X: Send>(db: &(impl Executor + ?Sized), outbox: &mut Outbox<X>, char_id: i64, corp: i64, amount: i64, mutation: LoyaltyMutation) -> Result<(), CallError> {
    change_lp(db, outbox, char_id, corp, amount, false, mutation).await
}

pub(crate) async fn initialize(db: &crate::db::Db) -> Result<(), crate::db::Error> {
    if crate::dao::lp_offers::is_empty(db).await? {
        tracing::warn!("LP offer catalogue is empty; using synthetic retail-derived offers");
    }
    Ok(())
}

pub async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    match call.service {
        "standingMgr" => standing_mgr::dispatch(c, call).await,
        "LPSvc" => lp_svc::dispatch(c, call).await,
        "LPStoreMgr" => lp_store_mgr::dispatch(c, call).await,
        _ => None,
    }
}

#[cfg(test)]
mod tests;
