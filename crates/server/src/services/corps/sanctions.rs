//! Sanctioned vote actions for `voteManager` (corp_ui_sanctionableactions.py): one
//! decided action per case, created when the close job finds a strict majority for
//! the affirmative option. `finish_in` runs inside the caller's close transaction;
//! `activate` serves the client call that lets the current active CEO execute an
//! action inside its window, and the durable `corp.sanction.expire` job records a
//! lapsed window and releases the blueprint reservation. No RPC lives here.

use super::*;
use crate::dao::corporations::{sanctions as dao, votes as vote_dao};
use crate::dao::corporations::registry;
use crate::dao::items::parent_locks::{LockRequest, LockStrength};
use infinity_core::consts::FILETIME_SECOND;
use infinity_core::messages::corporations as text;
use crate::scheduler::{self, Job};
use crate::services::identity::Identity;
use crate::services::inventory::destination;
use crate::services::market;
use std::{future::Future, pin::Pin, sync::Arc};

/// Durable expiry job kind; the payload is the case ID as a JSON number.
pub(crate) const KIND: &str = "corp.sanction.expire";
fn job_id(case: i64) -> String { format!("{KIND}:{case}") }

fn invalid() -> CallError { votes::invalid() }

/// A strict majority of the weight actually cast for the affirmative option:
/// zero turnout and ties decide nothing.
fn affirmative_majority(options: &[vote_dao::OptionRow]) -> bool {
    let total: i64 = options.iter().map(|option| option.votes_for).sum();
    total > 0 && options.iter().any(|option| {
        option.option_id == vote_request::OPTION_POSITIVE && option.votes_for > total - option.votes_for
    })
}

/// Vote types whose decided affirmative creates an executable sanctioned action.
/// General votes keep their tallies as the only, read-only outcome.
fn actionable(vote_type: i64) -> bool {
    matches!(vote_type,
        vote_request::VOTE_CEO | vote_request::VOTE_SHARES | vote_request::VOTE_KICK
            | vote_request::VOTE_LOCKDOWN | vote_request::VOTE_UNLOCK)
}

fn action_expires(case: &vote_dao::Case) -> Result<i64, CallError> {
    let window = i128::from(crate::config::game().corporations.vote_action_window_secs) * i128::from(FILETIME_SECOND);
    i128::from(case.ends).checked_add(window).and_then(|v| i64::try_from(v).ok())
        .ok_or_else(|| CallError::Internal(anyhow::anyhow!("sanctioned action deadline overflow")))
}

/// Release a case's blueprint reservation under the same item row lock that enacted
/// lock changes hold. The guarded update only clears a pointer still naming the item.
async fn release_reservation(ex: &(impl Executor + ?Sized), sde: &Sde, case: &vote_dao::Case) -> Result<(), CallError> {
    let Some(item) = case.reserved_item_id else { return Ok(()) };
    let _locks = destination::lock_items(ex, sde, &[LockRequest { item_id: item, strength: LockStrength::Update }], &[item]).await?;
    vote_dao::release_reservation(ex, case.id, item).await.map_err(db_err)?;
    Ok(())
}

/// Decide `case` after its guarded close inside the caller's transaction. A CEO
/// election resolves through `close_election` against live eligibility, not the
/// execution window: a decided election must still hand over after a late
/// restart. Every other affirmative strict majority creates the unique
/// sanctioned action inside `expires` (or records it expired when the close job
/// ran past the whole window rather than granting a fresh one), and the
/// blueprint reservation is released when no live pending action keeps it.
pub(super) async fn finish_in<X: Send>(ex: &(impl Executor + ?Sized), sde: &Sde, outbox: &mut Outbox<X>, case: &vote_dao::Case, now: i64) -> Result<(), CallError> {
    let options = vote_dao::options(ex, case.id).await.map_err(db_err)?;
    let won = actionable(case.vote_type) && affirmative_majority(&options);
    let mut live_action = false;
    if case.vote_type == vote_request::VOTE_CEO {
        close_election(ex, sde, outbox, case, &options, won, now).await?;
    } else if won {
        let expires = action_expires(case)?;
        let action = dao::Action {
            case_id: case.id, option_id: vote_request::OPTION_POSITIVE, expires,
            acted_upon: false, in_effect: false, time_acted_upon: None,
            time_rescinded: None, expired_at: None,
        };
        if dao::create(ex, &action).await.map_err(db_err)? {
            live_action = expires > now;
            if live_action {
                let job = Job { id: job_id(case.id), kind: KIND.into(), due_at: expires, payload: case.id.to_string() };
                scheduler::schedule(ex, &job).await.map_err(db_err)?;
            } else {
                dao::mark_expired(ex, case.id, now).await.map_err(db_err)?;
            }
            votes::notify(ex, outbox, case.corporation_id, case.id, "OnSanctionedActionChanged").await?;
        }
    }
    if !live_action {
        release_reservation(ex, sde, case).await?;
    }
    Ok(())
}

