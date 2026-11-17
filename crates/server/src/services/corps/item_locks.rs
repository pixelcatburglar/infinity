//! Shared corporation blueprint-lock boundary for vote lockdowns and unlocks.
//! `admit_target` proves one original's immutable office custody under the caller's
//! item locks; `apply_in` enacts or rescinds the lock inside the caller's
//! transaction. `ensure_mutable` is the single guard every inventory mutation calls
//! for its blueprint candidates so an enacted lock or a pending case reservation
//! freezes the item in place. Neither function opens a transaction or takes the
//! market lock: callers own both, plus organization and case admission.

use super::*;
use infinity_core::consts::{CATEGORY_BLUEPRINT, CORP_HANGAR_DIVISIONS, FLAG_CORP_SAG1, SINGLETON_QUANTITY};
use crate::dao::items::{corporation_locks, loading::ItemQuery, parent_locks::LockedItems};
use infinity_core::messages::corporations as text;
use crate::services::inventory::destination;
use crate::services::items::load_items;
use super::vote_request::{VOTE_LOCKDOWN, VOTE_UNLOCK};
/// One lock target as a vote case's parameters record it: the blueprint original,
/// its type and the NPC station holding the corporation's office.
#[derive(Clone, Copy)]
pub(crate) struct Target {
    pub item_id: i64,
    pub type_id: i32,
    pub station_id: i64,
}

/// Whether the caller enacts the lock or removes a matching existing one.
#[derive(Clone, Copy)]
pub(crate) enum Action { Lock, Unlock }

fn ineligible() -> CallError { notify_error(text::ITEM_LOCK_TARGET_INVALID) }
fn blocked_item() -> CallError { notify_error(text::ITEM_LOCKED_OR_RESERVED) }
/// The vote type an executing case must carry for `action`.
fn expected_vote_type(action: Action) -> i64 {
    match action { Action::Lock => VOTE_LOCKDOWN, Action::Unlock => VOTE_UNLOCK }
}

/// Blueprint-category members of `ids` per the caller's lock token. Every candidate
/// must be present in `locks`; a missing row means the caller's lock set did not
/// cover the mutation it intends, which is refusal-grade staleness.
pub(in crate::services) fn locked_blueprints(sde: &Sde, locks: &LockedItems<'_>, ids: &[i64]) -> Result<Vec<i64>, CallError> {
    let mut blueprints = Vec::new();
    for id in ids {
        let Some(row) = locks.row(*id) else {
            return Err(notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION));
        };
        if sde.types.get(row.type_id).and_then(|ty| ty.category_id) == Some(CATEGORY_BLUEPRINT) {
            blueprints.push(*id);
        }
    }
    Ok(blueprints)
}

/// Refuse when any candidate blueprint is under an enacted lock or a pending
/// vote-case reservation. The caller already holds the complete affected item set
/// locked at final strength and revalidated; empty candidate sets never query.
pub(crate) async fn ensure_mutable(ex: &(impl Executor + ?Sized), blueprint_ids: &[i64]) -> Result<(), CallError> {
    if blueprint_ids.is_empty() { return Ok(()); }
    if corporation_locks::blocked(ex, blueprint_ids).await.map_err(db_err)? {
        return Err(blocked_item());
    }
    Ok(())
}

