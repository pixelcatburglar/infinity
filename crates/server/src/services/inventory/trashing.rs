//! Atomic `invbroker.TrashItems` deletion of authorized item trees.
//! The same rule is used by Lua item deletion; all ancestry, custody and reservations
//! are checked inside the transaction before guarded sorted deletes.
//! Corporation deliveries are never trashed, even with the deliveries Take role:
//! the client treats them as a separate container whose items are taken out first.
//! Inventory writers that only create space objects (exploration/combat loot and
//! positioned containers), character-body implants, drone-bay/space state, trade
//! escrow, or newly-created ships/fittings cannot create a trashable station or
//! owned-container destination. The reachable cargo/hangar exceptions are the
//! slash `/create`, probe recovery, and pending-reload recovery writers; their
//! outer operations take the market lock. `return_pending` intentionally remains
//! lock-free because trashing and repackaging call it inside their existing lock.
//! Online fitted modules are taken offline through the existing transactional
//! lifecycle API before their item rows are deleted. This keeps its bank,
//! heat and client-side effect cleanup in the same post-commit outbox. Surviving
//! parent ships receive a single final fitting update after their children are deleted.
use infinity_core::{consts::{CATEGORY_MODULE, CATEGORY_SHIP, FLAG_CORP_DELIVERIES}, messages};
use crate::{client::Client, dao::items::{inventory as inventory_dao, loading::ItemQuery}, db::outbox::Outbox, services::{db_err, items::load_items, market, notify_error, user_error, CallError}};
use crate::services::combat::{banks, reload_recovery, targeting_charges};
use crate::services::effects::online::{self, OnlinePolicy};
use crate::dao::items::{combat as combat_dao, pending_reloads};
use crate::dao::insurance;
use marshal::Value;
use super::{access::{corporate_custody, custody_allowed}, is_fitting_flag, movement::{dispatch_move, MoveEffect}, notifications::{notify_corp_item_removed, notify_item_removed}};
use crate::dao::items::parent_locks::{LockRequest, LockStrength};
use super::destination;



/// Convert the trash business refusal into the catalogue key expected by
/// `invItemFunctions.TrashInvItems`; unrelated errors remain errors at the RPC boundary.
pub(super) fn broker_refusal_key(error: &CallError) -> Option<&'static str> {
    matches!(error, CallError::User { msg, .. } if msg == messages::MSG_CANNOT_TRASH_ITEM)
        .then_some(messages::MSG_CANNOT_TRASH_ITEM)
}

pub(crate) async fn resolve_location(c: &Client, item: i64) -> Result<i64, CallError> {
    let chain = crate::dao::items::deliveries::ancestry(&c.db, item).await.map_err(db_err)?;
    let ids: std::collections::HashSet<_> = chain.iter().map(|row| row.item_id).collect();
    chain.iter().find(|row| !ids.contains(&row.location_id)).map(|row| row.location_id)
        .ok_or_else(|| notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION))
}

async fn clear_deleted_runtime(
    tx: &(impl crate::db::executor::Executor + ?Sized),
    sde: &std::sync::Arc<crate::sde::Sde>,
    outbox: &mut Outbox<MoveEffect>,
    fences: &crate::services::effects::online::OnlineFences,
    item: &crate::dao::items::loading::Item,
    ship: i64,
    owner: i64,
    active_ship: Option<i64>,
    station: Option<i64>,
    notify: crate::space::NotifyTx,
) -> Result<(), CallError> {
    if item.category_id == i64::from(CATEGORY_SHIP) {
        let before = banks::snapshot(tx, ship, &[]).await?;
        combat_dao::clear_banks(tx, ship).await.map_err(db_err)?;
        for charge in crate::dao::items::charges::all(tx, ship).await.map_err(db_err)? {
            combat_dao::delete_charge(tx, ship, charge.flag_id).await.map_err(db_err)?;
        }
        for stop in banks::changes(tx, ship, before).await? {
            outbox.operation(MoveEffect::Bank(stop));
        }
        insurance::uninsure(tx, ship, item.owner_id).await.map_err(db_err)?;
    } else if item.category_id == i64::from(CATEGORY_MODULE) && is_fitting_flag(item.flag_id) {
        let online = crate::dao::items::dogma::online(tx, ship).await.map_err(db_err)?
            .into_iter().any(|module| module.item_id == item.item_id);
        if online {
            let policy = OnlinePolicy { owner, active_ship, station, notify };
            online::set_online_core(tx, sde, &policy, outbox, fences, ship, item.item_id, false).await?;
        } else {
            let before = banks::snapshot(tx, ship, &[item.item_id]).await?;
            let master = targeting_charges::master_for_member(tx, ship, item.item_id).await?;
            let changed = banks::remove_member(tx, ship, item.item_id, master, false).await?;
            for stop in banks::changes(tx, ship, before).await? {
                outbox.operation(MoveEffect::Bank(stop));
            }
            if changed {
                outbox.notify_client("OnWeaponBanksChanged", vec![Value::Int(ship), super::super::combat::banks(tx, ship).await?]);
            }
            online::queue_cleanup(outbox, fences, ship, targeting_charges::bank(tx, ship, master).await?);
        }
        combat_dao::delete_charge(tx, ship, item.flag_id).await.map_err(db_err)?;
    }
    Ok(())
}

