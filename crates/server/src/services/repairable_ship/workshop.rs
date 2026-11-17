//! `repairableShipMgr` workshop RPCs: `OnDisassemble` grinds pristine
//! non-singleton items into refurbished components, `OnRefurbishment` spends
//! components to raise the service life of damaged items.
//! Mirrors `repairableShip/client/ui/disassemble_window.py` and
//! `refurbishment_window.py`: inputs must be reachable at the docked station,
//! component yield is `floor(maxLife / restore) * quantity`, refurbishment
//! allocates components to the least-damaged items first (damage ascending,
//! then id). A requested component matches every selected item whose source
//! maps to it, so items of different sources share one damage-ordered queue.
//! Requested counts must fit both authorized inventory and the selected
//! items' component demand; only components actually used are consumed.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use infinity_core::consts::{FLAG_CORP_DELIVERIES, FLAG_HANGAR};
use infinity_core::messages::repairable_ship as msg;
use marshal::{DbRowDescriptor, Value};

use crate::client::{filetime_now, Client};
use crate::dao;
use crate::dao::items::loading::ItemQuery;
use crate::dao::items::quantities::{self, QuantityChange};
use crate::dao::items::{repairable_ship as custody, service_life as life_dao};
use crate::dao::items::parent_locks::{LockRequest, LockStrength};
use crate::dao::repairable_ship::ItemId;
use crate::db::executor::Executor;
use crate::db::outbox::{dispatch_sender, Outbox};
use crate::services::identity::Identity;
use crate::services::inventory::access::{custody_allowed, may_take_corp_hangar};
use crate::services::inventory::destination::Destination;
use crate::services::inventory::notifications;
use crate::services::inventory::reachability::{Endpoint, Reachability};
use crate::services::{db_err, items::load_items, market, notify_error, Call, CallError, CallResult};
use super::service_life;

fn reject_dis() -> CallError { notify_error(msg::DISASSEMBLE_REJECTED) }
fn reject_ref() -> CallError { notify_error(msg::REFURBISHMENT_REJECTED) }

fn allocation(damage: f64, restore: f64, available: i64) -> Result<(i64, f64), CallError> {
    let needed = (damage / restore).ceil();
    if !damage.is_finite() || damage < 0.0 || !restore.is_finite() || restore <= 0.0
        || !needed.is_finite() || needed >= i64::MAX as f64 || available < 0 {
        return Err(reject_ref());
    }
    let used = (needed as i64).min(available);
    Ok((used, (damage - used as f64 * restore).max(0.0)))
}

/// `OnDisassemble(itemIDs, sourceID, outputOwnerID, outputLocationID, outputFlagID)`
/// -> list of consumed inventory ids.
pub(super) async fn disassemble(c: &mut Client, call: &Call<'_>) -> CallResult {
    disassemble_for(Identity::of_client(c), &c.db, &c.world, &c.sde, &c.notify_tx, call).await
}

