//! Ship hold and ordinary container capacity for invCache inventories.
//! Reads game.items and reservations, and fitted dogma for ship holds.
//! Shared-bay permissions and in-space maintenance operations are handled elsewhere.
//! A hull's ordinary cargo may carry the persisted unlimited mode; every other
//! hold, container and destination stays a real finite capacity.

use std::sync::Arc;
use std::collections::BTreeMap;
use infinity_core::consts::{dogma::ATTR_CAPACITY, CARGO_CONTAINER_GROUPS, CATEGORY_SHIP, FLAG_CARGO, FLAG_FRIGATE_ESCAPE_BAY};
use crate::dao::items::{loading::{self, GameItem, ItemQuery}, pending_reloads::{self, PendingReload}, ship};
use crate::db::executor::Executor;
use crate::sde::Sde;
use crate::services::{db_err, item_volume::{item_volume, stack_size}, fit::{Fit, Key}, CallError};
use crate::dao::items::dogma::OverrideState;
use super::holds::{self, CapacityLimit};

pub(crate) struct Addition {
    pub(crate) type_id: i32,
    pub(crate) singleton: bool,
    pub(crate) quantity: i64,
}

#[derive(Copy, Clone, Default)]
pub(crate) struct Replaced<'a> {
    pub(crate) items: &'a [i64],
    pub(crate) reloads: &'a [i64],
}

pub(crate) fn is_ordinary_container(sde: &Sde, type_id: i32) -> bool {
    sde.types.get(type_id).and_then(|ty| ty.group_id).is_some_and(|group| CARGO_CONTAINER_GROUPS.contains(&group))
}

pub(crate) fn static_capacity(sde: &Sde, type_id: i32) -> Result<f64, CallError> {
    let ty = sde.types.get(type_id).ok_or_else(|| anyhow::anyhow!("missing SDE type {type_id}"))?;
    Ok(ty.attribute(ATTR_CAPACITY).or(ty.capacity).unwrap_or(0.0))
}

pub(crate) fn container_room(sde: &Sde, parent: &GameItem, items: &[GameItem], reloads: &[PendingReload]) -> Result<Option<Room>, CallError> {
    if !is_ordinary_container(sde, parent.type_id) { return Ok(None); }
    let capacity = static_capacity(sde, parent.type_id)?;
    if !capacity.is_finite() || capacity < 0.0 {
        return Err(crate::services::user_error(infinity_core::messages::MSG_CANNOT_STORE_DESTINATION_RESTRICTED, vec![]));
    }
    let used = total_volume(sde, FLAG_CARGO,
        items.iter().filter(|item| item.location_id == parent.item_id)
            .map(|item| (item.type_id, item.singleton, item.quantity))
            .chain(reloads.iter().filter(|row| row.source_location_id == parent.item_id)
                .map(|row| (row.type_id, false, row.quantity))) )?;
    Ok(Some(Room { capacity: CapacityLimit::Finite(capacity), used }))
}

pub(crate) async fn effective(ex: &(impl Executor + ?Sized), sde: &Arc<Sde>, owner: i64, ship: i64, flag: i32) -> Result<CapacityLimit, CallError> {
    let fit = if crate::dao::characters::selection::ship_id(ex, owner).await.map_err(db_err)? == Some(ship) {
        Fit::load(ex, sde.clone(), owner, ship).await?
    } else {
        Fit::load_unpiloted(ex, sde.clone(), ship).await?
    };
    Ok(holds::effective_capacity(&fit, flag))
}

fn volume(sde: &Sde, flag: i32, type_id: i32, singleton: bool, quantity: i64) -> Result<f64, CallError> {
    let ty = sde.types.get(type_id).ok_or_else(|| anyhow::anyhow!("missing SDE type {type_id}"))?;
    if ty.volume.is_none() { return Err(anyhow::anyhow!("missing SDE volume for type {type_id}").into()); }
    let value = if flag == FLAG_FRIGATE_ESCAPE_BAY { stack_size(singleton, quantity) as f64 }
        else { item_volume(sde, type_id, singleton, quantity, None)? };
    if !value.is_finite() || value < 0.0 { return Err(anyhow::anyhow!("invalid volume for type {type_id}").into()); }
    Ok(value)
}

