//! GM loyalty-point administration shared by the slash commands (`/lp`, `/addlp`,
//! `/setlp` and the six EverMarks commands). There is no client service mirror;
//! the wallet and LP-store services own the ordinary reads.
//! Balances live in `game.lp_balances`, keyed by owner and issuer; EverMarks are
//! the balance issued by `CORP_HERALDRY` (client `appConst.py` corpHeraldry).
//! Absolute sets replace only the observed balance (compare-and-set), so a
//! change committed between the read and the write is refused instead of being
//! folded into the target. Corporation membership is re-read after the
//! organization admission locks, so a transfer or deletion during the lock wait
//! refuses the change instead of crediting an organization the character left.
//! GM adjustments journal no earning event and advance no daily goals.

use super::{db_err, notify_error, CallError};
use crate::client::filetime_now;
use crate::dao::{characters::{members, selection}, corporations::registry};
use crate::db::{executor::Executor, outbox::{dispatch_world, Outbox}, Db};
use crate::services::identity::Identity;
use crate::sde::Sde;
use crate::space::World;
use infinity_core::messages;
use marshal::Value;

/// Which owner a GM loyalty command acts on.
#[derive(Clone, Copy)]
pub(crate) enum Scope { Character, Corporation }

/// Requested balance change: a signed delta or an absolute target.
#[derive(Clone, Copy)]
pub(crate) enum Change { Add(i64), Set(i64) }

/// The owner a command resolved and its exact resulting balance.
pub(crate) struct Balance { pub owner_id: i64, pub amount: i64 }

/// Read an owner's Heraldry LP balance (EverMarks); a missing row reports zero.
/// Read-only: no locks, writes or notifications, and the membership is the one
/// committed in the database, never the session attribute.
pub(crate) async fn gm_balance(db: &Db, sde: &Sde, character: i64, scope: Scope) -> Result<Balance, CallError> {
    let owner = match scope {
        _ if character <= 0 => return Err(notify_error(messages::gm::GM_ARGS_NO_CHARACTER)),
        Scope::Character => character,
        Scope::Corporation => current_corporation(db, sde, character).await?
            .ok_or_else(|| notify_error(messages::gm::GM_LP_CURRENT_CORPORATION_REQUIRED))?,
    };
    Ok(Balance { owner_id: owner, amount: super::lp_balance(db, owner, infinity_core::consts::CORP_HERALDRY).await? })
}

/// Apply one GM balance change under the market lock and the organization
/// locks, then dispatch the committed notifications. `identity.char_id` is the
/// operator; `identity.corp_id` is never trusted as the owner.
pub(crate) async fn gm_change(db: &Db, world: &World, sde: &std::sync::Arc<Sde>, identity: Identity, issuer: i64, scope: Scope, change: Change) -> Result<Balance, CallError> {
    let me = identity.char_id;
    if me <= 0 { return Err(notify_error(messages::gm::GM_ARGS_NO_CHARACTER)); }
    let occurred_at = filetime_now();
    let _trading = crate::services::market::lock().await;
    let corporation = matches!(scope, Scope::Corporation);
    db.transaction(|tx, outbox| {
        // The retryable operation closure must not carry a borrowed dataset.
        let sde = sde.clone();
        Box::pin(async move {
            let owner = match scope {
                Scope::Character => {
                    selection::lock_character(tx, me).await.map_err(db_err)?;
                    if !registry::active_character(tx, me).await.map_err(db_err)? {
                        return Err(notify_error(messages::gm::GM_ARGS_NO_CHARACTER));
                    }
                    me
                }
                Scope::Corporation => {
                    let Some(candidate) = registry::character_corp(tx, me).await.map_err(db_err)? else {
                        return Err(notify_error(messages::gm::GM_LP_CURRENT_CORPORATION_REQUIRED));
                    };
                    if !registry::lock_admission(tx, &[], &[candidate], &[me]).await.map_err(db_err)? {
                        return Err(notify_error(messages::gm::GM_LP_MEMBERSHIP_CHANGED));
                    }
                    match current_corporation(tx, &sde, me).await? {
                        Some(found) if found == candidate => found,
                        Some(_) => return Err(notify_error(messages::gm::GM_LP_MEMBERSHIP_CHANGED)),
                        None => return Err(notify_error(messages::gm::GM_LP_CURRENT_CORPORATION_REQUIRED)),
                    }
                }
            };
            let amount = match change {
                Change::Add(delta) => match super::change_lp(tx, outbox, owner, issuer, delta, corporation, super::LoyaltyMutation {
                    reason: crate::lua::events::resources::LoyaltyReason::GmAdjustment,
                    occurred_at,
                    context: crate::lua::events::resources::FactContext {
                        corporation_id: None, alliance_id: None, faction_id: None,
                        ship_id: identity.ship_id.filter(|id| *id > 0), ship_type_id: None,
                        system_id: identity.system_id.filter(|id| *id > 0), station_id: identity.station_id.filter(|id| *id > 0),
                        origin: crate::lua::events::resources::FactOrigin::Gm,
                    },
                }).await? {
                    Some(after) => after,
                    None => super::lp_balance(tx, owner, issuer).await?,
                },
                Change::Set(target) => set_tx(tx, outbox, owner, issuer, target, corporation).await?,
            };
            Ok(Balance { owner_id: owner, amount })
        })
    }, |value, outbox| async move { dispatch_world(value, outbox, world) }).await
}

