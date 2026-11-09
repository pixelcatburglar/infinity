//! Per-cycle consumption of effective dogma module fuel requirements.
//! The hold order follows inventoryFlagsCommon.py GetBaysToCheck without priority bays;
//! quantities live in game.items. Unsupported effect families remain unsupported.
use std::collections::BTreeMap;
use marshal::Value;
use infinity_core::{consts::{dogma::{ATTR_CONSUMPTION_QUANTITY, ATTR_CONSUMPTION_TYPE}, FLAG_CARGO, FLAG_COLONY_RESOURCES_HOLD, FLAG_SPECIALIZED_FUEL_BAY, GROUP_FUEL_BLOCK, GROUP_ICE_PRODUCT, IX_STACK_SIZE}, messages};
use crate::{dao::{items::{loading::{Item, ItemQuery}, quantities::{self, QuantityChange}, ship}, trades}, db::outbox::{dispatch_sender, Outbox}, services::{combat::Ctx, db_err, fit::{Fit, Key}, inventory::notifications::{notify_item_change, notify_item_removed}, items::load_items, notify_error, CallError}};
use super::lifecycle::Module;

fn refusal() -> CallError { notify_error(messages::effects::INSUFFICIENT_FUEL) }

// Each positive per-module requirement rounds up to indivisible units before aggregation.
fn requirement(type_value: f64, quantity: f64) -> Result<Option<(i32, i64)>, CallError> {
    if !type_value.is_finite() || !quantity.is_finite() || quantity < 0.0 || type_value < 0.0 || type_value.fract() != 0.0 || type_value > f64::from(i32::MAX) {
        return Err(CallError::Internal(anyhow::anyhow!("invalid module fuel attributes")));
    }
    if quantity == 0.0 { return Ok(None); }
    if type_value == 0.0 { return Err(CallError::Internal(anyhow::anyhow!("fuel quantity without type"))); }
    let units = quantity.ceil();
    if units >= i64::MAX as f64 { return Err(CallError::Internal(anyhow::anyhow!("module fuel quantity overflow"))); }
    Ok(Some((type_value as i32, units as i64)))
}

fn holds(group: Option<i32>) -> &'static [i32] {
    // The duplicate FuelBlock key in inventoryFlagsCommon.py:458–483 uses its last value.
    match group {
        Some(GROUP_ICE_PRODUCT) => &[FLAG_SPECIALIZED_FUEL_BAY, FLAG_CARGO],
        Some(GROUP_FUEL_BLOCK) => &[FLAG_COLONY_RESOURCES_HOLD, FLAG_CARGO],
        _ => &[FLAG_CARGO],
    }
}

fn plan(requirements: &BTreeMap<i32, i64>, items: &[Item], reserved: &[i64], sde: &crate::sde::Sde, owner: i64) -> Result<BTreeMap<i64, QuantityChange>, CallError> {
    let mut changes = BTreeMap::new();
    for (&type_id, &amount) in requirements {
        let mut remaining = amount;
        let group = sde.types.get(type_id).and_then(|ty| ty.group_id);
        for &flag in holds(group) {
            for item in items.iter().filter(|item| item.type_id == type_id && item.flag_id == flag && item.owner_id == owner && !item.singleton && item.quantity > 0 && reserved.binary_search(&item.item_id).is_err()) {
                let take = remaining.min(item.quantity);
                let left = item.quantity - take;
                changes.insert(item.item_id, QuantityChange::new(item, (left > 0).then_some(left)));
                remaining -= take;
                if remaining == 0 { break; }
            }
            if remaining == 0 { break; }
        }
        if remaining != 0 { return Err(refusal()); }
    }
    Ok(changes)
}

pub(super) async fn consume_cycle(cx: &Ctx, fit: &Fit, modules: &[Module]) -> Result<(), CallError> {
    let mut requirements = BTreeMap::<i32, i64>::new();
    for module in modules {
        let key = Key::Module(module.id);
        if let Some((type_id, units)) = requirement(fit.value(key, ATTR_CONSUMPTION_TYPE), fit.value(key, ATTR_CONSUMPTION_QUANTITY))? {
            if cx.world.sde().types.get(type_id).is_none() { return Err(CallError::Internal(anyhow::anyhow!("unknown module fuel type {type_id}"))); }
            let total = requirements.entry(type_id).or_default();
            *total = total.checked_add(units).ok_or_else(|| CallError::Internal(anyhow::anyhow!("module fuel total overflow")))?;
        }
    }
    if requirements.is_empty() { return Ok(()); }
    let sde = cx.world.sde().clone();
    let world = cx.world.clone();
    let sender = cx.notify.clone();
    let (ship_id, owner) = (cx.ship_id, cx.char_id);
    cx.db.transaction(
        |tx, outbox: &mut Outbox| {
            let sde = sde.clone();
            let requirements = requirements.clone();
            Box::pin(async move {
                if !ship::lock_reload_ship(tx, ship_id).await.map_err(db_err)? { return Err(refusal()); }
                let flags = [FLAG_CARGO, FLAG_SPECIALIZED_FUEL_BAY, FLAG_COLONY_RESOURCES_HOLD];
                let items = load_items(tx, &sde, ItemQuery::LocationOwnerFlags(ship_id, owner, &flags)).await.map_err(db_err)?;
                let ids: Vec<_> = items.iter().filter(|item| requirements.contains_key(&item.type_id) && !item.singleton && item.quantity > 0).map(|item| item.item_id).collect();
                let reserved = trades::reserved(tx, &ids).await.map_err(db_err)?;
                let changes = plan(&requirements, &items, &reserved, &sde, owner)?;
                if !quantities::apply(tx, &changes).await.map_err(db_err)? { return Err(refusal()); }
                for item in items {
                    if let Some(change) = changes.get(&item.item_id) {
                        if change.quantity.is_none() { notify_item_removed(outbox, item); }
                        else { notify_item_change(tx, &sde, outbox, item.item_id, vec![(IX_STACK_SIZE, Value::Int(item.quantity))]).await?; }
                    }
                }
                Ok(())
            })
        },
        |value, outbox| async move { dispatch_sender(value, outbox, &world, &sender) },
    ).await
}

#[cfg(test)] mod tests;
