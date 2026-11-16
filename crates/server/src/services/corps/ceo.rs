//! CEO election succession mechanics shared by corporation vote processing.
//! Mirrors the leadership part of voting.py: the handover body extracted from the
//! voluntary ResignFromCEO call performs transfer_ceo over game.corporations and
//! game.characters, pending-kick cleanup, audit, session role projection, the
//! persistent CEO notice and the ceoID corporation change. Candidate eligibility
//! combines live membership with the effective Corporation Management projection.
//! Callers own admission locks, the election pointer, vote-case guards, share
//! thresholds and the voluntary path's docking and active-CEO gates.
use super::*;
use crate::dao::corporations::roles as role_dao;

/// Whether `candidate` may take over `corporation` as CEO: a live, unblocked member who
/// is not the sitting CEO and reaches the configured effective Corporation Management
/// level at `now`. The caller composes the personal share threshold and the absence of
/// an active election; the batch potential_ceos listing keeps its own query.
pub(super) async fn eligible(ex: &(impl Executor + ?Sized), sde: &Sde, corporation: i64, candidate: i64, now: i64) -> Result<bool, CallError> {
    let Some(authority) = permissions::load(ex, candidate).await? else { return Ok(false) };
    if !authority.live || authority.corporation != corporation || authority.blocked || authority.ceo == Some(candidate) {
        return Ok(false);
    }
    let snapshot = crate::services::skills::effective::snapshot_at(ex, sde, candidate, now).await?;
    Ok(i64::from(snapshot.level(SKILL_CORPORATION_MANAGEMENT)) >= crate::config::game().corporations.min_management_skill_level)
}

/// Move `corporation` from `old` to `next` inside the caller's admission-locked
/// transaction. The caller cleared the election pointer first, so `old`'s before mask is
/// the real stored authority and the election's role freeze does not falsify the audit
/// delta. The guarded transfer_ceo refuses a stale sitting CEO; everything else reuses
/// the voluntary succession: sovereignty invalidation, pending-kick cleanup, issued-intent
/// cancellation when the outgoing CEO loses Director, audit, session projection, the
/// persistent new-CEO notice and the real ceoID corporation change.
pub(super) async fn handover_in<X: Send>(
    ex: &(impl Executor + ?Sized), sde: &Sde, outbox: &mut Outbox<X>,
    corporation: i64, old: i64, next: i64, actor: i64, now: i64,
) -> Result<(), CallError> {
    let before_old = permissions::load(ex, old).await?.ok_or_else(|| notify_error(messages::MEMBER_CANNOT_BECOME_CEO))?;
    let before_next = permissions::load(ex, next).await?.ok_or_else(|| notify_error(messages::MEMBER_CANNOT_BECOME_CEO))?;
    if before_next.blocked { return Err(notify_error(messages::MEMBER_CANNOT_BECOME_CEO)); }
    if !role_dao::transfer_ceo(ex, corporation, old, next, sde.corporation_roles.scope_masks()).await.map_err(db_err)? {
        return Err(notify_error(messages::MEMBER_CANNOT_BECOME_CEO));
    }
    crate::services::sovereignty::invalidate_sources(ex, outbox, &[old, next]).await?;
    role_dao::removal_clock(ex, next, None).await.map_err(db_err)?;
    if let Some(pending) = crate::dao::corporations::pending_kicks::get(ex, corporation, next).await.map_err(db_err)? {
        crate::dao::corporations::pending_kicks::delete(ex, corporation, next).await.map_err(db_err)?;
        pending_kicks::cancel(ex, &pending).await.map_err(db_err)?;
    }
    if !permissions::has(ex, old, corporation, ROLE_DIRECTOR).await? {
        pending_kicks::cancel_issued(ex, corporation, old).await.map_err(db_err)?;
    }
    audit::event(ex, old, actor, corporation, audit::CEASED_CEO, now).await?;
    audit::event(ex, next, actor, corporation, audit::BECAME_CEO, now).await?;
    for (char_id, before) in [(old, before_old), (next, before_next)] {
        let after = permissions::load(ex, char_id).await?.ok_or_else(|| notify_error(messages::MEMBER_CANNOT_BECOME_CEO))?;
        for scope in 0..4 {
            crate::dao::corporations::audit::role(ex, char_id, corporation, actor, now, before.roles[scope], after.roles[scope], false, scope as i16).await.map_err(db_err)?;
        }
        super::notifications::role_projection(outbox, char_id, after.roles);
    }
    let audience = crate::dao::characters::members::active_ids(ex, corporation).await.map_err(db_err)?;
    notifications::ceo(ex, outbox, &audience, corporation).await?;
    let change = Value::dict([("ceoID", Value::tuple([Value::Int(old), Value::Int(next)]))]);
    for recipient in &audience {
        outbox.notify_pilot(*recipient, "OnCorporationChanged", vec![Value::Int(corporation), change.clone()]);
    }
    Ok(())
}

/// Re-read `character`'s projected authority after the caller's election freeze or
/// unfreeze write in `corporation`, queue the real session role values and invalidate
/// sovereignty sources. A member who no longer effectively holds Director loses their
/// issued kick intents under the existing cancellation rule. No corporation change is
/// sent: the ceoID did not move.
pub(super) async fn role_projection_changed_in<X: Send>(
    ex: &(impl Executor + ?Sized), outbox: &mut Outbox<X>, corporation: i64, character: i64,
) -> Result<(), CallError> {
    let Some(authority) = permissions::load(ex, character).await? else { return Ok(()) };
    if authority.live && authority.corporation == corporation {
        super::notifications::role_projection(outbox, character, authority.roles);
        if !permissions::allows(&authority, corporation, ROLE_DIRECTOR) {
            pending_kicks::cancel_issued(ex, corporation, character).await.map_err(db_err)?;
        }
    }
    crate::services::sovereignty::invalidate_sources(ex, outbox, &[character]).await?;
    Ok(())
}
