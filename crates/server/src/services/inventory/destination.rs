//! Trusted inventory destination values and custody mode; callers must obtain fresh
//! authorization separately for client-selected endpoints. No access is granted here.

/// Item ownership may differ from the actual parent owner used for capacity and custody.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Destination {
    pub item_owner: i64,
    pub capacity_owner: i64,
    pub location: i64,
    pub flag: i32,
}

impl Destination {
    pub(crate) fn owned(owner: i64, location: i64, flag: i32) -> Self {
        Self { item_owner: owner, capacity_owner: owner, location, flag }
    }
}

/// A whole take acquires all descendants; ordinary deposits preserve custody.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::services) enum Custody { Preserve, TakeSubtree }

use crate::dao::items::{deliveries::Ancestor, loading::Item, parent_locks::{self, LockRequest, LockStrength, LockedItems, Parent}};
use crate::db::executor::Executor;
use crate::sde::Sde;
use crate::services::{db_err, notify_error, CallError};

fn same_row(a: &Parent, b: &Parent) -> bool {
    a.item_id == b.item_id && a.type_id == b.type_id && a.owner_id == b.owner_id
        && a.location_id == b.location_id && a.flag_id == b.flag_id
        && a.quantity == b.quantity && a.singleton == b.singleton && a.jump_clone_id == b.jump_clone_id
}

fn is_hull(sde: &Sde, type_id: i32) -> Result<bool, CallError> {
    let ty = sde.types.get(type_id).ok_or_else(|| notify_error(infinity_core::messages::ITEM_NOT_AT_ORIGINAL_LOCATION))?;
    Ok(ty.category_id == Some(infinity_core::consts::CATEGORY_SHIP))
}

pub(crate) fn parent_requests(
    sde: &Sde, parents: impl IntoIterator<Item = (i64, i32)>, hull_strength: LockStrength, nonhull_strength: LockStrength,
) -> Result<Vec<LockRequest>, CallError> {
    parents.into_iter().map(|(item_id, type_id)| Ok(LockRequest {
        item_id,
        strength: if is_hull(sde, type_id)? { hull_strength } else { nonhull_strength },
    })).collect()
}

/// Capture and classify the complete requested set before acquiring any item row.
pub(crate) async fn lock_items<'tx>(
    ex: &'tx (impl Executor + ?Sized), sde: &Sde, requests: &[LockRequest], optional_candidates: &[i64],
) -> Result<LockedItems<'tx>, CallError> {
    let refuse = || notify_error(infinity_core::messages::ITEM_NOT_AT_ORIGINAL_LOCATION);
    let mut ids: Vec<_> = requests.iter().map(|request| request.item_id).collect();
    ids.sort_unstable();
    ids.dedup();
    let captured = parent_locks::capture(ex, &ids).await.map_err(db_err)?;
    if optional_candidates.iter().any(|id| ids.binary_search(id).is_err())
        || ids.iter().any(|id| !optional_candidates.contains(id) && !captured.iter().any(|row| row.item_id == *id)) {
        return Err(refuse());
    }
    let mut hulls = Vec::new();
    for row in &captured {
        if is_hull(sde, row.type_id)? { hulls.push(row.item_id); }
    }
    let locks = parent_locks::lock(ex, &hulls, requests).await.map_err(db_err)?;
    if locks.rows().iter().any(|row| !captured.iter().any(|old| old.item_id == row.item_id))
        || captured.iter().any(|old| match locks.row(old.item_id) {
            None => !optional_candidates.contains(&old.item_id),
            Some(row) => row.type_id != old.type_id
                || is_hull(sde, row.type_id).ok() != Some(hulls.contains(&row.item_id))
                || (!optional_candidates.contains(&old.item_id) && !same_row(old, row)),
        }) { return Err(refuse()); }
    Ok(locks)
}

#[derive(Clone)]
pub(in crate::services) struct ParentSnapshot {
    pub item: Item,
    pub ancestry: Vec<Ancestor>,
}

/// A revalidated view of parents held by the caller's item-lock token.
pub(crate) struct ParentGuard<'locks, 'tx> {
    parents: Vec<i64>,
    locks: &'locks LockedItems<'tx>,
}

impl<'locks, 'tx> ParentGuard<'locks, 'tx> {
    pub(in crate::services) fn revalidate(
        locks: &'locks LockedItems<'tx>, snapshots: &[ParentSnapshot],
    ) -> Result<Self, CallError> {
        let refuse = || notify_error(infinity_core::messages::ITEM_NOT_AT_ORIGINAL_LOCATION);
        let mut parents = Vec::with_capacity(snapshots.len());
        for snapshot in snapshots {
            let item = &snapshot.item;
            if item.category_id == i64::from(infinity_core::consts::CATEGORY_SHIP) { return Err(refuse()); }
            let row = locks.row(item.item_id).ok_or_else(refuse)?;
            if row.jump_clone_id.is_some() || row.type_id != item.type_id || row.owner_id != item.owner_id
                || row.location_id != item.location_id || row.flag_id != item.flag_id
                || row.quantity != item.quantity || row.singleton != item.singleton
                || snapshot.ancestry.iter().any(|old| !locks.row(old.item_id).is_some_and(|row|
                    row.jump_clone_id.is_none() && row.type_id == old.type_id && row.owner_id == old.owner_id
                        && row.location_id == old.location_id && row.flag_id == old.flag_id
                        && row.quantity == old.quantity && row.singleton == old.singleton)) {
                return Err(refuse());
            }
            parents.push(item.item_id);
        }
        Ok(Self { parents, locks })
    }

    pub(in crate::services) fn contains_parent(&self, id: i64) -> bool { self.parents.contains(&id) }
    pub(in crate::services) fn uses_locks(&self, locks: &LockedItems<'_>) -> bool { std::ptr::eq(self.locks, locks) }
    pub(in crate::services) fn rows(&self) -> &[Parent] { self.locks.rows() }
}