/// Finish a CEO election at close. A strict affirmative majority hands the
/// corporation over whenever the candidate is still eligible and this case still
/// owns the election pointer: the decided outcome does not expire with the
/// activation window, which only bounds manual execution. The election pointer
/// clears before the handover so the audit records the outgoing CEO's real
/// stored authority; success records one already-acted, in-effect action.
/// Anything else restores the frozen CEO's stored authority by clearing the
/// pointer; a corporation whose pointer already moved is left alone.
async fn close_election<X: Send>(ex: &(impl Executor + ?Sized), sde: &Sde, outbox: &mut Outbox<X>, case: &vote_dao::Case, options: &[vote_dao::OptionRow], won: bool, now: i64) -> Result<(), CallError> {
    let Some(previous) = case.previous_ceo_id else { return Ok(()) };
    if won {
        let candidate = options.iter()
            .find(|option| option.option_id == vote_request::OPTION_POSITIVE)
            .and_then(|option| option.parameter)
            .ok_or_else(|| CallError::Internal(anyhow::anyhow!("ceo election case {} lacks its candidate option", case.id)))?;
        if ceo::eligible(ex, sde, case.corporation_id, candidate, now).await?
            && vote_dao::end_election(ex, case.corporation_id, case.id).await.map_err(db_err)?
        {
            ceo::handover_in(ex, sde, outbox, case.corporation_id, previous, candidate, case.creator_id, now).await?;
            let action = dao::Action {
                case_id: case.id, option_id: vote_request::OPTION_POSITIVE,
                expires: action_expires(case)?, acted_upon: true, in_effect: true,
                time_acted_upon: Some(now), time_rescinded: None, expired_at: None,
            };
            if !dao::create(ex, &action).await.map_err(db_err)? {
                return Err(CallError::Internal(anyhow::anyhow!("election action already recorded")));
            }
            votes::notify(ex, outbox, case.corporation_id, case.id, "OnSanctionedActionChanged").await?;
            return Ok(());
        }
    }
    if vote_dao::end_election(ex, case.corporation_id, case.id).await.map_err(db_err)?
        && case.previous_ceo_was_active == Some(true) {
        // The freeze removed real active authority, so restoring it is a real
        // session change; an inactive CEO's projection never moved.
        ceo::role_projection_changed_in(ex, outbox, case.corporation_id, previous).await?;
    }
    Ok(())
}

/// The action's winning option parameters as the immutable option row recorded
/// them; never the caller's own arguments.
async fn winning_option(ex: &(impl Executor + ?Sized), case: i64, option_id: i32) -> Result<vote_dao::OptionRow, CallError> {
    vote_dao::options(ex, case).await.map_err(db_err)?
        .into_iter().find(|option| option.option_id == option_id)
        .ok_or_else(invalid)
}

