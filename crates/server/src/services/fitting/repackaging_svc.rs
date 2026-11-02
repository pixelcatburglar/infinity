//! Item repackaging for `repackagingSvc`, mirroring
//! `eve/client/script/ui/services/menuSvcExtras/invItemFunctions.py`.
//! Uses game items, charges, reload reservations, banks, insurance and dogma overrides,
//! plus SDE stations and types. Confirmed ordinary ship rigs are destroyed; other
//! direct contents return to the owning station hangar atomically.
//! Structure access, sealed plastic wrap repackaging, and repackaging the contents
//! of non-ship containers are unsupported.
use std::{collections::BTreeMap, sync::Arc};

use crate::client::Client;
use crate::consts::dogma::EFFECT_ONLINE;
use crate::consts::{CATEGORY_MODULE, CATEGORY_SHIP, FLAG_HANGAR, FLAG_RIG_FIRST, FLAG_RIG_LAST, GROUP_STRATEGIC_CRUISER, IX_FLAG, IX_LOCATION_ID, IX_SINGLETON, TYPE_PLASTIC_WRAP};
use crate::dao::items::{charges, combat as combat_dao, dogma, fitting as fitting_dao, inventory as inventory_dao, loading::{Item, ItemQuery}, pending_reloads, ship};
use crate::db::outbox::Outbox;
use crate::messages;
use crate::services::{db_err, notify_error, user_error, Call, CallError, CallResult};
use crate::services::{combat, effects::online::{self, OnlineChanged, OnlinePolicy}, inventory, items::load_items};
use marshal::Value;

