//! Atomic GM fitting using the same item move and slot selection as invInventory.
//! The slash command supplies a resolved type and optional item ID; game.items
//! supplies the target's ship and inventory. Modules are not brought online.

use super::*;
use crate::db::outbox::Notification;

pub(super) fn available(item: &Item, target: i64, type_id: i32, ship: i64, station: Option<i64>) -> bool {
    item.owner_id == target && item.type_id == type_id && item.quantity > 0
        && ((item.location_id == ship && item.flag_id == FLAG_CARGO)
            || (station == Some(item.location_id) && item.flag_id == FLAG_HANGAR))
}

/// Fit onto the target's active ship, taking owned cargo/hangar items before creating stock.
pub(in crate::services) async fn gm_fit(c: &mut Client, target: i64, type_id: i32, item_id: Option<i64>, quantity: i32) -> Result<usize, CallError> {
    let _lock = super::super::market::lock().await;
    let refuse = || notify_error(messages::gm::GM_FIT_REFUSED);
    if quantity <= 0 { return Err(refuse()); }
    let ship = crate::dao::characters::selection::ship_id(&c.db, target).await.map_err(db_err)?.ok_or_else(refuse)?;
    let station = crate::dao::items::transfers::docked(&c.db, target).await.map_err(db_err)?;
    let db = c.db.clone();
    let sde = c.sde.clone();
    db.transaction(
        move |tx, outbox: &mut Outbox<MoveEffect>| {
            let sde = sde.clone();
            Box::pin(async move {
                let refuse = || notify_error(messages::gm::GM_FIT_REFUSED);
                if crate::dao::characters::selection::ship_id(tx, target).await.map_err(db_err)? != Some(ship)
                    || crate::dao::items::transfers::docked(tx, target).await.map_err(db_err)? != station {
                    return Err(refuse());
                }
                if !crate::dao::items::ship::lock_reload_ship(tx, ship).await.map_err(db_err)? { return Err(refuse()); }
                let hull = load_items(tx, &sde, ItemQuery::Id(ship)).await.map_err(db_err)?.into_iter().next().ok_or_else(refuse)?;
                if hull.owner_id != target || hull.category_id != CATEGORY_SHIP as i64 { return Err(refuse()); }
                let source_location = station.unwrap_or(ship);
                let source_flag = if station.is_some() { FLAG_HANGAR } else { FLAG_CARGO };
                let mut sources = if let Some(id) = item_id {
                    load_items(tx, &sde, ItemQuery::Id(id)).await.map_err(db_err)?
                } else {
                    load_items(tx, &sde, ItemQuery::OwnerTypeTwoLocations(target, type_id, ship, FLAG_CARGO, source_location, source_flag)).await.map_err(db_err)?
                };
                sources.retain(|item| available(item, target, type_id, ship, station));
                if item_id.is_some() && sources.iter().map(|item| i64::from(item.quantity)).sum::<i64>() < i64::from(quantity) { return Err(refuse()); }
                for _ in 0..quantity {
                    let item = if let Some(item) = sources.first().cloned() {
                        item
                    } else {
                        let id = crate::dao::items::transfers::insert(tx, type_id, target, source_location, source_flag, 1).await.map_err(db_err)?;
                        notify_item_change(tx, &sde, outbox, id, vec![(IX_LOCATION_ID, Value::Int(0))]).await?;
                        load_items(tx, &sde, ItemQuery::Id(id)).await.map_err(db_err)?.into_iter().next().ok_or_else(refuse)?
                    };
                    let flag = super::super::effects::fitting::free_slot(tx, sde.clone(), target, ship, item.item_id, type_id).await?.ok_or_else(|| notify_error(messages::fitting::SLOTS))?;
                    let take = (!item.singleton && item.quantity > 1).then_some(1);
                    let moved = move_core(tx, &sde, outbox, &item, target, ship, flag, take).await?;
                    super::super::effects::fitting::validate_fit(tx, sde.clone(), target, ship, moved).await?;
                    super::super::effects::online::reconcile_core(tx, &sde, outbox, target, ship).await?;
                    if !sources.is_empty() {
                        if take.is_some() { sources[0].quantity -= 1; } else { sources.remove(0); }
                    }
                }
                Ok(quantity as usize)
            })
        },
        |value, outbox| async move { dispatch_move(value, outbox, c, target) },
    ).await
}

