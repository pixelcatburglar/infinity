//! Station repairSvc quotes and paid settlement, mirroring `repair/__init__.py`,
//! `eve/client/script/ui/station/repairshop/base_repairshop.py` and
//! `eve/common/script/util/inventoryUtil.py`. Condition is stored in game.item_attributes;
//! item context is read from game.items and prices and health from the startup SDE and Fit.
//! The client requires one row per item; aggregating ship armor and hull health
//! and applying the same repair fraction to both is a server inference.
//! Payment and full-quote comparison use wallet-cent precision; zero-cent charges are refused.
//! Damage persistence and attribute-change construction are shared with the GM
//! docked repair through `services::repair`; quotes and settlement stay here.
//! Structure facilities and in-space paste repair are not modelled.
use super::*;
use infinity_core::consts::{dogma::ATTR_ARMOR_DAMAGE, dogma::ATTR_ARMOR_HP, dogma::ATTR_HULL_DAMAGE, CARGO_CONTAINER_GROUPS, CATEGORY_DRONE, CATEGORY_MODULE, CATEGORY_SHIP, CATEGORY_STARBASE, FLAG_HANGAR};
use crate::dao::items::{dogma, parent_locks::{LockRequest, LockStrength}, repair};
use crate::db::{executor::Executor, outbox::{dispatch_world, Outbox}};
use crate::services::{fit::{Fit, Key}, inventory::is_fitting_flag, market, notify_error, repair as shared_repair, wallets};
use crate::sde::Sde;
use std::sync::Arc;
/// Client inventorycommon.const.categoryStructureModule.
const CATEGORY_STRUCTURE_MODULE: i32 = 66;
/// Client inventorycommon.const.categoryDeployable.
const CATEGORY_DEPLOYABLE: i32 = 22;
/// Client inventorycommon.const.groupTool.
const GROUP_TOOL: i32 = 332;

fn repairable(singleton: bool, category: Option<i32>, group: Option<i32>) -> bool {
    singleton && (matches!(category, Some(CATEGORY_DEPLOYABLE | CATEGORY_SHIP | CATEGORY_DRONE | CATEGORY_STARBASE | CATEGORY_MODULE | CATEGORY_STRUCTURE_MODULE))
        || group.is_some_and(|id| CARGO_CONTAINER_GROUPS.contains(&id) || id == GROUP_TOOL))
}


#[derive(Clone)]
struct Quote {
    item: i64, type_id: i32, group: i32, category: i32, attribute: i32,
    damage: f64, max: f64, unit: f64, module: bool,
}
impl Quote {
    fn wire(&self, damage: f64) -> Value {
        keyval([
            ("itemID", Value::Int(self.item)), ("typeID", Value::Int(self.type_id as i64)),
            ("groupID", Value::Int(self.group as i64)), ("damage", Value::Float(damage)),
            ("maxHealth", Value::Float(self.max)), ("repairable", Value::Bool(self.unit > 0.0)),
            ("costToRepairOneUnitOfDamage", Value::Float(self.unit)),
        ])
    }
}

// The client deduplicates by itemID; both ship layers share one price.
fn wire_rows(rows: &[Quote]) -> Vec<Value> {
    let mut emitted = Vec::new();
    let mut values = Vec::new();
    for row in rows {
        if emitted.contains(&row.item) { continue; }
        emitted.push(row.item);
        let damage: f64 = rows.iter().filter(|other| other.item == row.item).map(|r| r.damage).sum();
        values.push(row.wire(damage));
    }
    values
}
fn total_cost(rows: &[Quote]) -> f64 {
    let mut emitted = Vec::new();
    let mut total = 0.0;
    for row in rows {
        if emitted.contains(&row.item) { continue; }
        emitted.push(row.item);
        let damage: f64 = rows.iter().filter(|r| r.item == row.item).map(|r| r.damage).sum();
        total += damage.ceil() * row.unit;
    }
    total
}
fn refuse() -> CallError { notify_error(infinity_core::messages::repair::INVALID) }
/// Client inventoryUtil.GetComputedBasePrice(typeID, 1), including its material portion rule.
fn computed_base_price(sde: &Sde, id: i32) -> f64 {
    let Some(ty) = sde.types.get(id) else { return 0.0 };
    let materials = ty.materials();
    if materials.is_empty() {
        let portion = ty.portion_size.unwrap_or(1).max(1) as f64;
        return (ty.base_price.unwrap_or(0.0) / portion).trunc();
    }
    materials.iter().filter_map(|m| {
        let material = sde.types.get(m.material_type_id)?;
        let portion = material.portion_size.unwrap_or(0);
        if portion <= 0 { return None; }
        Some(m.quantity.unwrap_or(0) as f64 * (material.base_price.unwrap_or(0.0) / portion as f64).trunc() / portion as f64)
    }).sum::<f64>().trunc()
}