/// RepackageItems({stationID: [(itemID, locationID)]}, destroyable=).
async fn repackage(c: &mut Client, call: &Call<'_>) -> CallResult {
    let char_id = c.session.int("charid").unwrap_or(0);
    let active_ship = c.session.int("shipid");
    let destroyable = call.kwargs.get("destroyable").or_else(|| call.args.get(1))
        .is_some_and(|v| matches!(v, Value::Bool(true)) || v.as_int() == Some(1));
    let refuse = || notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION);
    let mut requested = BTreeMap::new();
    if let Some(Value::Dict(by_station)) = call.args.first() {
        for (station, items) in by_station {
            let station = station.as_int().ok_or_else(refuse)?;
            for pair in items.items().unwrap_or_default() {
                let pair = pair.items().unwrap_or_default();
                let id = pair.first().and_then(Value::as_int).ok_or_else(refuse)?;
                let location = pair.get(1).and_then(Value::as_int).ok_or_else(refuse)?;
                if Some(id) == active_ship { return Err(user_error(messages::MSG_CANT_REPACKAGE_ACTIVE_SHIP, vec![])); }
                if requested.insert(id, (location, station)).is_some_and(|previous| previous != (location, station)) {
                    return Err(refuse());
                }
            }
        }
    }
    let _lock = crate::services::market::lock().await;
    let db = c.db.clone();
    let sde = Arc::clone(&c.sde);
    let requested = Arc::new(requested);
    let notify = c.notify_tx.clone();
    let ships = db.transaction(
        move |tx, outbox: &mut Outbox<OnlineChanged>| {
            let requested = Arc::clone(&requested);
            let sde = Arc::clone(&sde);
            let notify = notify.clone();
            Box::pin(async move {
                let refuse = || notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION);
                for &id in requested.keys() {
                    if !ship::lock_reload_ship(tx, id).await.map_err(db_err)? { return Err(refuse()); }
                }
                let mut roots = Vec::<Item>::new();
                for (&id, &(location, station)) in requested.iter() {
                    let item = load_items(tx, &sde, ItemQuery::Id(id)).await.map_err(db_err)?.into_iter().next().ok_or_else(refuse)?;
                    if item.owner_id != char_id || item.location_id != station || item.location_id != location
                        || item.flag_id != FLAG_HANGAR || sde.map.station(station).is_none() {
                        return Err(refuse());
                    }
                    if item.singleton && item.type_id == TYPE_PLASTIC_WRAP { return Err(refuse()); }
                    if item.singleton { roots.push(item); }
                }
                let mut locked_children = BTreeMap::new();
                for root in &roots {
                    if root.category_id == i64::from(CATEGORY_SHIP) {
                        locked_children.insert(root.item_id, fitting_dao::lock_children(tx, root.item_id).await.map_err(db_err)?);
                    }
                }
                let root_ids: Vec<_> = roots.iter().map(|item| item.item_id).collect();
                let tree = crate::dao::trades::descendants(tx, &root_ids).await.map_err(db_err)?;
                if !crate::dao::trades::reserved(tx, &tree).await.map_err(db_err)?.is_empty() {
                    return Err(notify_error(messages::trade::TRADE_ITEM_RESERVED));
                }
                if crate::dao::industry::has_active_blueprint_job(tx, &tree).await.map_err(db_err)? {
                    return Err(notify_error(messages::industry::BLUEPRINT_ACTIVE));
                }
                let mut pending = BTreeMap::new();
                let mut destructive = false;
                for root in &roots {
                    if root.category_id != i64::from(CATEGORY_SHIP) { continue; }
                    let children = load_items(tx, &sde, ItemQuery::Location(root.item_id)).await.map_err(db_err)?;
                    if !children.iter().map(|item| item.item_id).eq(locked_children[&root.item_id].iter().copied()) { return Err(refuse()); }
                    if children.iter().any(|item| item.owner_id != char_id) { return Err(refuse()); }
                    let reservations = pending_reloads::for_ship(tx, root.item_id).await.map_err(db_err)?;
                    if reservations.iter().any(|row| row.owner_id != char_id) { return Err(refuse()); }
                    pending.insert(root.item_id, reservations.into_iter().map(|row| row.reload_id).collect::<Vec<_>>());
                    for charge in charges::all(tx, root.item_id).await.map_err(db_err)? {
                        if charge.quantity < 0 || (charge.quantity > 0 && !children.iter().any(|item| item.flag_id == charge.flag_id && inventory::is_fitting_flag(item.flag_id))) {
                            return Err(refuse());
                        }
                    }
                    if root.group_id != GROUP_STRATEGIC_CRUISER {
                        for rig in children.iter().filter(|item| (FLAG_RIG_FIRST..=FLAG_RIG_LAST).contains(&item.flag_id)) {
                            destructive = true;
                            if crate::dao::trades::descendants(tx, &[rig.item_id]).await.map_err(db_err)?.len() != 1 { return Err(refuse()); }
                        }
                    }
                }
                if destructive && !destroyable {
                    return Err(CallError::User { msg: messages::MSG_CONFIRM_REPACKAGE_SOMETHING_WITH_UPGRADES.into(), dict: vec![] });
                }
                let mut ships = Vec::new();
                for root in &roots {
                    if root.category_id == i64::from(CATEGORY_SHIP) {
                        let id = root.item_id;
                        combat::reload_recovery::return_pending(tx, &sde, outbox, &pending[&id], true).await?;
                        let locked = fitting_dao::lock_children(tx, id).await.map_err(db_err)?;
                        let children = load_items(tx, &sde, ItemQuery::Location(id)).await.map_err(db_err)?;
                        if !children.iter().map(|item| item.item_id).eq(locked.iter().copied()) { return Err(refuse()); }
                        if children.iter().any(|item| item.owner_id != char_id) { return Err(refuse()); }
                        let policy = OnlinePolicy { owner: char_id, active_ship, station: Some(root.location_id), notify: notify.clone() };
                        let loaded = charges::all(tx, id).await.map_err(db_err)?;
                        for item in &children {
                            if inventory::is_fitting_flag(item.flag_id) && loaded.iter().any(|charge| charge.flag_id == item.flag_id && charge.quantity > 0) {
                                combat::targeting_charges::unload(tx, &sde, outbox, char_id, id, item, root.location_id, FLAG_HANGAR).await?;
                            }
                        }
                        for item in children {
                            if inventory::is_fitting_flag(item.flag_id) && item.category_id == i64::from(CATEGORY_MODULE)
                                && sde.types.effects(item.type_id).iter().any(|effect| i64::from(effect.effect_id) == EFFECT_ONLINE) {
                                online::set_online_core(tx, &sde, &policy, outbox, id, item.item_id, false).await?;
                            }
                            if root.group_id != GROUP_STRATEGIC_CRUISER && (FLAG_RIG_FIRST..=FLAG_RIG_LAST).contains(&item.flag_id) {
                                if !fitting_dao::destroy_upgrade(tx, &item).await.map_err(db_err)? { return Err(refuse()); }
                                inventory::notify_item_removed(outbox, item);
                            } else {
                                if !inventory_dao::move_item(tx, &item, root.location_id, FLAG_HANGAR, char_id, item.singleton).await.map_err(db_err)? { return Err(refuse()); }
                                inventory::notify_item_change(tx, &sde, outbox, item.item_id, vec![(IX_LOCATION_ID, Value::Int(item.location_id)), (IX_FLAG, Value::Int(i64::from(item.flag_id)))]).await?;
                                outbox.event(crate::lua::events::GameEvent::ItemsMoved { char_id, item_id: item.item_id, from_location_id: id, to_location_id: root.location_id, quantity: item.quantity });
                            }
                        }
                        for charge in charges::all(tx, id).await.map_err(db_err)? {
                            if charge.quantity != 0 { return Err(refuse()); }
                            combat_dao::delete_empty_charge(tx, id, charge.flag_id).await.map_err(db_err)?;
                        }
                        if combat_dao::clear_banks(tx, id).await.map_err(db_err)? != 0 {
                            outbox.notify_client("OnWeaponBanksChanged", vec![Value::Int(id), combat::banks(tx, id).await?]);
                        }
                        crate::dao::insurance::uninsure(tx, id, char_id).await.map_err(db_err)?;
                        if !fitting_dao::lock_children(tx, id).await.map_err(db_err)?.is_empty() { return Err(refuse()); }
                        ships.push(id);
                    }
                    if fitting_dao::repackage(tx, root).await.map_err(db_err)? == 0 { return Err(refuse()); }
                    dogma::clear_overrides(tx, root.item_id).await.map_err(db_err)?;
                    inventory::notify_item_change(tx, &sde, outbox, root.item_id, vec![(IX_SINGLETON, Value::Bool(true))]).await?;
                }
                Ok(ships)
            })
        },
        |value, outbox| async move { online::dispatch_changed(value, outbox, c) },
    ).await?;
    for ship in ships { combat::forget_ship(ship); }
    Ok(Value::None)
}

pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    match call.method { "RepackageItems" => Some(repackage(c, call).await), _ => None }
}

#[cfg(test)] mod tests;
