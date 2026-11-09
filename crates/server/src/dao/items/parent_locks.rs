//! Orders locks on existing `game.items` rows for inventory transactions.
//! This DAO has no direct client mirror; callers supply classification and policy.
//! It does not discover ancestors or open transactions.

use crate::db::{self, executor::Executor};
use std::{collections::{BTreeMap, BTreeSet}, marker::PhantomData};
use tokio_postgres::Row;

#[derive(Clone)]
pub(crate) struct Parent {
    pub item_id: i64,
    pub type_id: i32,
    pub owner_id: i64,
    pub location_id: i64,
    pub flag_id: i32,
    pub quantity: i64,
    pub singleton: bool,
    pub jump_clone_id: Option<i64>,
}

impl Parent {
    pub(super) fn from_row(row: &Row) -> Self {
        Self {
            item_id: row.get("item_id"),
            type_id: row.get("type_id"),
            owner_id: row.get("owner_id"),
            location_id: row.get("location_id"),
            flag_id: row.get("flag_id"),
            quantity: row.get("quantity"),
            singleton: row.get("singleton"),
            jump_clone_id: row.get("jump_clone_id"),
        }
    }
}

/// Capture unrestricted item metadata without acquiring locks.
pub(crate) async fn capture(ex: &(impl Executor + ?Sized), ids: &[i64]) -> Result<Vec<Parent>, db::Error> {
    let rows = ex.query(
        "SELECT item_id, type_id, owner_id, location_id, flag_id, quantity, singleton, jump_clone_id FROM game.items WHERE item_id = ANY($1) ORDER BY item_id",
        &[&ids],
    ).await?;
    Ok(rows.iter().map(Parent::from_row).collect())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub(crate) enum LockStrength { Share, Update }

#[derive(Clone, Copy, Debug)]
pub(crate) struct LockRequest {
    pub item_id: i64,
    pub strength: LockStrength,
}

pub(crate) struct LockedItems<'tx> {
    inline_hull: Option<Parent>,
    rows: Vec<Parent>,
    strengths: BTreeMap<i64, LockStrength>,
    _transaction: PhantomData<&'tx ()>,
}

impl LockedItems<'_> {
    pub(crate) fn rows(&self) -> &[Parent] {
        self.inline_hull.as_ref().map(std::slice::from_ref).unwrap_or(&self.rows)
    }
    pub(crate) fn row(&self, id: i64) -> Option<&Parent> {
        let rows = self.rows();
        rows.binary_search_by_key(&id, |row| row.item_id).ok().map(|index| &rows[index])
    }
    /// Whether the final requested strength is UPDATE, even if the row was missing.
    pub(crate) fn has_update_intent(&self, id: i64) -> bool {
        self.inline_hull.as_ref().is_some_and(|row| row.item_id == id) || self.strengths.get(&id) == Some(&LockStrength::Update)
    }
    pub(crate) fn is_write_locked(&self, id: i64) -> bool {
        self.row(id).is_some() && self.has_update_intent(id)
    }
}

impl<'tx> LockedItems<'tx> {
    /// The hull was loaded after its UPDATE lock; ordinary loads exclude clone rows.
    pub(crate) fn held_hull(_ex: &'tx (impl Executor + ?Sized), hull: &super::loading::GameItem) -> Self {
        Self {
            inline_hull: Some(Parent { item_id: hull.item_id, type_id: hull.type_id, owner_id: hull.owner_id,
                location_id: hull.location_id, flag_id: hull.flag_id, quantity: hull.quantity,
                singleton: hull.singleton, jump_clone_id: None }),
            rows: Vec::new(), strengths: BTreeMap::new(), _transaction: PhantomData,
        }
    }
}

/// Lock each requested row once, at its final strength, hulls before other rows.
/// The caller must classify and revalidate metadata and closure within its transaction.
pub(crate) async fn lock<'tx>(ex: &'tx (impl Executor + ?Sized), hull_ids: &[i64], requests: &[LockRequest]) -> Result<LockedItems<'tx>, db::Error> {
    let mut strengths = BTreeMap::new();
    for request in requests {
        strengths.entry(request.item_id).and_modify(|strength: &mut LockStrength| *strength = (*strength).max(request.strength)).or_insert(request.strength);
    }
    let hulls: BTreeSet<_> = hull_ids.iter().copied().collect();
    if hulls.iter().any(|id| !strengths.contains_key(id)) {
        return Err(db::Error::InvalidItemLockRequest("hull classification is not a subset of lock requests"));
    }
    let mut rows = Vec::with_capacity(strengths.len());
    for id in hulls.iter().copied().chain(strengths.keys().copied().filter(|id| !hulls.contains(id))) {
        let sql = match strengths[&id] {
            LockStrength::Share => "SELECT item_id, type_id, owner_id, location_id, flag_id, quantity, singleton, jump_clone_id FROM game.items WHERE item_id = $1 FOR SHARE",
            LockStrength::Update => "SELECT item_id, type_id, owner_id, location_id, flag_id, quantity, singleton, jump_clone_id FROM game.items WHERE item_id = $1 FOR UPDATE",
        };
        if let Some(row) = ex.query_opt(sql, &[&id]).await? { rows.push(Parent::from_row(&row)); }
    }
    rows.sort_unstable_by_key(|row| row.item_id);
    Ok(LockedItems { inline_hull: None, rows, strengths, _transaction: PhantomData })
}

#[cfg(test)] pub(crate) mod tests;