pub(crate) async fn trash_items(c: &mut Client, roots: &[i64], location: i64) -> Result<Value, CallError> {
    let me = c.session.int("charid").unwrap_or(0);
    let active_ship = c.session.int("shipid");
    let station = c.session.int("stationid");
    let notify = c.notify_tx.clone();
    if roots.is_empty() { return Ok(Value::list([])); }
    let _lock = market::lock().await;
    let snapshots_ids = inventory_dao::descendants(&c.db, roots).await.map_err(db_err)?;
    let mut snapshots = load_items(&c.db, &c.sde, ItemQuery::Ids(&snapshots_ids)).await.map_err(db_err)?;
    snapshots.sort_by_key(|item| item.item_id);
    let notice_endpoints: Vec<_> = snapshots.iter().map(|item| super::reachability::Endpoint { location: item.location_id, flag: item.flag_id }).collect();
    let mut checked = std::collections::BTreeSet::new();
    let mut lineages = std::collections::HashMap::new();
    let mut shared_ids = std::collections::HashSet::new();
    for item in &snapshots {
        if station.is_none() && roots.contains(&item.item_id) { checked.insert((item.location_id, item.flag_id)); }
        if !lineages.contains_key(&item.location_id) {
            lineages.insert(item.location_id, crate::dao::items::deliveries::ancestry(&c.db, item.location_id).await.map_err(db_err)?);
        }
        let rows = &lineages[&item.location_id];
        let endpoint = super::reachability::Endpoint { location: item.location_id, flag: item.flag_id };
        if super::movement::shared_lineage(&c.sde, endpoint, rows.len(), |id| rows.iter().find(|row| row.item_id == id)) {
            shared_ids.insert(item.item_id);
        }
        let mut parent = item.location_id;
        let mut flag = item.flag_id;
        for _ in 0..rows.len() {
            let Some(row) = rows.iter().find(|row| row.item_id == parent) else { break };
            if c.sde.types.get(row.type_id).is_some_and(|ty| ty.category_id == Some(CATEGORY_SHIP)) { checked.insert((parent, flag)); }
            parent = row.location_id;
            flag = row.flag_id;
        }
    }
    let ancestors: Vec<i64> = lineages.values().flat_map(|rows| rows.iter().map(|row| row.item_id)).collect();
    let endpoints: Vec<_> = checked.into_iter().map(|(location, flag)| super::reachability::Endpoint { location, flag }).collect();
    let identity = crate::services::identity::Identity::of_client(c);
    let system = c.session.int("solarsystemid");
    let reach = super::reachability::Reachability::capture(&identity, &c.db, &c.world, system, &endpoints).await?;
    let audience = super::notifications::Audience::capture(&identity, &c.db, &c.world, system, &notice_endpoints).await?;
    let db = c.db.clone(); let sde = c.sde.clone(); let roots = roots.to_vec();
    let mut owners: Vec<_> = snapshots.iter().map(|item| item.owner_id).collect();
    owners.extend(lineages.values().flat_map(|rows| rows.iter().map(|row| row.owner_id)));
    owners.push(me);
    owners.sort_unstable();
    owners.dedup();
    let fences = online::capture_online_fences(&c.world, &owners).await;
    db.transaction(move |tx, outbox: &mut Outbox<MoveEffect>| {
        let sde = sde.clone(); let roots = roots.clone(); let notify = notify.clone(); let fences = fences.clone();
        let snapshots_ids = snapshots_ids.clone(); let snapshots = snapshots.clone();
        let endpoints = endpoints.clone(); let reach = reach.clone(); let audience = audience.clone();
        let shared_ids = shared_ids.clone();
        let ancestors = ancestors.clone();
        let lineages = lineages.clone();
        Box::pin(async move {
            let mut unique_roots = roots.clone();
            unique_roots.sort_unstable(); unique_roots.dedup();
            let captured = inventory_dao::descendants_and_ancestors(tx, &unique_roots, &ancestors).await.map_err(db_err)?;
            let mut ship_ids = std::collections::BTreeSet::new();
            for item in &snapshots {
                if item.category_id == i64::from(CATEGORY_SHIP) { ship_ids.insert(item.item_id); }
                else if item.category_id == i64::from(CATEGORY_MODULE) && is_fitting_flag(item.flag_id) { ship_ids.insert(item.location_id); }
            }
            let mut pending = Vec::new();
            for &ship in &ship_ids {
                pending.extend(pending_reloads::for_ship(tx, ship).await.map_err(db_err)?.into_iter().filter(|row|
                    snapshots.iter().any(|item| (item.category_id == i64::from(CATEGORY_SHIP) && item.item_id == row.ship_id)
                        || (item.category_id == i64::from(CATEGORY_MODULE) && is_fitting_flag(item.flag_id) && item.item_id == row.module_id))));
            }
            pending.sort_by_key(|row| row.reload_id);
            let mut refund = reload_recovery::RefundPreparation::discard(tx, &sde, &pending).await?;
            let mut requests: Vec<_> = captured.iter().map(|&item_id| LockRequest { item_id, strength: LockStrength::Update }).collect();
            requests.extend_from_slice(refund.requests());
            let locks = destination::lock_items(tx, &sde, &requests, &[]).await?;
            let mut current_pending = Vec::new();
            for &ship in &ship_ids {
                current_pending.extend(pending_reloads::for_ship(tx, ship).await.map_err(db_err)?.into_iter().filter(|row|
                    snapshots.iter().any(|item| (item.category_id == i64::from(CATEGORY_SHIP) && item.item_id == row.ship_id)
                        || (item.category_id == i64::from(CATEGORY_MODULE) && is_fitting_flag(item.flag_id) && item.item_id == row.module_id))));
            }
            current_pending.sort_by_key(|row| row.reload_id);
            if current_pending.iter().any(|row| !pending.contains(row)) { return Err(user_error(messages::MSG_CANNOT_TRASH_ITEM, vec![])); }
            let closure = inventory_dao::descendants(tx, &unique_roots).await.map_err(db_err)?;
            if unique_roots.iter().any(|id| !locks.is_write_locked(*id))
                || closure.iter().any(|id| !locks.is_write_locked(*id)) {
                return Err(user_error(messages::MSG_CANNOT_TRASH_ITEM, vec![]));
            }
            for (&parent, captured) in &lineages {
                if crate::dao::items::deliveries::ancestry(tx, parent).await.map_err(db_err)? != *captured {
                    return Err(user_error(messages::MSG_CANNOT_TRASH_ITEM, vec![]));
                }
                let outer = if captured.is_empty() { Some(parent) } else {
                    let ids: std::collections::HashSet<_> = captured.iter().map(|row| row.item_id).collect();
                    captured.iter().find(|row| !ids.contains(&row.location_id)).map(|row| row.location_id)
                };
                if outer != Some(location) { return Err(user_error(messages::MSG_CANNOT_TRASH_ITEM, vec![])); }
            }
            let mut items = load_items(tx, &sde, ItemQuery::Ids(&closure)).await.map_err(db_err)?;
            items.sort_by_key(|i| i.item_id);
            if items.len() != closure.len() { return Err(user_error(messages::MSG_CANNOT_TRASH_ITEM, vec![])); }
            if closure != snapshots_ids || items.iter().zip(&snapshots).any(|(a, b)| a.item_id != b.item_id
                || a.owner_id != b.owner_id || a.location_id != b.location_id || a.flag_id != b.flag_id
                || a.type_id != b.type_id || a.singleton != b.singleton || a.quantity != b.quantity)
                || reach.check_many(tx, &sde, &endpoints).await?.into_iter().any(|allowed| !allowed) {
                return Err(user_error(messages::MSG_CANNOT_TRASH_ITEM, vec![]));
            }
            if !crate::dao::trades::reserved(tx, &closure).await.map_err(db_err)?.is_empty()
                || crate::dao::industry::has_active_blueprint_job(tx, &closure).await.map_err(db_err)? {
                return Err(user_error(messages::MSG_CANNOT_TRASH_ITEM, vec![]));
            }
            let audience = audience.resolve(tx, &sde).await?;
            let mut owning_ship = std::collections::HashMap::new();
            for item in &items {
                let ship = if item.category_id == i64::from(CATEGORY_SHIP) {
                    item.item_id
                } else if item.category_id == i64::from(CATEGORY_MODULE) && is_fitting_flag(item.flag_id) {
                    let parent = load_items(tx, &sde, ItemQuery::Id(item.location_id)).await.map_err(db_err)?.into_iter().next()
                        .ok_or_else(|| user_error(messages::MSG_CANNOT_TRASH_ITEM, vec![]))?;
                    if parent.category_id != i64::from(CATEGORY_SHIP) || !parent.singleton {
                        return Err(user_error(messages::MSG_CANNOT_TRASH_ITEM, vec![]));
                    }
                    parent.item_id
                } else {
                    continue;
                };
                owning_ship.insert(item.item_id, ship);
            }
            for item in &items {
                if Some(item.item_id) == active_ship { return Err(user_error(messages::MSG_CANNOT_TRASH_ITEM, vec![])); }
                if sde.typelists.is_untrashable(item.type_id, item.group_id as i32, item.category_id as i32) { return Err(user_error(messages::MSG_CANNOT_TRASH_ITEM, vec![])); }
                let custody = if station.is_none() && shared_ids.contains(&item.item_id) { None }
                    else { corporate_custody(tx, Some(&sde), item.item_id).await? };
                if let Some((_, _, flag)) = custody {
                    if flag == FLAG_CORP_DELIVERIES {
                        return Err(user_error(messages::MSG_CANNOT_TRASH_ITEM, vec![]));
                    }
                    if !custody_allowed(tx, &sde, item.item_id, me, Some(location), true).await?.unwrap_or(false) { return Err(user_error(messages::MSG_CANNOT_TRASH_ITEM, vec![])); }
                } else if item.owner_id != me && !shared_ids.contains(&item.item_id) {
                    return Err(user_error(messages::MSG_CANNOT_TRASH_ITEM, vec![]));
                }
            }
            let mut boundaries = std::collections::BTreeMap::new();
            for item in &items {
                if item.category_id == i64::from(CATEGORY_MODULE) && is_fitting_flag(item.flag_id) {
                    let ship = owning_ship[&item.item_id];
                    if !closure.contains(&ship) && !boundaries.contains_key(&ship) {
                        let hull = load_items(tx, &sde, ItemQuery::Id(ship)).await.map_err(db_err)?.into_iter().next()
                            .ok_or_else(|| user_error(messages::MSG_CANNOT_TRASH_ITEM, vec![]))?;
                        let before = online::fitting::capture(tx, &sde, &fences, hull.owner_id, ship).await?;
                        boundaries.insert(ship, (hull.owner_id, before));
                    }
                }
            }
            let reload_ids: Vec<_> = pending.iter().map(|row| row.reload_id).collect();
            reload_recovery::return_pending(tx, &sde, outbox, &reload_ids, false, &locks, &mut refund).await?;
            let mut heat = std::collections::HashMap::<i64, Vec<i64>>::new();
            for item in &items {
                let ship = owning_ship.get(&item.item_id).copied().unwrap_or(item.item_id);
                if item.category_id == i64::from(CATEGORY_MODULE) && is_fitting_flag(item.flag_id) {
                    heat.entry(ship).or_default().push(item.item_id);
                }
                if item.category_id == i64::from(CATEGORY_SHIP) || (item.category_id == i64::from(CATEGORY_MODULE) && is_fitting_flag(item.flag_id)) {
                    clear_deleted_runtime(tx, &sde, outbox, &fences, item, ship, item.owner_id, active_ship, station, notify.clone()).await?;
                }
            }
            if inventory_dao::delete_guarded(tx, &closure, &locks).await.map_err(db_err)? != closure.len() as u64 { return Err(user_error(messages::MSG_CANNOT_TRASH_ITEM, vec![])); }
            for (ship, modules) in heat { online::queue_cleanup(outbox, &fences, ship, modules); }
            for item in items {
                let gone = crate::dao::items::loading::Item { location_id: infinity_core::consts::LOCATION_JUNKYARD, ..item.clone() };
                audience.item_change(outbox, &item, &gone, vec![(infinity_core::consts::IX_LOCATION_ID, Value::Int(item.location_id))], me);
                notify_item_removed(outbox, item.clone());
                if station.is_some() || !shared_ids.contains(&item.item_id) { notify_corp_item_removed(tx, outbox, &item).await?; }
            }
            for (ship, (owner, before)) in boundaries {
                let fit = online::reconcile_core(tx, &sde, outbox, &fences, owner, ship).await?;
                online::fitting::finish(tx, before, fit, outbox).await?;
            }
            Ok(Value::list([]))
        })
    }, |value, outbox| async move {
        let world = c.world.clone();
        dispatch_move(value, outbox, &world, |notification| online::dispatch_notification(notification, &world, &c.notify_tx, c.session.int("charid")))
    }).await
}

#[cfg(test)]
mod tests;