/// Queue the post-commit LP notification a committed change produces; an
/// unchanged balance notifies nobody. Characters get their before/after values;
/// corporation changes are announced to every active member once.
pub(super) async fn notify_change<X: Send>(ex: &(impl Executor + ?Sized), outbox: &mut Outbox<X>, owner: i64, issuer: i64, before: i64, after: i64, corporation: bool) -> Result<(), CallError> {
    if before == after { return Ok(()); }
    if corporation {
        for member in members::active_ids(ex, owner).await.map_err(db_err)? {
            outbox.notify_pilot(member, "OnCorpLPChange", vec![Value::str("lp"), Value::Int(issuer)]);
        }
    } else {
        outbox.notify_pilot(owner, "OnLPChange", vec![Value::str("lp"), Value::Int(issuer), Value::Int(before), Value::Int(after)]);
    }
    Ok(())
}

/// Set `owner`'s balance to `amount` only if it still equals the value read in
/// this transaction; a miss, a negative target or a concurrent insert refuses
/// the whole change without retry.
async fn set_tx<X: Send>(ex: &(impl Executor + ?Sized), outbox: &mut Outbox<X>, owner: i64, issuer: i64, amount: i64, corporation: bool) -> Result<i64, CallError> {
    if amount < 0 { return Err(notify_error(messages::gm::GM_LP_BALANCE_INVALID)); }
    let expected = crate::dao::wallets::loyalty::balance(ex, owner, issuer).await.map_err(db_err)?;
    let Some(after) = crate::dao::wallets::loyalty::replace(ex, owner, issuer, expected, amount).await.map_err(db_err)? else {
        return Err(notify_error(messages::gm::GM_LP_BALANCE_CHANGED));
    };
    notify_change(ex, outbox, owner, issuer, expected.unwrap_or(0), after, corporation).await?;
    Ok(after)
}

/// The character's current valid corporation from committed game rows: a known
/// SDE NPC corporation, or a live player corporation whose row still exists.
async fn current_corporation(ex: &(impl Executor + ?Sized), sde: &Sde, character: i64) -> Result<Option<i64>, CallError> {
    let Some(organization) = members::organization(ex, character).await.map_err(db_err)? else { return Ok(None); };
    let corporation = organization.corporation_id;
    if sde.npc_corporations.get(corporation).is_some() { return Ok(Some(corporation)); }
    if crate::services::corps::is_player_corp(corporation) && registry::headquarters(ex, corporation).await.map_err(db_err)?.is_some() {
        return Ok(Some(corporation));
    }
    Ok(None)
}

#[cfg(all(test, feature = "tests-social"))]
mod tests;
