//! Protobuf TextFormat notifications for `repairableShipMgr`, encoded through
//! the shared descriptor pool: `repairableShip/client/repairable_ship_svc.py`
//! parses `Responsed`, `ExplodedResponsed`, `DeliveryStarted`, `AbortStarted`
//! and `ItemsWithdrawn` with `google.protobuf.text_format.Parse`.
//! Sequential identifiers are uint64 unless the descriptor narrows them;
//! recovery DB quantities are i64 and refuse the wire instead of truncating.
//! The deprecated top-level `DeliveryStarted.repair_items` and the
//! `transaction_id` are left unset; the station/structure oneof only ever
//! carries `station`. Unpaid delivery and withdrawal deadlines stay absent
//! (the protocol zero value), never negative Unix seconds.
//! Billing and encoding share the same last-service-life projection.

use anyhow::{ensure, Context, Result};
#[cfg(all(test, feature = "tests-services"))] use crate::client::unix_parts_from_filetime;
use crate::dao::repairable_ship::{Group, RecoveryItem, Selection};
use prost_reflect::{DynamicMessage, ReflectMessage, Value as PV};
use crate::gateway::message::{child, named, nested, sequential, set, timestamp};



/// The service-life value sent in recovery item attributes.
pub(super) fn last_service_life(value: f64) -> f32 {
    value as f32
}

/// One `recoveryitem.RecoveryItem` message for an escrowed member; wire
/// conversions are checked, never truncated.
fn recovery_item(member: &RecoveryItem) -> Result<DynamicMessage> {
    let life = last_service_life(member.last_service_life);
    ensure!(member.last_service_life.is_finite() && member.last_service_life >= 0.0
        && life.is_finite(), "service life outside wire range");
    let group = child(&named("eve.repairableship.Responsed")?.descriptor(), "recovery_groups")?;
    let mut item = child(&group.descriptor(), "recovery_items")?;
    sequential(&mut item, "id", member.id.get() as u64)?;
    let mut attributes = child(&item.descriptor(), "attributes")?;
    set(&mut attributes, "quantity", PV::U32(u32::try_from(member.quantity)?))?;
    sequential(&mut attributes, "item_type", member.type_id as u64)?;
    if let Some(at) = member.delivery_completed_at { timestamp(&mut attributes, "delivery_complete_time", at)?; }
    if let Some(at) = member.withdraw_expires_at { timestamp(&mut attributes, "withdraw_expiry_time", at)?; }
    sequential(&mut attributes, "item", member.item.get() as u64)?;
    timestamp(&mut attributes, "repair_complete_time", member.repair_completed_at)?;
    set(&mut attributes, "item_flag", PV::EnumNumber(member.flag))?;
    set(&mut attributes, "last_service_life", PV::F32(life))?;
    nested(&mut item, "attributes", attributes)?;
    Ok(item)
}

/// One `recoverygroup.RecoveryGroup` message: identity, the frozen loss and
/// destination facts, and every member.
fn recovery_group(group: &Group, members: &[RecoveryItem]) -> Result<DynamicMessage> {
    let mut message = child(&named("eve.repairableship.Responsed")?.descriptor(), "recovery_groups")?;
    sequential(&mut message, "id", group.id.get() as u64)?;
    let mut attributes = child(&message.descriptor(), "attributes")?;
    sequential(&mut attributes, "ship_type", group.shape.type_id as u64)?;
    sequential(&mut attributes, "solar_system", group.system as u64)?;
    sequential(&mut attributes, "station", group.destination_station as u64)?;
    timestamp(&mut attributes, "recovery_expiry_time", group.payment_expires_at)?;
    timestamp(&mut attributes, "ship_explosion_time", group.occurred_at)?;
    sequential(&mut attributes, "ship", group.shape.item.get() as u64)?;
    nested(&mut message, "attributes", attributes)?;
    let mut items = Vec::with_capacity(members.len());
    for member in members { items.push(PV::Message(recovery_item(member)?)); }
    set(&mut message, "recovery_items", PV::List(items))?;
    Ok(message)
}

/// `eve.repairableship.Responsed`: the character's whole owned snapshot.
/// An empty group list still produces a legal notification.
pub(super) fn responsed(character: i64, selections: &[Selection]) -> Result<String> {
    let mut message = named("eve.repairableship.Responsed")?;
    sequential(&mut message, "character", character as u64)?;
    let mut groups = Vec::with_capacity(selections.len());
    for selection in selections { groups.push(PV::Message(recovery_group(&selection.group, &selection.items)?)); }
    set(&mut message, "recovery_groups", PV::List(groups))?;
    Ok(message.to_text_format())
}