async fn disassemble_for(
    identity: Identity, db: &crate::db::Db, world: &crate::space::World,
    sde: &Arc<crate::sde::Sde>, sender: &crate::space::NotifyTx, call: &Call<'_>,
) -> CallResult {
    let me = identity.char_id;
    let station = identity.station_id.ok_or_else(reject_dis)?;

    let values = call.args.first().and_then(Value::items).ok_or_else(reject_dis)?;
    let ids = values.iter().map(|value| value.as_int().filter(|id| *id > 0).ok_or_else(reject_dis)).collect::<Result<Vec<_>, _>>()?;
    if ids.is_empty() { return Err(reject_dis()); }
    // Duplicates are rejected before any computation.
    if ids.iter().copied().collect::<BTreeSet<_>>().len() != ids.len() {
        return Err(reject_dis());
    }
    let source_id = call.args.get(1).and_then(Value::as_int).filter(|id| *id > 0).ok_or_else(reject_dis)?;
    let out_owner = call.args.get(2).and_then(Value::as_int).filter(|id| *id > 0).ok_or_else(reject_dis)?;
    let out_location = call.args.get(3).and_then(Value::as_int).filter(|id| *id > 0).ok_or_else(reject_dis)?;
    let out_flag = call.args.get(4).and_then(Value::as_int).and_then(|f| i32::try_from(f).ok()).ok_or_else(reject_dis)?;

    // Reachability is sampled once before the transaction; the guarded writes
    // inside it re-verify everything that could have changed.
    let items = load_items(db, sde, ItemQuery::Ids(&ids)).await.map_err(db_err)?;
    if items.len() != ids.len() { return Err(reject_dis()); }
    let endpoints: Vec<Endpoint> = items.iter()
        .map(|item| Endpoint { location: item.location_id, flag: item.flag_id })
        .collect();
    let reach = Reachability::capture(&identity, db, world, identity.system_id, &endpoints).await?;

    let _lock = market::lock().await;
    let sde = Arc::clone(sde);
    let plan = db.transaction(
        move |tx, outbox: &mut Outbox| {
            let sde = Arc::clone(&sde);
            let reach = reach.clone();
            let ids = ids.clone();
            let endpoints = endpoints.clone();
            Box::pin(async move {
                let items = load_items(tx, &sde, ItemQuery::Ids(&ids)).await.map_err(db_err)?;
                if items.len() != ids.len() { return Err(reject_dis()); }
                if items.iter().zip(&endpoints).any(|(item, endpoint)|
                    item.location_id != endpoint.location || item.flag_id != endpoint.flag) { return Err(reject_dis()); }
                let requests: Vec<_> = ids.iter().map(|&item_id| LockRequest { item_id, strength: LockStrength::Update }).collect();
                let locks = crate::services::inventory::destination::lock_items(tx, &sde, &requests, &[]).await?;
                if items.iter().any(|item| locks.row(item.item_id).is_none_or(|row|
                    row.type_id != item.type_id || row.owner_id != item.owner_id || row.location_id != item.location_id
                    || row.flag_id != item.flag_id || row.quantity != item.quantity || row.singleton != item.singleton
                    || row.jump_clone_id.is_some())) { return Err(reject_dis()); }
                let admitted = reach.inspect_many(tx, &sde, &endpoints).await?.admitted;
                let item_ids: Vec<ItemId> = items.iter()
                    .map(|item| ItemId::new(item.item_id).ok_or_else(reject_dis))
                    .collect::<Result<_,_>>()?;
                let shapes = custody::capture_many(tx, &item_ids).await.map_err(db_err)?;
                if shapes.len() != item_ids.len() || shapes.iter().any(|row| row.brain_slot.is_some() || row.jump_clone.is_some()) {
                    return Err(reject_dis());
                }
                service_life::require_pristine(tx, &item_ids).await?;
                let online = custody::online(tx, &item_ids).await.map_err(db_err)?;
                if dao::trades::reserved(tx, &ids).await.map_err(db_err)?.iter().any(|_| true)
                    || dao::industry::has_active_blueprint_job(tx, &ids).await.map_err(db_err)? {
                    return Err(reject_dis());
                }
                let sources = custody::dynamic_sources(tx, &item_ids).await.map_err(db_err)?;
                let mut outputs: BTreeMap<i32, i64> = BTreeMap::new();
                for (i, item) in items.iter().enumerate() {
                    if !admitted.get(i).copied().unwrap_or(false) { return Err(reject_dis()); }
                    let authorized = custody_allowed(tx, &sde, item.item_id, me, Some(station), true)
                        .await?.unwrap_or(item.owner_id == me);
                    if !authorized { return Err(reject_dis()); }
                    if item.location_id != source_id { return Err(reject_dis()); }
                    if item.singleton || online.contains(&ItemId::new(item.item_id).unwrap()) {
                        return Err(reject_dis());
                    }
                    let ty = sde.types.get(item.type_id)
                        .ok_or_else(|| CallError::Internal(anyhow::anyhow!("unknown type {}", item.type_id)))?;
                    let (group_id, category_id) = (ty.group_id.ok_or_else(reject_dis)?, ty.category_id.ok_or_else(reject_dis)?);
                    if sde.repairable_ship.unrepairable(category_id, group_id, item.type_id) {
                        return Err(reject_dis());
                    }
                    let source_type = sources.get(&ItemId::new(item.item_id).unwrap())
                        .copied().unwrap_or(item.type_id);
                    let component = sde.repairable_ship.component(source_type)
                        .ok_or_else(reject_dis)?;
                    let per = (sde.repairable_ship.max_life() / sde.repairable_ship.restore_per_component()).floor();
                    if !per.is_finite() || per < 0.0 || per >= i64::MAX as f64 || item.quantity <= 0 { return Err(reject_dis()); }
                    let yield_qty = (per as i64).checked_mul(item.quantity).ok_or_else(reject_dis)?;
                    let total = outputs.entry(component).or_default();
                    *total = total.checked_add(yield_qty).ok_or_else(reject_dis)?;
                }
                // Output destination: personal or authorized corporate.
                let (o_owner, o_loc, o_flag) = resolve_output(tx, me, station, out_owner, out_location, out_flag).await?;
                // Consume whole stacks.
                let mut changes = BTreeMap::new();
                for item in &items {
                    changes.insert(item.item_id, QuantityChange::new(item, None));
                }
                if !quantities::apply(tx, &changes).await.map_err(db_err)? { return Err(reject_dis()); }
                for item in &items { notifications::notify_item_removed(outbox, item.clone()); }
                for (&type_id, &qty) in &outputs {
                    if qty <= 0 { continue; }
                    crate::services::items::deposit(tx, &sde, outbox,
                        &Destination { item_owner: o_owner, capacity_owner: o_owner, location: o_loc, flag: o_flag },
                        type_id, qty, 0).await?;
                }
                Ok(items.iter().map(|item| item.item_id).collect::<Vec<_>>())
            })
        },
        |consumed, outbox| async move { dispatch_sender(consumed, outbox, world, Some(sender)) },
    ).await?;
    Ok(Value::List(plan.into_iter().map(Value::Int).collect()))
}