/// Prove `target` is a lockable corporation blueprint original under item locks.
/// The caller already holds its organization and case locks; this captures the
/// item and its complete ancestry before the first row lock, takes every row once
/// at final strength, then revalidates the immutable original, its office custody
/// (an inactive lease is impounded custody, which still admits), busy jobs, trade
/// staging, and conflicting locks or reservations. `own_case` exempts only that
/// case's own pending reservation, and only after the case row proves it is this
/// corporation's matching lock or unlock case reserving this exact target; case
/// creation passes `None`.
pub(crate) async fn admit_target<'tx>(
    ex: &'tx (impl Executor + ?Sized),
    sde: &Sde,
    corporation: i64,
    target: Target,
    action: Action,
    own_case: Option<i64>,
    now: i64,
) -> Result<LockedItems<'tx>, CallError> {
    let Some(ty) = sde.types.get(target.type_id) else { return Err(ineligible()) };
    // Only SDE blueprint originals qualify; reaction formulas are outside the
    // supported industry data, and copies never reach this point.
    if ty.category_id != Some(CATEGORY_BLUEPRINT) || sde.map.station(target.station_id).is_none()
        || sde.industry.activity(target.type_id, crate::sde::industry::MANUFACTURING).is_none() {
        return Err(ineligible());
    }
    let Some(item) = load_items(ex, sde, ItemQuery::Id(target.item_id)).await.map_err(db_err)?.into_iter().next()
        else { return Err(ineligible()) };
    let ancestry = crate::dao::items::deliveries::ancestry_many(ex, &[target.item_id]).await.map_err(db_err)?;
    let requests = destination::custody_requests(sde, &[target.item_id], &ancestry)?;
    let locks = destination::lock_items(ex, sde, &requests, &[]).await?;
    if crate::dao::items::deliveries::ancestry_many(ex, &[target.item_id]).await.map_err(db_err)? != ancestry {
        return Err(ineligible());
    }
    let Some(row) = locks.row(target.item_id) else { return Err(ineligible()) };
    if row.jump_clone_id.is_some()
        || row.type_id != item.type_id || row.type_id != target.type_id || row.owner_id != corporation
        || row.location_id != item.location_id || row.flag_id != item.flag_id
        || !row.singleton || row.quantity != SINGLETON_QUANTITY {
        return Err(ineligible());
    }
    let Some(office_row) = ancestry.iter().find(|row| row.office.is_some()) else { return Err(ineligible()) };
    let office = office_row.office.as_ref().unwrap();
    if office.corporation_id != corporation || office.station_id != target.station_id
        || office_row.ancestor.location_id != target.station_id {
        return Err(ineligible());
    }
    // An active office admits only originals sitting directly in one of its hangar
    // divisions; an inactive lease is impounded custody, where the classifier keeps
    // the office's direct child carrying a division flag.
    let division = if office.is_active(now) {
        if row.location_id != office_row.ancestor.item_id { return Err(ineligible()); }
        row.flag_id
    } else {
        ancestry.iter().find(|child| child.ancestor.location_id == office_row.ancestor.item_id)
            .map(|child| child.ancestor.flag_id).unwrap_or(office_row.ancestor.flag_id)
    };
    if !(FLAG_CORP_SAG1..FLAG_CORP_SAG1 + CORP_HANGAR_DIVISIONS).contains(&division) {
        return Err(ineligible());
    }
    if crate::dao::industry::has_active_blueprint_job(ex, &[target.item_id]).await.map_err(db_err)?
        || !crate::dao::trades::reserved(ex, &[target.item_id]).await.map_err(db_err)?.is_empty() {
        return Err(ineligible());
    }
    match action {
        Action::Lock => {
            if corporation_locks::get(ex, target.item_id).await.map_err(db_err)?.is_some() {
                return Err(blocked_item());
            }
        }
        Action::Unlock => {
            if corporation_locks::get(ex, target.item_id).await.map_err(db_err)?.map(|lock| lock.corporation_id) != Some(corporation) {
                return Err(blocked_item());
            }
        }
    }
    if let Some(own_case) = own_case {
        // The reservation exemption is only as strong as the case identity: the
        // executing case must be this corporation's matching lock or unlock case
        // that reserved exactly this target at creation.
        let case = crate::dao::corporations::votes::case(ex, own_case).await.map_err(db_err)?
            .ok_or_else(ineligible)?;
        if case.corporation_id != corporation || case.vote_type != expected_vote_type(action)
            || case.reserved_item_id != Some(target.item_id) {
            return Err(ineligible());
        }
    }
    if corporation_locks::reserved(ex, &[target.item_id], own_case).await.map_err(db_err)? {
        return Err(blocked_item());
    }
    Ok(locks)
}

/// Enact or rescind the lock inside the caller's transaction: admit the target,
/// write the fact, rescind the old lockdown's sanctioned action on unlock, release
/// the caller's own case reservation, and queue OnLockedItemChange for the
/// corporation's current members. `case_id` is the vote case executing the action;
/// on unlock the removed lock identifies the older case whose action rescinds.
pub(crate) async fn apply_in<X: Send>(
    ex: &(impl Executor + ?Sized),
    sde: &Sde,
    outbox: &mut Outbox<X>,
    corporation: i64,
    case_id: i64,
    target: Target,
    action: Action,
    now: i64,
) -> Result<(), CallError> {
    let _locks = admit_target(ex, sde, corporation, target, action, Some(case_id), now).await?;
    let locked = matches!(action, Action::Lock);
    match action {
        Action::Lock => {
            if !corporation_locks::insert(ex, &corporation_locks::LockRow {
                item_id: target.item_id, corporation_id: corporation,
                station_id: target.station_id, case_id, locked_at: now,
            }).await.map_err(db_err)? {
                return Err(blocked_item());
            }
        }
        Action::Unlock => {
            let Some(lock) = corporation_locks::get(ex, target.item_id).await.map_err(db_err)? else {
                return Err(blocked_item());
            };
            if corporation_locks::remove(ex, target.item_id, corporation, lock.case_id).await.map_err(db_err)?.is_none() {
                return Err(blocked_item());
            }
            // The old lockdown action stays in history but is no longer in effect.
            if !crate::dao::corporations::sanctions::rescind(ex, lock.case_id, now).await.map_err(db_err)? {
                return Err(blocked_item());
            }
            super::votes::notify(ex, outbox, corporation, lock.case_id, "OnSanctionedActionChanged").await?;
        }
    }
    crate::dao::corporations::votes::release_reservation(ex, case_id, target.item_id).await.map_err(db_err)?;
    for member in crate::dao::characters::members::active_ids(ex, corporation).await.map_err(db_err)? {
        outbox.notify_pilot(member, "OnLockedItemChange",
            vec![Value::Int(target.item_id), Value::Int(corporation), Value::Int(target.station_id), Value::Bool(locked)]);
    }
    Ok(())
}