/// `voteManager.ActivateSanctionedAction`: execute a decided action as the
/// corporation's current active CEO inside `expires`. A replayed activation on an
/// already-executed action is a no-op; an expired or unauthorized one is refused.
pub(crate) async fn activate(identity: &Identity, db: &Db, world: &World, sde: &Arc<Sde>, case_id: i64) -> Result<(), CallError> {
    let actor = identity.char_id;
    let _market = market::lock().await;
    let transaction_world = world.shared().ok_or_else(|| CallError::Internal(anyhow::anyhow!("world unavailable during sanctioned action")))?;
    let sde = Arc::clone(sde);
    db.transaction(|tx, outbox: &mut Outbox| {
        let world = Arc::clone(&transaction_world);
        let sde = Arc::clone(&sde);
        Box::pin(async move {
            // Classify the admission set before locking: the case's corporation plus
            // the caller and the member an expulsion names.
            let case = vote_dao::case(tx, case_id).await.map_err(db_err)?.ok_or_else(invalid)?;
            let corporation = case.corporation_id;
            let winning = winning_option(tx, case_id,
                dao::get(tx, case_id).await.map_err(db_err)?.ok_or_else(invalid)?.option_id).await?;
            let mut characters = vec![actor];
            if case.vote_type == vote_request::VOTE_KICK {
                if let Some(target) = winning.parameter { characters.push(target); }
            }
            if !registry::lock_admission(tx, &[], &[corporation], &characters).await.map_err(db_err)? {
                return Err(CallError::Internal(anyhow::anyhow!("corporation association changed during sanctioned action")));
            }
            let case = vote_dao::lock_case(tx, case_id).await.map_err(db_err)?.ok_or_else(invalid)?;
            if case.corporation_id != corporation {
                return Err(CallError::Internal(anyhow::anyhow!("vote case corporation changed during sanctioned action")));
            }
            let action = dao::lock(tx, case_id).await.map_err(db_err)?.ok_or_else(invalid)?;
            if !permissions::is_active_ceo(tx, actor, case.corporation_id).await? {
                return Err(notify_error(text::SANCTION_AUTHORITY_REQUIRED));
            }
            if action.acted_upon { return Ok(()); }
            let now = filetime_now();
            if action.expired_at.is_some() || action.expires <= now {
                return Err(notify_error(text::SANCTION_EXPIRED));
            }
            match case.vote_type {
                vote_request::VOTE_SHARES => {
                    let quantity = winning.parameter.ok_or_else(invalid)?;
                    shares::issue_in(tx, outbox, case.corporation_id, quantity).await?;
                }
                vote_request::VOTE_KICK => {
                    let target = winning.parameter.ok_or_else(invalid)?;
                    match exit::request(tx, &world, target, actor, case.corporation_id, now).await? {
                        exit::Admission::Ready => leave_inner(tx, sde.as_ref(), outbox, target, actor, now).await?,
                        // The saved pending intent is the enacted outcome: the
                        // existing role-removal, presence and issuer rules finish it.
                        exit::Admission::AwaitRoleRemoval | exit::Admission::WaitUntil(_) | exit::Admission::AwaitSafePresence => {}
                        exit::Admission::Denied => return Err(notify_error(text::VOTE_TARGET_INVALID)),
                    }
                }
                vote_request::VOTE_LOCKDOWN | vote_request::VOTE_UNLOCK => {
                    let target = item_locks::Target {
                        item_id: winning.parameter.ok_or_else(invalid)?,
                        type_id: i32::try_from(winning.parameter1.ok_or_else(invalid)?).map_err(|_| invalid())?,
                        station_id: winning.parameter2.ok_or_else(invalid)?,
                    };
                    let action_kind = if case.vote_type == vote_request::VOTE_LOCKDOWN {
                        item_locks::Action::Lock
                    } else {
                        item_locks::Action::Unlock
                    };
                    item_locks::apply_in(tx, sde.as_ref(), outbox, case.corporation_id, case_id, target, action_kind, now).await?;
                }
                _ => return Err(invalid()),
            }
            if !dao::mark_acted(tx, case_id, now).await.map_err(db_err)? {
                return Err(CallError::Internal(anyhow::anyhow!("sanctioned action was marked concurrently")));
            }
            votes::notify(tx, outbox, case.corporation_id, case_id, "OnSanctionedActionChanged").await
        })
    }, |value, outbox| async move {
        let result = dispatch_world(value, outbox, world);
        scheduler::wake();
        result
    }).await
}

/// Seed close-time action expiry jobs for every unacted, unexpired action at boot;
/// dead jobs are revived, live ones keep their lease.
pub(crate) async fn seed(db: &Db) -> Result<(), CallError> {
    let _market = market::lock().await;
    let now = filetime_now();
    db.transaction(|tx, _: &mut Outbox| Box::pin(async move {
        for deadline in dao::pending_deadlines(tx).await.map_err(db_err)? {
            let job = Job { id: job_id(deadline.case_id), kind: KIND.into(), due_at: deadline.due_at, payload: deadline.case_id.to_string() };
            crate::dao::jobs::seed(tx, &job, now).await.map_err(db_err)?;
        }
        Ok(())
    }), |value, _: Outbox| async move { scheduler::wake(); Ok(value) }).await
}

pub(crate) fn handle(db: Db, world: Arc<World>, job: Job) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send>> {
    Box::pin(async move {
        let case_id: i64 = serde_json::from_str(&job.payload)?;
        let _market = market::lock().await;
        let sde = Arc::clone(world.sde());
        db.transaction(|tx, outbox: &mut Outbox| {
            let sde = Arc::clone(&sde);
            Box::pin(async move {
                let Some(case) = vote_dao::case(tx, case_id).await.map_err(db_err)? else { return Ok(()) };
                if !registry::lock_admission(tx, &[], &[case.corporation_id], &[]).await.map_err(db_err)? {
                    return Err(CallError::Internal(anyhow::anyhow!("corporation association changed during sanction expiry")));
                }
                let Some(case) = vote_dao::lock_case(tx, case_id).await.map_err(db_err)? else { return Ok(()) };
                let Some(action) = dao::lock(tx, case_id).await.map_err(db_err)? else { return Ok(()) };
                if action.acted_upon || action.expired_at.is_some() { return Ok(()); }
                let now = filetime_now();
                if action.expires > now {
                    return Err(CallError::Internal(anyhow::anyhow!("sanction expiry invoked before its deadline")));
                }
                if dao::mark_expired(tx, case_id, now).await.map_err(db_err)? {
                    release_reservation(tx, sde.as_ref(), &case).await?;
                    votes::notify(tx, outbox, case.corporation_id, case_id, "OnSanctionedActionChanged").await?;
                }
                Ok(())
            })
        }, |value, outbox| async move { dispatch_world(value, outbox, &world) }).await.map_err(|error| match error {
            CallError::Internal(error) => error,
            CallError::User { msg, .. } => anyhow::anyhow!("sanction expiry refused: {msg}"),
        })
    })
}
