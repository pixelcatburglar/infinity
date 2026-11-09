//! Shared persisted repair: one damage writer used by the paid repairSvc settlement
//! and by the GM `/heal` docked path. There is no direct client mirror; callers own
//! authorization, prices, transactions and the dispatch of queued notifications.
//! Condition lives in game.item_attributes through the dogma checkpoint primitives;
//! item context comes from game.items and fitted values from `services::fit`.
//! Only hull/armor damage flows through write_damage; ship shield, armor and hull
//! condition is written as one `save_conditions` snapshot so the shield checkpoint
//! timestamp stays consistent. In-space condition, capacitor and heat repair are not
//! covered here: a docked ship has no actor state and its capacitor and rack heat are
//! already full/zero in the dogma projection, so those fields are never written.
//! Online state, charges, weapon groups, cargo and wallets are never touched.
use std::sync::Arc;
use infinity_core::consts::{CATEGORY_SHIP, FLAG_HANGAR};
use infinity_core::consts::dogma::{ATTR_ARMOR_DAMAGE, ATTR_HULL_DAMAGE, ATTR_SHIELD_CAPACITY, ATTR_SHIELD_CHARGE};
use infinity_core::messages;
use marshal::Value;
use crate::client::filetime_now;
use crate::dao::characters::{selection, ship as character_ship};
use crate::dao::items::{dogma, loading::{ItemQuery, GameItem}, parent_locks::{LockRequest, LockStrength}};
use crate::db::{self, executor::Executor, outbox::{self, Outbox}};
use crate::sde::Sde;
use crate::services::{db_err, effects::notify::attribute_change, fit::{Fit, Key}, identity::Identity, inventory::{self, destination}, market, notify_error, CallError};
use crate::space::World;

/// Persist one armor or hull damage value on a locked singleton and build its
/// `OnModuleAttributeChanges` entry. `None` means the target row was lost; the
/// caller turns that into its own refusal. Other attributes, online state and
/// pricing are never written through this path.
pub(super) async fn write_damage(
    ex: &(impl Executor + ?Sized), owner: i64, item: i64, attribute: i32, old: f64, new: f64,
) -> Result<Option<Value>, CallError> {
    Ok(dogma::set_override(ex, item, attribute, new).await.map_err(db_err)?
        .then(|| attribute_change(owner, Value::Int(item), attribute, Value::Float(new), Value::Float(old))))
}

fn refuse() -> CallError { notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION) }
fn no_ship() -> CallError { notify_error(messages::gm::GM_DOGMA_NO_SHIP) }
fn not_docked() -> CallError { notify_error(messages::gm::GM_SHIP_DOCKED_REQUIRED) }

/// Project the loaded rows onto the repair set: the ship row plus direct children
/// in fitting slots. Malformed members refuse instead of being silently skipped;
/// cargo and other non-fitted contents never join the set.
fn targets(sde: &Sde, items: &mut Vec<GameItem>, ship: i64, owner: i64, station: i64) -> Result<(), CallError> {
    items.retain(|item| item.item_id == ship
        || (item.location_id == ship && inventory::is_fitting_flag(item.flag_id)));
    let mut found = false;
    for item in items.iter() {
        if item.item_id == ship {
            if !item.singleton || item.owner_id != owner || item.location_id != station || item.flag_id != FLAG_HANGAR
                || sde.types.get(item.type_id).and_then(|t| t.category_id) != Some(CATEGORY_SHIP) { return Err(refuse()); }
            found = true;
        } else if !item.singleton || item.owner_id != owner || sde.types.get(item.type_id).is_none() { return Err(refuse()); }
    }
    if !found { return Err(refuse()); }
    Ok(())
}

fn same_snapshot(a: &GameItem, b: &crate::dao::items::parent_locks::Parent) -> bool {
    a.item_id == b.item_id && a.type_id == b.type_id && a.owner_id == b.owner_id
        && a.location_id == b.location_id && a.flag_id == b.flag_id
        && a.quantity == b.quantity && a.singleton == b.singleton
}

fn same_item(a: &GameItem, b: &GameItem) -> bool {
    a.item_id == b.item_id && a.type_id == b.type_id && a.owner_id == b.owner_id
        && a.location_id == b.location_id && a.flag_id == b.flag_id
        && a.quantity == b.quantity && a.singleton == b.singleton
}

