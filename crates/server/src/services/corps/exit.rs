//! Shared voluntary-exit and director-kick admission.
//! The client calls these rules through corpRegistry; durable intents are handled by pending_kicks.
use super::*;
use crate::{client::filetime_now, db::Db, db::executor::Executor, db::outbox::dispatch_world, sde::Sde, services::user_error, space::World};
use std::sync::Arc;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Admission { Denied, AwaitRoleRemoval, WaitUntil(i64), AwaitSafePresence, Ready }
fn has_rights(a: &permissions::Authority) -> bool { a.roles.iter().chain(a.grantable.iter()).any(|mask| *mask != 0) }

pub(crate) fn deadline(removed: i64) -> Result<i64, CallError> {
    let ticks = i128::from(crate::config::game().corporations.role_removal_cooldown_secs) * i128::from(FILETIME_SECOND);
    i128::from(removed).checked_add(ticks).and_then(|v| i64::try_from(v).ok()).ok_or_else(|| CallError::Internal(anyhow::anyhow!("corporation role removal deadline overflow")))
}

pub(crate) async fn admit(ex: &(impl Executor + ?Sized), world: Option<&World>, target: i64, issuer: i64, expected: i64, self_request: bool, now: i64) -> Result<Admission, CallError> {
    let Some(authority) = permissions::load(ex, target).await? else { return Ok(Admission::Denied) };
    if !authority.live || authority.corporation != expected || !is_player_corp(expected) || (authority.ceo == Some(target) && !self_request) { return Ok(Admission::Denied) }
    if !self_request {
        let Some(actor) = permissions::load(ex, issuer).await? else { return Ok(Admission::Denied) };
        if actor.corporation != expected || !actor.live || actor.roles[0] & ROLE_DIRECTOR == 0 { return Ok(Admission::Denied) }
    } else if authority.ceo == Some(target) { return Ok(Admission::Denied) }
    if has_rights(&authority) { return Ok(Admission::AwaitRoleRemoval) }
    let (station, removed) = registry::exit_facts(ex, target).await.map_err(db_err)?;
    if let Some(removed) = removed {
        let deadline = deadline(removed)?;
        if now < deadline { return Ok(Admission::WaitUntil(deadline)); }
    }
    Ok(if world.is_some_and(|world| world.is_online(target)) && station.is_none() { Admission::AwaitSafePresence } else { Admission::Ready })
}

pub(crate) async fn request(ex: &(impl Executor + ?Sized), world: &World, target: i64, issuer: i64, corp: i64, now: i64) -> Result<Admission, CallError> {
    let result = admit(ex, Some(world), target, issuer, corp, false, now).await?;
    match result {
        Admission::Ready | Admission::Denied => Ok(result),
        Admission::AwaitRoleRemoval | Admission::WaitUntil(_) | Admission::AwaitSafePresence => {
            if crate::dao::corporations::pending_kicks::get(ex, corp, target).await.map_err(db_err)?.is_some() {
                // A repeated request is idempotent: preserve the original
                // identity and never recreate a consumed due-in-space job.
                return Ok(result);
            }
            let eligible_at = match result {
                Admission::WaitUntil(at) => Some(at),
                Admission::AwaitSafePresence => Some(now),
                _ => None,
            };
            let row = crate::dao::corporations::pending_kicks::PendingKick { corporation_id: corp, character_id: target, issuer_id: issuer, requested_at: now, eligible_at, reason: 1 };
            crate::dao::corporations::pending_kicks::upsert(ex, &row).await.map_err(db_err)?;
            if row.eligible_at.is_some() {
                super::pending_kicks::schedule(ex, &row).await.map_err(db_err)?;
            } else {
                super::pending_kicks::cancel(ex, &row).await.map_err(db_err)?;
            }
            Ok(result)
        }
    }
}
pub(super) async fn execute(
    db: &Db, sde: &Arc<Sde>, world: &World, expected: i64, target: i64, issuer: i64, stamp: Option<i64>,
) -> Result<Admission, CallError> {
    let sde = sde.clone();
    let transaction_world = world.shared().ok_or_else(|| CallError::Internal(anyhow::anyhow!("world unavailable during corporation exit")))?;
    let now = filetime_now();
    db.transaction(
        |tx, outbox| {
            let sde = sde.clone();
            let world = transaction_world.clone();
            Box::pin(async move {
                if !crate::dao::corporations::registry::lock_admission(tx, &[], &[expected], &[issuer, target]).await.map_err(db_err)? {
                    return Err(CallError::Internal(anyhow::anyhow!("corporation association changed during pending kick execution")));
                }
                crate::dao::characters::selection::lock_character(tx, target).await.map_err(db_err)?;
                let pending = crate::dao::corporations::pending_kicks::get(tx, expected, target).await.map_err(db_err)?;
                if let Some(stamp) = stamp {
                    if pending.as_ref().is_none_or(|row| row.issuer_id != issuer || row.requested_at != stamp) {
                        return Ok(Admission::Denied);
                    }
                }
                let admission = admit(tx, Some(&world), target, issuer, expected, target == issuer, now).await?;
                match admission {
                    Admission::Ready => {
                        super::leave_inner(tx, &sde, outbox, target, issuer, now).await?;
                        if let Some(row) = pending {
                            super::pending_kicks::cancel(tx, &row).await.map_err(db_err)?;
                            crate::dao::corporations::pending_kicks::delete(tx, expected, target).await.map_err(db_err)?;
                        }
                    }
                    Admission::Denied if stamp.is_some() => {
                        if let Some(row) = pending {
                            super::pending_kicks::cancel(tx, &row).await.map_err(db_err)?;
                            crate::dao::corporations::pending_kicks::delete(tx, expected, target).await.map_err(db_err)?;
                        }
                    }
                    Admission::AwaitRoleRemoval | Admission::WaitUntil(_) | Admission::AwaitSafePresence if target != issuer && stamp.is_none() => {
                        request(tx, &world, target, issuer, expected, now).await?;
                    }
                    _ => {}
                }
                Ok(admission)
            })
        },
        |value, outbox| async move { crate::scheduler::wake(); dispatch_world(value, outbox, world) },
    ).await
}
pub(crate) fn refusal(result: Admission) -> CallError {
    match result {
        Admission::Denied => user_error("CrpCantQuitDefaultCorporation", Vec::new()),
        Admission::AwaitRoleRemoval => user_error("CrpCantQuitNotInStasis", Vec::new()),
        Admission::WaitUntil(deadline) => user_error("CrpCantQuitNotInStasis", vec![("deadline".into(), marshal::Value::Int(deadline))]),
        Admission::AwaitSafePresence => user_error("CrpCanNotQuitMemberInSpace", Vec::new()),
        Admission::Ready => notify_error(infinity_core::messages::CANNOT_LEAVE_NPC_CORP),
    }
}
pub(crate) fn job_id(character: i64) -> String { format!("corporations.pending_kick:{character}") }
