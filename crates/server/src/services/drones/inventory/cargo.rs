//! Real cargo rows recovered by a salvage drone, delivered to the mother
//! ship's hold inside the caller's transaction.
//! Delivery always reads the drone's actual children: it never trusts a
//! restart-lost marker and never keeps an in-memory inventory. The caller
//! supplies the item-lock token that already covers the drone root
//! write-locked; every child must belong to the recipient, the batch is
//! capacity checked once, and rows move in ascending ID order. A full hold or
//! foreign-owned cargo refuses the whole delivery: nothing moves, the cargo
//! stays on the drone, and only the next player command retries it.
use std::sync::Arc;
use marshal::Value;
use infinity_core::consts::{FLAG_CARGO, IX_FLAG, IX_LOCATION_ID};
use infinity_core::messages;
use infinity_core::messages::drones as text;
use crate::dao::items::{inventory, loading::ItemQuery, parent_locks::LockedItems};
use crate::db::{executor::Executor, outbox::Outbox};
use crate::sde::Sde;
use crate::services::{db_err, items::load_items, notify_error, CallError,
    inventory::{hold_storage::{self, Addition, CapacitySource, HoldCapacity}, notifications::notify_item_change}};

/// owner of the drone root itself, which is not the cargo owner.
#[derive(Clone, Copy)]
pub(in crate::services::drones) struct CargoSource {
    pub(in crate::services::drones) drone: i64,
    pub(in crate::services::drones) type_id: i32,
    pub(in crate::services::drones) root_owner: i64,
    pub(in crate::services::drones) system: i64,
}

/// One contact's delivery result; `Full` moved nothing.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::services::drones) enum CargoDelivery {
    Empty,
    Delivered,
    Full,
}

/// Move every actual child of `source.drone` into `ship`'s cargo hold.
/// The lock token must cover the drone root write-locked and still match the
/// admitted positioned singleton. `Empty` and `Delivered` are idempotent
/// outcomes; `Full` changed no rows and `CARGO_FOREIGN` refuses mixed-owner
/// cargo without rewriting ownership.
pub(in crate::services::drones) async fn deliver_in<X: Send>(
    tx: &(impl Executor + ?Sized), outbox: &mut Outbox<X>, sde: &Arc<Sde>,
    source: &CargoSource, recipient: i64, ship: i64, capacity: HoldCapacity,
    locks: &LockedItems<'_>,
) -> Result<CargoDelivery, CallError> {
    let refuse = || notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION);
    let root = locks.row(source.drone).filter(|row| row.type_id == source.type_id
        && row.owner_id == source.root_owner && row.location_id == source.system
        && row.flag_id == 0 && row.singleton && row.quantity == 1);
    if root.is_none() || !locks.is_write_locked(source.drone) { return Err(refuse()); }
    let mut children = load_items(tx, sde, ItemQuery::Location(source.drone)).await.map_err(db_err)?;
    if children.is_empty() { return Ok(CargoDelivery::Empty); }
    children.sort_unstable_by_key(|item| item.item_id);
    if children.iter().any(|item| item.owner_id != recipient) {
        return Err(notify_error(text::CARGO_FOREIGN));
    }
    let additions: Vec<_> = children.iter().map(|item| Addition {
        type_id: item.type_id, singleton: item.singleton, quantity: item.quantity,
    }).collect();
    match hold_storage::check_destination(tx, sde, recipient, ship, FLAG_CARGO, &additions,
        Default::default(), CapacitySource::Captured(capacity)).await {
        Ok(_) => {}
        Err(CallError::User { msg, .. }) if msg == messages::MSG_NOT_ENOUGH_CARGO_SPACE => return Ok(CargoDelivery::Full),
        Err(error) => return Err(error),
    }
    for item in children {
        if !inventory::move_item(tx, &item, ship, FLAG_CARGO, recipient, item.singleton).await.map_err(db_err)? {
            return Err(refuse());
        }
        notify_item_change(tx, sde, outbox, item.item_id, vec![
            (IX_LOCATION_ID, Value::Int(source.drone)),
            (IX_FLAG, Value::Int(i64::from(item.flag_id))),
        ]).await?;
    }
    Ok(CargoDelivery::Delivered)
}