/// Repair the selected ship and every fitted module of a docked character in one
/// transaction; the world is only used to dispatch queued notifications after
/// COMMIT. Authorization stays with the caller (the GM slash gate).
pub(super) async fn heal_docked(
    identity: &Identity, db: &db::Db, world: &World, sde: &Arc<Sde>,
) -> Result<(), CallError> {
    if identity.char_id <= 0 { return Err(no_ship()); }
    identity.station_id.filter(|station| *station > 0).ok_or_else(not_docked)?;
    identity.ship_id.filter(|ship| *ship > 0).ok_or_else(no_ship)?;
    let identity = *identity;
    let _lock = market::lock().await;
    db.transaction(
        move |tx, outbox| {
            let sde = sde.clone();
            Box::pin(async move { heal_docked_in(tx, outbox, identity, sde).await })
        },
        |(), outbox| async move { outbox::dispatch_world((), outbox, world) },
    ).await
}

async fn heal_docked_in(
    ex: &(impl Executor + ?Sized), outbox: &mut Outbox, identity: Identity, sde: Arc<Sde>,
) -> Result<(), CallError> {
    let station = identity.station_id.filter(|station| *station > 0).ok_or_else(not_docked)?;
    let ship = identity.ship_id.filter(|ship| *ship > 0 && identity.char_id > 0).ok_or_else(no_ship)?;
    let owner = identity.char_id;
    selection::lock_character(ex, owner).await.map_err(db_err)?;
    let current = character_ship::current(ex, owner).await.map_err(db_err)?.ok_or_else(refuse)?;
    if current.ship != Some(ship) || current.station != Some(station) { return Err(refuse()); }
    // Ship plus fitted children only; other direct contents are read but never locked.
    let mut loaded = crate::dao::items::loading::load_items(ex, ItemQuery::IdOrLocation(ship)).await.map_err(db_err)?;
    targets(&sde, &mut loaded, ship, owner, station)?;
    let before = loaded;
    let requests: Vec<_> = before.iter().map(|item| LockRequest { item_id: item.item_id, strength: LockStrength::Update }).collect();
    let locks = destination::lock_items(ex, &sde, &requests, &[]).await?;
    if before.iter().any(|item| locks.row(item.item_id).is_none_or(|row| row.jump_clone_id.is_some() || !same_snapshot(item, row))) {
        return Err(refuse());
    }
    let mut reloaded = crate::dao::items::loading::load_items(ex, ItemQuery::IdOrLocation(ship)).await.map_err(db_err)?;
    targets(&sde, &mut reloaded, ship, owner, station)?;
    if reloaded.len() != before.len() || reloaded.iter().zip(&before).any(|(a, b)| !same_item(a, b)) {
        return Err(refuse());
    }

    let fit = Fit::load(ex, sde, owner, ship).await?;
    let (old_shield, old_armor, old_hull) = crate::services::combat::prepare_condition_with_fit(ex, ship, &fit).await?.scalars();
    let shield_max = fit.value(Key::Ship, ATTR_SHIELD_CAPACITY);
    let mut changed = Vec::new();
    // `before` is already in item-ID order from the load query.
    for item_id in before.iter().map(|item| item.item_id) {
        if item_id == ship {
            let saved = dogma::save_conditions(ex, station,
                &[ATTR_SHIELD_CHARGE, ATTR_ARMOR_DAMAGE, ATTR_HULL_DAMAGE],
                &[(ship, [shield_max, 0.0, 0.0], filetime_now())]).await.map_err(db_err)?;
            if saved != [ship] { return Err(refuse()); }
            for (attribute, new, old) in [
                (ATTR_SHIELD_CHARGE, shield_max, old_shield),
                (ATTR_ARMOR_DAMAGE, 0.0, old_armor),
                (ATTR_HULL_DAMAGE, 0.0, old_hull),
            ] {
                if new != old { changed.push(attribute_change(owner, Value::Int(ship), attribute, Value::Float(new), Value::Float(old))); }
            }
        } else {
            let old = fit.overrides(Key::Module(item_id)).iter().find(|entry| entry.0 == ATTR_HULL_DAMAGE).map_or(0.0, |entry| entry.1);
            if old == 0.0 { continue; }
            let Some(entry) = write_damage(ex, owner, item_id, ATTR_HULL_DAMAGE, old, 0.0).await? else { return Err(refuse()); };
            changed.push(entry);
        }
    }
    if !changed.is_empty() {
        outbox.notify_pilot(owner, "OnModuleAttributeChanges", vec![Value::list(changed)]);
    }
    Ok(())
}

#[cfg(test)] mod tests;
