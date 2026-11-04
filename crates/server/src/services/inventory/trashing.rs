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
//! heat and client-side effect cleanup in the same post-commit outbox.
use crate::{client::Client, consts::{CATEGORY_MODULE, CATEGORY_SHIP, FLAG_CORP_DELIVERIES}, dao::items::{inventory as inventory_dao, loading::ItemQuery}, db::outbox::Outbox, messages, services::{db_err, items::load_items, market, notify_error, user_error, CallError}};
use crate::services::combat::{banks, reload_recovery, targeting_charges};
use crate::services::effects::online::{self, OnlinePolicy};
use crate::dao::items::{combat as combat_dao, pending_reloads};
use crate::dao::insurance;
use marshal::Value;
use super::{access::{corporate_custody, custody_allowed}, is_fitting_flag, movement::{dispatch_move, MoveEffect}, notifications::{notify_corp_item_removed, notify_item_removed}};


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
    item: &crate::dao::items::loading::Item,
    ship: i64,
    owner: i64,
    active_ship: Option<i64>,
    station: Option<i64>,
    notify: crate::space::NotifyTx,
) -> Result<(), CallError> {
    let reloads = pending_reloads::for_ship(tx, ship).await.map_err(db_err)?;
    let reload_ids: Vec<_> = reloads.into_iter()
        .filter(|reload| item.category_id == i64::from(CATEGORY_SHIP) || reload.module_id == item.item_id)
        .map(|reload| reload.reload_id)
        .collect();
    if !reload_ids.is_empty() {
        reload_recovery::return_pending(tx, sde, outbox, &reload_ids, false).await?;
    }
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
            online::set_online_core(tx, sde, &policy, outbox, ship, item.item_id, false).await?;
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
            outbox.operation(MoveEffect::ClearHeat(ship, targeting_charges::bank(tx, ship, master).await?));
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
    let db = c.db.clone(); let sde = c.sde.clone(); let roots = roots.to_vec();
    db.transaction(move |tx, outbox: &mut Outbox<MoveEffect>| {
        let sde = sde.clone(); let roots = roots.clone(); let notify = notify.clone();
        Box::pin(async move {
            let mut unique_roots = roots.clone();
            unique_roots.sort_unstable(); unique_roots.dedup();
            let closure = inventory_dao::lock_descendants(tx, &unique_roots).await.map_err(db_err)?;
            if unique_roots.iter().any(|id| !closure.contains(id)) {
                return Err(user_error(messages::MSG_CANNOT_TRASH_ITEM, vec![]));
            }
            let mut items = load_items(tx, &sde, ItemQuery::Ids(&closure)).await.map_err(db_err)?;
            items.sort_by_key(|i| i.item_id);
            if items.len() != closure.len() { return Err(user_error(messages::MSG_CANNOT_TRASH_ITEM, vec![])); }
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
                let ancestry = crate::dao::items::deliveries::ancestry(tx, item.item_id).await.map_err(db_err)?;
                let ancestry_ids: std::collections::HashSet<_> = ancestry.iter().map(|a| a.item_id).collect();
                if ancestry.iter().find(|a| !ancestry_ids.contains(&a.location_id)).is_none_or(|a| a.location_id != location) { return Err(user_error(messages::MSG_CANNOT_TRASH_ITEM, vec![])); }
                if Some(item.item_id) == active_ship { return Err(user_error(messages::MSG_CANNOT_TRASH_ITEM, vec![])); }
                if sde.typelists.is_untrashable(item.type_id, item.group_id as i32, item.category_id as i32) { return Err(user_error(messages::MSG_CANNOT_TRASH_ITEM, vec![])); }
                if !crate::dao::trades::reserved(tx, &[item.item_id]).await.map_err(db_err)?.is_empty() { return Err(user_error(messages::MSG_CANNOT_TRASH_ITEM, vec![])); }
                if crate::dao::industry::has_active_blueprint_job(tx, &[item.item_id]).await.map_err(db_err)? { return Err(user_error(messages::MSG_CANNOT_TRASH_ITEM, vec![])); }
                let custody = corporate_custody(tx, Some(&sde), item.item_id).await?;
                if let Some((_, _, flag)) = custody {
                    if flag == FLAG_CORP_DELIVERIES {
                        return Err(user_error(messages::MSG_CANNOT_TRASH_ITEM, vec![]));
                    }
                    if !custody_allowed(tx, &sde, item.item_id, me, Some(location), true).await?.unwrap_or(false) { return Err(user_error(messages::MSG_CANNOT_TRASH_ITEM, vec![])); }
                } else if item.owner_id != me { return Err(user_error(messages::MSG_CANNOT_TRASH_ITEM, vec![])); }
            }
            let mut heat = std::collections::HashMap::<i64, Vec<i64>>::new();
            for item in &items {
                let ship = owning_ship.get(&item.item_id).copied().unwrap_or(item.item_id);
                if item.category_id == i64::from(CATEGORY_MODULE) && is_fitting_flag(item.flag_id) {
                    heat.entry(ship).or_default().push(item.item_id);
                }
                if item.category_id == i64::from(CATEGORY_SHIP) || (item.category_id == i64::from(CATEGORY_MODULE) && is_fitting_flag(item.flag_id)) {
                    clear_deleted_runtime(tx, &sde, outbox, item, ship, item.owner_id, active_ship, station, notify.clone()).await?;
                }
            }
            if inventory_dao::delete_guarded(tx, &closure).await.map_err(db_err)? != closure.len() as u64 { return Err(user_error(messages::MSG_CANNOT_TRASH_ITEM, vec![])); }
            for (ship, modules) in heat { outbox.operation(MoveEffect::ClearHeat(ship, modules)); }
            for item in items {
                notify_item_removed(outbox, item.clone());
                notify_corp_item_removed(tx, outbox, &item).await?;
            }
            Ok(Value::list([]))
        })
    }, |value, outbox| async move {
        let world = c.world.clone();
        dispatch_move(value, outbox, |notification| notification.dispatch(&world, Some(c)))
    }).await
}

#[cfg(test)]
mod tests;