fn total_volume(sde: &Sde, flag: i32, rows: impl Iterator<Item = (i32, bool, i64)>) -> Result<f64, CallError> {
    let mut stacks = BTreeMap::<i32, i128>::new();
    let mut total = 0.0;
    for (type_id, singleton, quantity) in rows {
        // Negative quantities encode one item's own volume, not a stack decrement.
        if singleton || quantity < 0 {
            total += volume(sde, flag, type_id, singleton, quantity)?;
        } else {
            let entry = stacks.entry(type_id).or_default();
            *entry = entry.checked_add(i128::from(quantity)).ok_or_else(|| crate::services::user_error(infinity_core::messages::MSG_CANNOT_STORE_DESTINATION_RESTRICTED, vec![]))?;
        }
    }
    for (type_id, quantity) in stacks {
        let value = volume(sde, flag, type_id, false, 1)? * quantity as f64;
        if !value.is_finite() || value < 0.0 { return Err(anyhow::anyhow!("invalid volume for type {type_id}").into()); }
        total += value;
    }
    if !total.is_finite() {
        return Err(crate::services::user_error(infinity_core::messages::MSG_CANNOT_STORE_DESTINATION_RESTRICTED, vec![]));
    }
    Ok(total)
}
/// A ship hold's admission limit with the persisted override state observed at
/// capture time. Fields stay private: snapshots come only from a real ship
/// `Fit`, never from an arbitrary caller-supplied number.
#[derive(Clone, Copy, Debug)]
pub(crate) struct HoldCapacity {
    ship: i64,
    flag: i32,
    attribute: i32,
    limit: CapacityLimit,
    observed: Option<OverrideState>,
}

impl HoldCapacity {
    /// Bind one hold's limit and persisted override state from an in-hand fit.
    /// Special bays always capture a finite limit; the cargo mode only applies
    /// to the ordinary cargo flag of a real ship.
    pub(crate) fn capture(fit: &Fit, flag: i32) -> Result<Self, CallError> {
        let ship = fit.ship_id().ok_or_else(|| crate::services::notify_error(infinity_core::messages::ITEM_NOT_AT_ORIGINAL_LOCATION))?;
        let hold = holds::descriptor(flag)
            .ok_or_else(|| crate::services::user_error(infinity_core::messages::MSG_CANNOT_STORE_DESTINATION_RESTRICTED, vec![]))?;
        Ok(Self { ship, flag, attribute: hold.attribute, limit: holds::effective_capacity(fit, flag), observed: fit.override_state(Key::Ship, hold.attribute) })
    }

    pub(crate) fn ship(&self) -> i64 { self.ship }
    pub(crate) fn flag(&self) -> i32 { self.flag }

    /// Whether the persisted row still matches the state seen at capture.
    fn matches(&self, hull: &GameItem, state: Option<OverrideState>) -> bool {
        self.ship == hull.item_id && self.observed == state
    }
}

/// Where a destination check reads the hull's capacity: re-evaluated from the
/// current database under the hull lock, or pre-validated against a snapshot
/// taken earlier from a real ship fit.
#[derive(Clone, Copy, Debug)]
pub(crate) enum CapacitySource {
    Current,
    Captured(HoldCapacity),
}

pub(crate) struct Room { pub(crate) capacity: CapacityLimit, pub(crate) used: f64 }

impl Room {
    pub(crate) fn units(&self, sde: &Sde, flag: i32, type_id: i32, singleton: bool, requested: i64) -> Result<i64, CallError> {
        let refuse = || crate::services::user_error(infinity_core::messages::MSG_CANNOT_STORE_DESTINATION_RESTRICTED, vec![]);
        if !self.used.is_finite() || self.used < 0.0 { return Err(refuse()); }
        let capacity = match self.capacity {
            CapacityLimit::Unlimited => {
                // Quota is waived only after the same unit and occupancy
                // validation a finite hold performs; the post-write volume
                // must stay finite as well.
                let unit = volume(sde, flag, type_id, singleton, 1)?;
                holds::check(sde, flag, type_id, singleton, requested.max(1), self.capacity, self.used)?;
                if requested <= 0 { return Ok(0); }
                if !(self.used + unit * requested as f64).is_finite() { return Err(refuse()); }
                return Ok(requested);
            }
            CapacityLimit::Finite(value) => value,
        };
        if !capacity.is_finite() || capacity < 0.0 { return Err(refuse()); }
        let unit = volume(sde, flag, type_id, singleton, 1)?;
        let admission_capacity = if capacity > 0.0 { capacity.max(unit) } else { capacity };
        holds::check(sde, flag, type_id, singleton, 1, CapacityLimit::Finite(admission_capacity), 0.0)?;
        if requested <= 0 { return Ok(0); }
        if unit <= 0.0 { return Ok(requested); }
        Ok((((capacity - self.used).max(0.0) / unit).floor() as i64).min(requested))
    }
}