/// `eve.repairableship.event.ExplodedResponsed` → `OnAddRecoveryGroup`.
pub(crate) fn exploded(character: i64, selection: &Selection) -> Result<String> {
    let mut message = named("eve.repairableship.event.ExplodedResponsed")?;
    sequential(&mut message, "character", character as u64)?;
    nested(&mut message, "recovery_group", recovery_group(&selection.group, &selection.items)?)?;
    Ok(message.to_text_format())
}

/// `eve.repairableship.DeliveryStarted` → `OnDeliveryStarted`, grouped repair
/// items carrying the purchased or replayed persisted deadlines.
pub(super) fn delivery_started(character: i64, selections: &[Selection]) -> Result<String> {
    let mut message = named("eve.repairableship.DeliveryStarted")?;
    sequential(&mut message, "character", character as u64)?;
    let mut groups = Vec::with_capacity(selections.len());
    for selection in selections {
        let mut group = child(&message.descriptor(), "recovery_groups")?;
        sequential(&mut group, "recovery_group", selection.group.id.get() as u64)?;
        let mut items = Vec::with_capacity(selection.items.len());
        for member in &selection.items {
            let mut repair = child(&group.descriptor(), "repair_items")?;
            sequential(&mut repair, "item", member.id.get() as u64)?;
            let deadline = |at: Option<i64>| at.context("paid member without deadline");
            timestamp(&mut repair, "delivery_complete_time", deadline(member.delivery_completed_at)?)?;
            timestamp(&mut repair, "withdraw_expiry_time", deadline(member.withdraw_expires_at)?)?;
            items.push(PV::Message(repair));
        }
        set(&mut group, "repair_items", PV::List(items))?;
        groups.push(PV::Message(group));
    }
    set(&mut message, "recovery_groups", PV::List(groups))?;
    Ok(message.to_text_format())
}

/// `eve.repairableship.AbortStarted` → `OnAbortStarted`: only the members the
/// caller actually selected are named for cache deletion.
pub(super) fn abort_started(character: i64, selections: &[Selection]) -> Result<String> {
    let mut message = named("eve.repairableship.AbortStarted")?;
    sequential(&mut message, "character", character as u64)?;
    let mut groups = Vec::with_capacity(selections.len());
    for selection in selections {
        let mut group = child(&message.descriptor(), "recovery_groups")?;
        sequential(&mut group, "recovery_group", selection.group.id.get() as u64)?;
        let mut items = Vec::with_capacity(selection.items.len());
        for member in &selection.items {
            let mut item = child(&group.descriptor(), "recovery_items")?;
            set(&mut item, "sequential", PV::U64(member.id.get() as u64))?;
            items.push(PV::Message(item));
        }
        set(&mut group, "recovery_items", PV::List(items))?;
        groups.push(PV::Message(group));
    }
    set(&mut message, "recovery_groups", PV::List(groups))?;
    Ok(message.to_text_format())
}

/// `eve.repairableship.ItemsWithdrawn` → `OnItemsWithdrawn`: one item group
/// per recovery group; `items[].id` is the recovery member identity and
/// `item_id` the persisted new inventory identity (a different ID band).
pub(super) fn items_withdrawn(character: i64, selections: &[Selection]) -> Result<String> {
    let mut message = named("eve.repairableship.ItemsWithdrawn")?;
    sequential(&mut message, "character", character as u64)?;
    let mut groups = Vec::with_capacity(selections.len());
    for selection in selections {
        let mut group = child(&message.descriptor(), "item_groups")?;
        sequential(&mut group, "solar_system", selection.group.system as u64)?;
        sequential(&mut group, "station", selection.group.destination_station as u64)?;
        timestamp(&mut group, "ship_explosion_time", selection.group.occurred_at)?;
        sequential(&mut group, "recovery_group", selection.group.id.get() as u64)?;
        let mut items = Vec::with_capacity(selection.items.len());
        for member in &selection.items {
            let mut item = child(&group.descriptor(), "items")?;
            sequential(&mut item, "id", member.id.get() as u64)?;
            sequential(&mut item, "item_id", member.withdrawn_item
                .context("withdrawn member without a stored inventory mapping")?.get() as u64)?;
            sequential(&mut item, "item_type", member.type_id as u64)?;
            items.push(PV::Message(item));
        }
        set(&mut group, "items", PV::List(items))?;
        groups.push(PV::Message(group));
    }
    set(&mut message, "item_groups", PV::List(groups))?;
    Ok(message.to_text_format())
}

#[cfg(all(test, feature = "tests-services"))] mod tests;