/// Resolve a personal hangar or corporate division with live take permission.
async fn resolve_output(
    ex: &(impl Executor + ?Sized),
    me: i64, station: i64, owner: i64, location: i64, flag: i32,
) -> Result<(i64, i64, i32), CallError> {
    if owner == me {
        if location != station || flag != FLAG_HANGAR { return Err(reject_dis()); }
        return Ok((me, location, flag));
    }
    // Corporate output: deliveries flag lands at the station, divisions land
    // in the corporation's leased office item.
    if !may_take_corp_hangar(ex, me, owner, station, flag).await? { return Err(reject_dis()); }
    if flag == FLAG_CORP_DELIVERIES && location == station {
        return Ok((owner, location, flag));
    }
    let office = dao::offices::active_at(ex, owner, station, filetime_now()).await.map_err(db_err)?;
    if flag != FLAG_CORP_DELIVERIES && office.is_some_and(|office| office.office_id == location) {
        return Ok((owner, location, flag));
    }
    Err(reject_dis())
}

/// `OnRefurbishment(itemRows, {componentType: count})` -> {row: newDamage}.
/// Input rows carry their own itemID column; the same id twice (even via a
/// different row object) refuses the batch before any math.
pub(super) async fn refurbish(c: &mut Client, call: &Call<'_>) -> CallResult {
    refurbish_for(Identity::of_client(c), &c.db, &c.world, &c.sde, &c.notify_tx, call).await
}