async fn used(ex: &(impl Executor + ?Sized), sde: &Sde, ship: i64, flag: i32, items: &[GameItem]) -> Result<f64, CallError> {
    let reloads = pending_reloads::for_source(ex, ship).await.map_err(db_err)?;
    total_volume(sde, flag, items.iter().map(|item| (item.type_id, item.singleton, item.quantity))
        .chain(reloads.iter().filter(|row| row.source_flag == flag)
            .map(|row| (row.type_id, false, row.quantity))))
}

fn ship_category(sde: &Sde, hull: &GameItem) -> Result<bool, CallError> {
    let ty = sde.types.get(hull.type_id).ok_or_else(|| anyhow::anyhow!("missing SDE type {}", hull.type_id))?;
    let category = ty.category_id.ok_or_else(|| anyhow::anyhow!("missing SDE category for type {}", hull.type_id))?;
    Ok(category == CATEGORY_SHIP)
}

fn validate_hull(sde: &Sde, hull: &GameItem, owner: i64) -> Result<(), CallError> {
    if !ship_category(sde, hull)? || hull.owner_id != owner || !hull.singleton || hull.quantity != 1 {
        return Err(crate::services::notify_error(infinity_core::messages::ITEM_NOT_AT_ORIGINAL_LOCATION));
    }
    Ok(())
}

/// The current room of a ship hold bound by an earlier snapshot. The hull lock
/// must be acquired by this function; the snapshot's ship, flag, attribute and
/// observed override state are re-verified against the joined witness row
/// before the captured limit is trusted.
pub(crate) async fn room(ex: &(impl Executor + ?Sized), sde: &Sde, owner: i64, snapshot: &HoldCapacity) -> Result<Room, CallError> {
    let refuse = || crate::services::notify_error(infinity_core::messages::ITEM_NOT_AT_ORIGINAL_LOCATION);
    if !ship::lock_reload_ship(ex, snapshot.ship).await.map_err(db_err)? { return Err(refuse()); }
    let bound = crate::dao::items::dogma::hull_override(ex, snapshot.ship, snapshot.attribute).await.map_err(db_err)?.ok_or_else(refuse)?;
    if !snapshot.matches(&bound.item, bound.state) { return Err(refuse()); }
    validate_hull(sde, &bound.item, owner)?;
    let items = loading::load_items(ex, ItemQuery::LocationFlags(snapshot.ship, &[snapshot.flag])).await.map_err(db_err)?;
    let used = used(ex, sde, snapshot.ship, snapshot.flag, &items).await?;
    Ok(Room { capacity: snapshot.limit, used })
}


pub(crate) async fn capacity<'tx>(ex: &'tx (impl Executor + ?Sized), sde: &Arc<Sde>, owner: i64, location: i64, flag: i32) -> Result<Option<(CapacityLimit, f64, crate::dao::items::parent_locks::LockedItems<'tx>)>, CallError> {
    let Some(hull) = loading::load_items(ex, ItemQuery::Id(location)).await.map_err(db_err)?.into_iter().next() else { return Ok(None) };
    if !ship_category(sde, &hull)? { return Ok(None); }
    if !ship::lock_reload_ship(ex, location).await.map_err(db_err)? {
        return Err(crate::services::notify_error(infinity_core::messages::ITEM_NOT_AT_ORIGINAL_LOCATION));
    }
    let hull = loading::load_items(ex, ItemQuery::Id(location)).await.map_err(db_err)?.into_iter().next()
        .ok_or_else(|| crate::services::notify_error(infinity_core::messages::ITEM_NOT_AT_ORIGINAL_LOCATION))?;
    validate_hull(sde, &hull, owner)?;
    let cap = effective(ex, sde, owner, location, flag).await?;
    let items = loading::load_items(ex, ItemQuery::LocationFlags(location, &[flag])).await.map_err(db_err)?;
    let used = used(ex, sde, location, flag, &items).await?;
    Ok(Some((cap, used, crate::dao::items::parent_locks::LockedItems::held_hull(ex, &hull))))
}