fn fraction(payment: f64, total: f64, modules: bool) -> Option<f64> {
    if !payment.is_finite() || !total.is_finite() || payment <= 0.0 || total <= 0.0 { return None; }
    let payment = market::round2(payment);
    let total = market::round2(total);
    if !payment.is_finite() || !total.is_finite() || payment <= 0.0 || total <= 0.0 || payment > total || (modules && payment != total) { return None; }
    Some(if payment == total { 1.0 } else { payment / total })
}

async fn quotes(ex: &(impl Executor + ?Sized), sde: Arc<Sde>, ids: &[i64], owner: i64, station: i64) -> Result<Vec<(i64, Vec<Quote>)>, CallError> {
    let items = repair::candidates(ex, ids, owner).await.map_err(db_err)?;
    let candidate_ids: Vec<_> = items.iter().map(|item| item.item_id).collect();
    let requests: Vec<_> = candidate_ids.iter().map(|&item_id| LockRequest { item_id, strength: LockStrength::Update }).collect();
    let locks = crate::services::inventory::destination::lock_items(ex, &sde, &requests, &[]).await?;
    if items.iter().any(|item| locks.row(item.item_id).is_none_or(|row| row.jump_clone_id.is_some()
        || row.type_id != item.type_id || row.owner_id != item.owner_id || row.location_id != item.location_id
        || row.flag_id != item.flag_id || row.singleton != item.singleton)) { return Err(refuse()); }
    let fresh = repair::candidates(ex, ids, owner).await.map_err(db_err)?;
    if fresh.len() != items.len() || fresh.iter().zip(&items).any(|(a, b)| a.item_id != b.item_id
        || a.type_id != b.type_id || a.owner_id != b.owner_id || a.location_id != b.location_id
        || a.flag_id != b.flag_id || a.singleton != b.singleton) { return Err(refuse()); }
    let mut result = Vec::with_capacity(ids.len());
    for &id in ids {
        let item = items.iter().find(|i| i.item_id == id).ok_or_else(refuse)?;
        let ty = sde.types.get(item.type_id).ok_or_else(refuse)?;
        let in_hangar = item.owner_id == owner && item.location_id == station && item.flag_id == FLAG_HANGAR;
        let parent = items.iter().find(|p| p.item_id == item.location_id && p.owner_id == owner && p.location_id == station && p.flag_id == FLAG_HANGAR && p.singleton && sde.types.get(p.type_id).is_some_and(|t| t.category_id == Some(CATEGORY_SHIP)));
        if !repairable(item.singleton, ty.category_id, ty.group_id) || !(in_hangar || (parent.is_some() && is_fitting_flag(item.flag_id))) { return Err(refuse()); }
        let ship = ty.category_id == Some(CATEGORY_SHIP);
        let fit_ship = if ship { Some(id) } else { parent.map(|p| p.item_id) };
        let fit = if let Some(ship_id) = fit_ship { Some(Fit::load(ex, sde.clone(), owner, ship_id).await?) } else { None };
        let mut selected = vec![item];
        if ship { selected.extend(items.iter().filter(|m| m.location_id == id && m.owner_id == owner && m.singleton && is_fitting_flag(m.flag_id))); }
        let selected_ids: Vec<_> = selected.iter().map(|i| i.item_id).collect();
        let overrides = dogma::overrides_for(ex, &selected_ids).await.map_err(db_err)?;
        let mut rows = Vec::new();
        for selected_item in selected {
            let Some(data) = sde.types.get(selected_item.type_id) else { continue };
            if !repairable(selected_item.singleton, data.category_id, data.group_id) { continue; }
            let is_ship = selected_item.item_id == id && ship;
            let module = matches!(data.category_id, Some(CATEGORY_MODULE | CATEGORY_STRUCTURE_MODULE));
            let max_health = if is_ship {
                let fit = fit.as_ref().unwrap();
                fit.value(Key::Ship, ATTR_STRUCTURE_HP) + fit.value(Key::Ship, ATTR_ARMOR_HP)
            } else if let Some(fit) = &fit { fit.value(Key::Module(selected_item.item_id), ATTR_STRUCTURE_HP) }
            else { data.attribute(ATTR_STRUCTURE_HP).unwrap_or(0.0) };
            let base = computed_base_price(&sde, selected_item.type_id);
            for (attribute, hp) in [(ATTR_HULL_DAMAGE, ATTR_STRUCTURE_HP), (ATTR_ARMOR_DAMAGE, ATTR_ARMOR_HP)] {
                if attribute == ATTR_ARMOR_DAMAGE && !is_ship { continue; }
                let damage = overrides.iter().find(|v| v.item_id == selected_item.item_id && v.attribute_id == attribute).map_or(0.0, |v| v.value);
                let max = if is_ship { fit.as_ref().unwrap().value(Key::Ship, hp) }
                    else if let Some(fit) = &fit { fit.value(Key::Module(selected_item.item_id), hp) }
                    else { data.attribute(hp).unwrap_or(0.0) };
                if !damage.is_finite() || !max.is_finite() || damage < 0.0 || max < 0.0 || damage > max { return Err(refuse()); }
                if damage == 0.0 || max == 0.0 { continue; }
                let unit = base / max_health * crate::config::game().repair.cost_multiplier;
                if !unit.is_finite() || unit < 0.0 { return Err(refuse()); }
                rows.push(Quote { item: selected_item.item_id, type_id: selected_item.type_id, group: data.group_id.unwrap_or(0), category: data.category_id.unwrap_or(0), attribute, damage, max: max_health, unit, module });
            }
        }
        result.push((id, rows));
    }
    Ok(result)
}