pub(super) fn route_target(notification: Notification, target: i64, current: Option<i64>) -> Notification {
    match notification {
        Notification::DirectClient { event, args } if current != Some(target) => Notification::Pilot { char_id: target, event, args },
        notification => notification,
    }
}


/// Unload selected modules and their bank's charges in one transaction.
pub(in crate::services) async fn gm_unload(c: &mut Client, ship: i64, owner: i64, mut ids: Vec<i64>, destination: i64, flag: i32) -> Result<usize, CallError> {
    use super::super::effects::online::{dispatch_changed, set_online_core, OnlineChanged, OnlinePolicy};
    let _lock = super::super::market::lock().await;
    let policy = OnlinePolicy::from_client(c)?;
    if policy.owner != owner { return Err(notify_error(messages::gm::GM_FIT_REFUSED)); }
    ids.sort_unstable();
    ids.dedup();
    let db = c.db.clone();
    let sde = c.sde.clone();
    db.transaction(
        move |tx, outbox: &mut Outbox<OnlineChanged>| {
            let sde = sde.clone();
            let ids = ids.clone();
            let policy = policy.clone();
            Box::pin(async move {
                let refuse = || notify_error(messages::gm::GM_FIT_REFUSED);
                if !crate::dao::items::ship::lock_reload_ship(tx, ship).await.map_err(db_err)?
                    || crate::dao::characters::selection::ship_id(tx, owner).await.map_err(db_err)? != Some(ship) {
                    return Err(refuse());
                }
                let station = crate::dao::items::transfers::docked(tx, owner).await.map_err(db_err)?;
                if (destination, flag) != station.map(|s| (s, FLAG_HANGAR)).unwrap_or((ship, FLAG_CARGO)) {
                    return Err(refuse());
                }
                let hull = load_items(tx, &sde, ItemQuery::Id(ship)).await.map_err(db_err)?.into_iter().next().ok_or_else(refuse)?;
                if hull.owner_id != owner || hull.category_id != CATEGORY_SHIP as i64 { return Err(refuse()); }
                let modules = load_items(tx, &sde, ItemQuery::Ids(&ids)).await.map_err(db_err)?;
                if modules.len() != ids.len() || modules.iter().any(|m| m.owner_id != owner || m.location_id != ship || !is_fitting_flag(m.flag_id)) {
                    return Err(refuse());
                }
                let mut charges = std::collections::BTreeSet::new();
                for module in &modules {
                    let master = super::super::combat::targeting_charges::master_for_member(tx, ship, module.item_id).await?;
                    charges.extend(super::super::combat::targeting_charges::bank(tx, ship, master).await?);
                }
                let charge_ids: Vec<i64> = charges.into_iter().collect();
                let charge_modules = load_items(tx, &sde, ItemQuery::Ids(&charge_ids)).await.map_err(db_err)?;
                if charge_modules.len() != charge_ids.len() || charge_modules.iter().any(|m| m.owner_id != owner || m.location_id != ship || !is_fitting_flag(m.flag_id)) {
                    return Err(refuse());
                }
                let flags: Vec<i32> = charge_modules.iter().map(|m| m.flag_id).collect();
                if !crate::dao::items::pending_reloads::for_flags(tx, ship, &flags).await.map_err(db_err)?.is_empty() {
                    return Err(refuse());
                }
                for module in &modules {
                    if super::super::effects::lifecycle::has_effect(sde.types.effects(module.type_id), crate::consts::EFFECT_ONLINE as i32) {
                        set_online_core(tx, &sde, &policy, outbox, ship, module.item_id, false).await?;
                    }
                }
                for module in charge_modules {
                    super::super::combat::targeting_charges::unload(tx, &sde, outbox, owner, ship, &module, destination, flag).await?;
                }
                for module in &modules {
                    move_core(tx, &sde, outbox, module, owner, destination, flag, None).await?;
                }
                super::super::effects::online::reconcile_core(tx, &sde, outbox, owner, ship).await?;
                Ok(modules.len())
            })
        },
        |value, outbox| async move { dispatch_changed(value, outbox, c) },
    ).await
}