fn replacement_volume(sde: &Sde, flag: i32, items: &[GameItem], reloads: &[PendingReload], additions: &[Addition], replaced: Replaced<'_>) -> Result<(f64, f64), CallError> {
    let refuse = || crate::services::notify_error(infinity_core::messages::ITEM_NOT_AT_ORIGINAL_LOCATION);
    for (index, id) in replaced.items.iter().enumerate() {
        if replaced.items[..index].contains(id) || !items.iter().any(|item| item.item_id == *id) { return Err(refuse()); }
    }
    for (index, id) in replaced.reloads.iter().enumerate() {
        if replaced.reloads[..index].contains(id) || !reloads.iter().any(|row| row.reload_id == *id) { return Err(refuse()); }
    }
    if !replaced.reloads.is_empty() {
        let mut reserved = BTreeMap::<i32, i128>::new();
        let mut incoming = BTreeMap::<i32, i128>::new();
        for row in reloads.iter().filter(|row| replaced.reloads.contains(&row.reload_id)) {
            let value = reserved.entry(row.type_id).or_default();
            *value = value.checked_add(i128::from(row.quantity)).ok_or_else(refuse)?;
        }
        for item in additions {
            if item.singleton { return Err(refuse()); }
            let value = incoming.entry(item.type_id).or_default();
            *value = value.checked_add(i128::from(item.quantity)).ok_or_else(refuse)?;
        }
        if reserved.iter().any(|(type_id, quantity)| incoming.get(type_id).is_none_or(|value| value < quantity)) {
            return Err(refuse());
        }
    }
    let removed = total_volume(sde, flag,
        items.iter().filter(|item| replaced.items.contains(&item.item_id))
            .map(|item| (item.type_id, item.singleton, item.quantity))
            .chain(reloads.iter().filter(|row| replaced.reloads.contains(&row.reload_id))
                .map(|row| (row.type_id, false, row.quantity))) )?;
    let added = total_volume(sde, flag, additions.iter().map(|item| (item.type_id, item.singleton, item.quantity)))?;
    Ok((removed, added))
}