async fn refurbish_for(
    identity: Identity, db: &crate::db::Db, world: &crate::space::World,
    sde: &Arc<crate::sde::Sde>, sender: &crate::space::NotifyTx, call: &Call<'_>,
) -> CallResult {
    let me = identity.char_id;
    let station = identity.station_id.ok_or_else(reject_ref)?;

    let rows: Vec<Value> = call.args.first().and_then(|v| v.items()).map(|rows| rows.to_vec()).unwrap_or_default();
    if rows.is_empty() { return Err(reject_ref()); }
    let mut row_ids: Vec<(i64, Value)> = Vec::with_capacity(rows.len());
    for row in &rows {
        let Value::DbRow { descriptor, .. } = row else { return Err(reject_ref()); };
        let desc = DbRowDescriptor::from_value(descriptor).ok_or_else(reject_ref)?;
        let id = desc.get(row, "itemID").and_then(|v| v.as_int()).ok_or_else(reject_ref)?;
        if row_ids.iter().any(|(seen, _)| *seen == id) { return Err(reject_ref()); }
        row_ids.push((id, row.clone()));
    }
    let mut counts: BTreeMap<i32, i64> = BTreeMap::new();
    let Some(Value::Dict(pairs)) = call.args.get(1) else { return Err(reject_ref()) };
    for (k, v) in pairs {
        let t = k.as_int().and_then(|t| i32::try_from(t).ok()).ok_or_else(reject_ref)?;
        let n = v.as_int().ok_or_else(reject_ref)?;
        if n < 0 || counts.insert(t, n).is_some() { return Err(reject_ref()); }
    }

    let ids: Vec<_> = row_ids.iter().map(|(id, _)| *id).collect();
    let preview = load_items(db, sde, ItemQuery::Ids(&ids)).await.map_err(db_err)?;
    if preview.len() != ids.len() { return Err(reject_ref()); }
    let endpoints: Vec<_> = preview.iter().map(|item| Endpoint { location: item.location_id, flag: item.flag_id }).collect();
    let reach = Reachability::capture(&identity, db, world, identity.system_id, &endpoints).await?;

    let _lock = market::lock().await;
    let sde = Arc::clone(sde);
    let result = db.transaction(
        move |tx, outbox: &mut Outbox| {
            let sde = Arc::clone(&sde);
            let row_ids = row_ids.clone();
            let counts = counts.clone();
            let reach = reach.clone();
            let endpoints = endpoints.clone();
            Box::pin(async move {
                let ids: Vec<i64> = row_ids.iter().map(|(id, _)| *id).collect();
                let items = load_items(tx, &sde, ItemQuery::Ids(&ids)).await.map_err(db_err)?;
                if items.len() != ids.len() { return Err(reject_ref()); }
                if items.iter().zip(&endpoints).any(|(item, endpoint)|
                    item.location_id != endpoint.location || item.flag_id != endpoint.flag) { return Err(reject_ref()); }
                let locations: BTreeSet<_> = items.iter().map(|item| (item.location_id, item.flag_id, item.owner_id))
                    .chain(std::iter::once((station, FLAG_HANGAR, me))).collect();
                let mut stacks_by_type = BTreeMap::new();
                for &component in counts.keys() {
                    let mut stacks = Vec::new();
                    for &(location, flag, owner) in &locations {
                        stacks.extend(load_items(tx, &sde, ItemQuery::LocationFlagTypeOwnerStack(location, flag, component, owner)).await.map_err(db_err)?);
                    }
                    stacks.sort_by_key(|item| item.item_id);
                    stacks_by_type.insert(component, stacks);
                }
                let requests: Vec<_> = items.iter().chain(stacks_by_type.values().flatten())
                    .map(|item| LockRequest { item_id: item.item_id, strength: LockStrength::Update }).collect();
                let locks = crate::services::inventory::destination::lock_items(tx, &sde, &requests, &[]).await?;
                if items.iter().chain(stacks_by_type.values().flatten()).any(|item| locks.row(item.item_id).is_none_or(|row|
                    row.type_id != item.type_id || row.owner_id != item.owner_id || row.location_id != item.location_id
                    || row.flag_id != item.flag_id || row.quantity != item.quantity || row.singleton != item.singleton
                    || row.jump_clone_id.is_some())) { return Err(reject_ref()); }
                let all_ids: Vec<_> = items.iter().chain(stacks_by_type.values().flatten()).map(|item| item.item_id).collect();
                if !dao::trades::reserved(tx, &all_ids).await.map_err(db_err)?.is_empty()
                    || dao::industry::has_active_blueprint_job(tx, &all_ids).await.map_err(db_err)? { return Err(reject_ref()); }
                let item_ids: Vec<ItemId> = ids.iter()
                    .map(|&id| ItemId::new(id).ok_or_else(reject_ref))
                    .collect::<Result<_,_>>()?;
                let shapes = custody::capture_many(tx, &item_ids).await.map_err(db_err)?;
                if shapes.len() != item_ids.len() || shapes.iter().any(|row| row.brain_slot.is_some() || row.jump_clone.is_some())
                    || !custody::online(tx, &item_ids).await.map_err(db_err)?.is_empty() { return Err(reject_ref()); }
                let admitted = reach.inspect_many(tx, &sde, &endpoints).await?.admitted;
                for (index, item) in items.iter().enumerate() {
                    if !admitted.get(index).copied().unwrap_or(false) || !item.singleton
                        || !custody_allowed(tx, &sde, item.item_id, me, Some(station), true).await?.unwrap_or(item.owner_id == me) {
                        return Err(reject_ref());
                    }
                }
                for stack in stacks_by_type.values().flatten() {
                    if !custody_allowed(tx, &sde, stack.item_id, me, Some(station), true).await?.unwrap_or(stack.owner_id == me) {
                        return Err(reject_ref());
                    }
                }
                // Fetch current damage and dynamic sources.
                let damages = life_dao::get_many(tx, &item_ids).await.map_err(db_err)?;
                let sources = custody::dynamic_sources(tx, &item_ids).await.map_err(db_err)?;
                // Sort candidates by ascending damage, then id.
                let mut work: Vec<(ItemId, i32, f64)> = items.iter().map(|item| {
                    let id = ItemId::new(item.item_id).unwrap();
                    let damage = damages.get(&id).copied().unwrap_or(0.0);
                    let source_type = sources.get(&id).copied().unwrap_or(item.type_id);
                    (id, source_type, damage)
                }).filter(|(_, _, d)| *d > 0.0).collect();
                work.sort_by(|a, b| a.2.partial_cmp(&b.2).unwrap_or(std::cmp::Ordering::Equal).then(a.0.cmp(&b.0)));
                // Allocate each component type to its matching items.
                let restore = sde.repairable_ship.restore_per_component();
                for (&component, &count) in &counts {
                    if !items.iter().any(|item| {
                        let source_type = sources.get(&ItemId::new(item.item_id).unwrap()).copied().unwrap_or(item.type_id);
                        sde.repairable_ship.component(source_type) == Some(component)
                    }) {
                        return Err(reject_ref());
                    }
                    let available = stacks_by_type[&component].iter().try_fold(0_i64, |sum, stack| {
                        (stack.quantity > 0).then_some(stack.quantity).and_then(|quantity| sum.checked_add(quantity))
                    }).ok_or_else(reject_ref)?;
                    let needed = work.iter().filter(|(_, ty, _)| sde.repairable_ship.component(*ty) == Some(component)).try_fold(0_i64, |sum, (_, _, damage)| {
                        let (required, _) = allocation(*damage, restore, i64::MAX)?;
                        sum.checked_add(required).ok_or_else(reject_ref)
                    })?;
                    if count > available || count > needed { return Err(reject_ref()); }
                }
                let mut updates: BTreeMap<ItemId, f64> = BTreeMap::new();
                let mut used: BTreeMap<i32, i64> = BTreeMap::new();
                for (&comp, &count) in &counts {
                    let mut remaining = count;
                    for (id, source_type, damage) in &mut work {
                        if remaining <= 0 { break; }
                        if sde.repairable_ship.component(*source_type) != Some(comp) { continue; }
                        let (take, remaining_damage) = allocation(*damage, restore, remaining)?;
                        if take <= 0 { continue; }
                        *damage = remaining_damage;
                        remaining -= take;
                        let total = used.entry(comp).or_default();
                        *total = total.checked_add(take).ok_or_else(reject_ref)?;
                        updates.insert(*id, *damage);
                    }
                }
                if updates.is_empty() { return Ok(Value::Dict(vec![])); }
                // Guarded damage writes.
                for (&id, &new_damage) in &updates {
                    let expected = damages.get(&id).copied().unwrap_or(0.0);
                    if !life_dao::set_if(tx, id, Some(expected), new_damage).await.map_err(db_err)? {
                        return Err(notify_error(msg::STATE_CHANGED));
                    }
                }
                // Consume only the components actually used, from hangar stacks.
                for (&comp, &qty) in &used {
                    let mut left = qty;
                    let stacks = stacks_by_type.get(&comp).ok_or_else(reject_ref)?;
                    let mut changes = BTreeMap::new();
                    for stack in stacks {
                        if left <= 0 { break; }
                        let take = stack.quantity.min(left);
                        left -= take;
                        let rem = stack.quantity - take;
                        changes.insert(stack.item_id, QuantityChange::new(stack, (rem > 0).then_some(rem)));
                    }
                    if left > 0 { return Err(reject_ref()); }
                    if !quantities::apply(tx, &changes).await.map_err(db_err)? { return Err(reject_ref()); }
                    for stack in stacks {
                        if let Some(change) = changes.get(&stack.item_id) {
                            match change.quantity {
                                Some(_) => notifications::notify_item_change(tx, &sde, outbox, stack.item_id,
                                    vec![(infinity_core::consts::IX_STACK_SIZE, Value::Int(stack.quantity))]).await?,
                                None => notifications::notify_item_removed(outbox, stack.clone()),
                            }
                        }
                    }
                }
                // Service-life notification + row-keyed reply.
                outbox.notify_client("OnItemServiceLifeDamageChanged",
                    vec![service_life::damage_dict(&updates)]);
                Ok(super::refurbishment_result(&row_ids, &updates))
            })
        },
        |value, outbox| async move { dispatch_sender(value, outbox, world, Some(sender)) },
    ).await?;
    Ok(result)
}

#[cfg(all(test, feature = "tests-services"))] mod tests;