fn station(c: &Client, call: &Call<'_>) -> Result<i64, CallError> {
    let station = c.session.int("stationid").filter(|v| *v > 0).ok_or_else(|| notify_error(infinity_core::messages::repair::UNSUPPORTED))?;
    let bound = call.bind_params.and_then(|v| v.items()).and_then(|v| v.first()).and_then(Value::as_int);
    if bound != Some(station) { return Err(refuse()); }
    Ok(station)
}
fn ids(call: &Call<'_>) -> Result<Vec<i64>, CallError> {
    let values = call.args.first().and_then(Value::items).ok_or_else(refuse)?;
    let mut ids = Vec::with_capacity(values.len());
    for value in values {
        let id = value.as_int().filter(|v| *v > 0).ok_or_else(refuse)?;
        if ids.contains(&id) { return Err(refuse()); }
        ids.push(id);
    }
    Ok(ids)
}

async fn handle(c: &Client, call: &Call<'_>) -> CallResult {
    if call.method == "RepairItemsInStructure" { return Err(notify_error(infinity_core::messages::repair::UNSUPPORTED)); }
    let station = station(c, call)?;
    let ids = ids(call)?;
    let owner = char_id(c);
    let payment = if call.method == "RepairItemsInStation" { Some(call.args.get(1).and_then(Value::as_float).ok_or_else(refuse)?) } else { None };
    let _lock = market::lock().await;
    let sde = c.sde.clone();
    let world = &*c.world;
    c.db.transaction(
        |tx, outbox: &mut Outbox| {
            let sde = sde.clone();
            let ids = ids.clone();
            Box::pin(async move {
                let grouped = quotes(tx, sde, &ids, owner, station).await?;
                let Some(payment) = payment else {
                    return Ok(Value::Dict(grouped.into_iter().map(|(id, rows)| (Value::Int(id), Value::list(wire_rows(&rows)))).collect()));
                };
                let mut rows: Vec<Quote> = grouped.into_iter().flat_map(|(_, rows)| rows).collect();
                rows.sort_by_key(|r| (r.item, r.attribute));
                rows.dedup_by_key(|r| (r.item, r.attribute));
                if rows.iter().any(|r| r.unit <= 0.0) { return Err(refuse()); }
                let total = total_cost(&rows);
                let portion = fraction(payment, total, rows.iter().any(|r| r.module)).ok_or_else(refuse)?;
                let charge = market::round2(payment);
                if !charge.is_finite() || charge <= 0.0 { return Err(refuse()); }
                wallets::debit(tx, outbox, owner, crate::isk::IskAmount::from_isk(charge)?).await?;
                let mut changed = Vec::new();
                let mut result: Vec<(i64, i32, i32, f64, f64, f64, f64)> = Vec::new();
                for row in rows {
                    let new = row.damage * (1.0 - portion);
                    if let Some(entry) = result.iter_mut().find(|entry| entry.0 == row.item) {
                        if row.attribute == ATTR_ARMOR_DAMAGE { entry.4 += row.damage * portion; }
                        else { entry.3 += row.damage * portion; }
                        entry.5 += row.damage;
                    } else {
                        result.push((row.item, row.category, row.type_id,
                            if row.attribute == ATTR_HULL_DAMAGE { row.damage * portion } else { 0.0 },
                            if row.attribute == ATTR_ARMOR_DAMAGE { row.damage * portion } else { 0.0 },
                            row.damage, row.unit));
                    }
                    changed.push(shared_repair::write_damage(tx, owner, row.item, row.attribute, row.damage, new).await?.ok_or_else(refuse)?);
                }
                outbox.notify_pilot(owner, "OnModuleAttributeChanges", vec![Value::list(changed)]);
                let mut remaining = charge;
                let last = result.len() - 1;
                let lines = result.into_iter().enumerate().map(|(index, (item, category, type_id, hull, armor, damage, unit))| {
                    let actual = if index == last { remaining } else { market::round2(charge * (damage.ceil() * unit / total)).min(remaining) };
                    remaining = market::round2(remaining - actual);
                    vec![Value::Int(item), Value::Float(hull), Value::Float(armor), Value::Int(category as i64), Value::Int(type_id as i64), Value::Float(actual)]
                }).collect();
                Ok(rowset(&["itemID", "amountOfStructureToRepair", "amountOfArmorToRepair", "categoryID", "typeID", "actualCost"], lines))
            })
        },
        |value, outbox| async move { dispatch_world(value, outbox, world) },
    ).await
}

pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    match (call.service, call.method) {
        ("repairSvc", "GetRepairQuotes" | "RepairItemsInStation" | "RepairItemsInStructure") => Some(handle(c, call).await),
        _ => None,
    }
}

#[cfg(test)] mod tests;