pub(crate) async fn check_destination<'tx>(ex: &'tx (impl Executor + ?Sized), sde: &Arc<Sde>, owner: i64, location: i64, flag: i32, additions: &[Addition], replaced: Replaced<'_>, source: CapacitySource) -> Result<Option<crate::dao::items::parent_locks::LockedItems<'tx>>, CallError> {
    let refuse = || crate::services::notify_error(infinity_core::messages::ITEM_NOT_AT_ORIGINAL_LOCATION);
    // A captured snapshot only applies to its own hull, flag and hold
    // attribute; any other destination, including the static/non-hull/box
    // early exits below, is refused rather than silently re-evaluated.
    let snapshot = match source {
        CapacitySource::Captured(snapshot) => {
            if snapshot.ship != location || snapshot.flag != flag
                || holds::descriptor(flag).map(|hold| hold.attribute) != Some(snapshot.attribute) {
                return Err(refuse());
            }
            Some(snapshot)
        }
        CapacitySource::Current => None,
    };
    let Some(parent) = loading::load_items(ex, ItemQuery::Id(location)).await.map_err(db_err)?.into_iter().next() else {
        if snapshot.is_some() { return Err(refuse()); }
        if crate::services::items::is_static_location(sde, location) { return Ok(None); }
        if !replaced.items.is_empty() || !replaced.reloads.is_empty() {
            return Err(crate::services::notify_error(infinity_core::messages::ITEM_NOT_AT_ORIGINAL_LOCATION));
        }
        return Ok(None);
    };
    let is_hull = ship_category(sde, &parent)?;
    // A captured snapshot is refused on every non-hull destination: ordinary
    // boxes keep their static rule for Current, never for a bound token.
    if !is_hull {
        if snapshot.is_some() { return Err(refuse()); }
    }
    let is_box = !is_hull && is_ordinary_container(sde, parent.type_id);
    if !is_hull && !is_box || is_hull && super::is_fitting_flag(flag) {
        return Ok(None);
    }
    let (parent, hull_capacity) = if is_hull {
        if !ship::lock_reload_ship(ex, location).await.map_err(db_err)? { return Err(refuse()); }
        if let Some(snapshot) = snapshot {
            // The joined witness replaces the plain hull read: ship, attribute
            // and complete persisted state must match the captured fit.
            let bound = crate::dao::items::dogma::hull_override(ex, location, snapshot.attribute).await.map_err(db_err)?.ok_or_else(refuse)?;
            if !snapshot.matches(&bound.item, bound.state) { return Err(refuse()); }
            validate_hull(sde, &bound.item, owner)?;
            (bound.item, snapshot.limit)
        } else {
            let hull = loading::load_items(ex, ItemQuery::Id(location)).await.map_err(db_err)?.into_iter().next().ok_or_else(refuse)?;
            validate_hull(sde, &hull, owner)?;
            (hull, effective(ex, sde, owner, location, flag).await?)
        }
    } else { (parent, CapacityLimit::Finite(0.0)) };
    let flag = if is_box { FLAG_CARGO } else { flag };
    let items = if is_box { loading::load_items(ex, ItemQuery::Location(location)).await }
        else { loading::load_items(ex, ItemQuery::LocationFlags(location, &[flag])).await }.map_err(db_err)?;
    let mut reloads = if is_box || !replaced.reloads.is_empty() {
        pending_reloads::for_source(ex, location).await.map_err(db_err)?
    } else { Vec::new() };
    if !is_box { reloads.retain(|row| row.source_flag == flag); }
    let (removed, added) = replacement_volume(sde, flag, &items, &reloads, additions, replaced)?;
    for item in additions { holds::admission(sde, flag, item.type_id, item.singleton, item.quantity)?; }
    let box_room = if is_box { Some(container_room(sde, &parent, &items, &reloads)?.ok_or_else(refuse)?) } else { None };
    let capacity = match &box_room { Some(room) => room.capacity, None => hull_capacity };
    // Finite keeps its original early return; Unlimited always computes the
    // real occupied volume because the quota is the only thing waived.
    let finite_early_return = match capacity {
        CapacityLimit::Finite(cap) => is_box || cap > 0.0 || additions.is_empty(),
        CapacityLimit::Unlimited => false,
    };
    if added <= removed && finite_early_return {
        return Ok(if is_hull { Some(crate::dao::items::parent_locks::LockedItems::held_hull(ex, &parent)) } else { None });
    }
    let occupied = if let Some(room) = box_room {
        room.used
    } else if replaced.reloads.is_empty() {
        used(ex, sde, location, flag, &items).await?
    } else {
        total_volume(sde, flag, items.iter().map(|item| (item.type_id, item.singleton, item.quantity))
            .chain(reloads.iter().map(|row| (row.type_id, false, row.quantity))) )?
    };
    // An invalid occupied or resulting volume refuses outright under either
    // mode; Unlimited skips only the quota comparison itself.
    let exceeded = match capacity {
        CapacityLimit::Unlimited => !(occupied.is_finite() && occupied >= 0.0
            && ((occupied - removed).max(0.0) + added).is_finite()),
        CapacityLimit::Finite(capacity) => capacity <= 0.0 || occupied > capacity || added - removed > capacity - occupied,
    };
    if exceeded {
        if let (CapacityLimit::Finite(capacity), Some(item)) = (capacity, additions.first()) {
            let free = (capacity - (occupied - removed).max(0.0)).max(0.0);
            return Err(holds::full_error(flag, item.type_id, free, added));
        }
        if capacity == CapacityLimit::Unlimited {
            return Err(crate::services::user_error(infinity_core::messages::MSG_CANNOT_STORE_DESTINATION_RESTRICTED, vec![]));
        }
        return Ok(if is_hull { Some(crate::dao::items::parent_locks::LockedItems::held_hull(ex, &parent)) } else { None });
    }
    Ok(if is_hull { Some(crate::dao::items::parent_locks::LockedItems::held_hull(ex, &parent)) } else { None })
}

#[cfg(all(test, feature = "tests-inventory"))] mod tests;
