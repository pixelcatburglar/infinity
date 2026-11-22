//! Shared persisted repair for the paid repairSvc settlement and GM `/heal`.
//! There is no direct client mirror; callers own authorization and pricing.
//! Item damage uses game.item_attributes and game.items; fitted values use Fit.
//! Docked condition is saved as a snapshot; space condition uses the actor and
//! its existing checkpoints. Capacitor and rack heat in space remain in memory.
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

/// The flag persisted by the ship undock operation for a ship in space.
const SPACE_SHIP_FLAG: i32 = 0;

pub(super) enum SpaceHealOutcome { Complete, ModulesOnly }

struct SpaceRepair {
    patch: crate::services::fit::attributes::CommittedFitting,
    module_changes: Vec<Value>,
}
crate::progress::leaf_effect!(SpaceRepair);

pub(super) async fn heal_space(
    expected: &infinity_core::session::OnlineSession, db: &db::Db, world: &World, sde: &Arc<Sde>,
) -> Result<SpaceHealOutcome, CallError> {
    let system = expected.system_id.filter(|id| *id > 0).ok_or_else(no_ship)?;
    expected.ship_id.filter(|id| *id > 0).ok_or_else(no_ship)?;
    if expected.character_id <= 0 || expected.station_id.is_some() { return Err(no_ship()); }
    let captured = expected.clone();
    let (generation, gate) = world.with(system, move |s| s.capture_space_heal(&captured))
        .await.flatten().ok_or_else(refuse)?;
    let operation_expected = expected.clone();
    let dispatch_expected = expected.clone();
    let ack = {
        let _market = market::lock().await;
        db.transaction(
            move |tx, outbox| {
                let expected = operation_expected.clone();
                let sde = sde.clone();
                Box::pin(async move { heal_space_in(tx, outbox, &expected, &sde).await })
            },
            |(), outbox| std::future::ready(Ok(dispatch_space_heal(world, dispatch_expected, generation, gate, outbox))),
        ).await?
    };
    Ok(if matches!(ack.await, Ok(true)) { SpaceHealOutcome::Complete } else { SpaceHealOutcome::ModulesOnly })
}

async fn heal_space_in(
    ex: &(impl Executor + ?Sized), outbox: &mut Outbox<SpaceRepair>,
    expected: &infinity_core::session::OnlineSession, sde: &Sde,
) -> Result<(), CallError> {
    let owner = expected.character_id;
    let ship = expected.ship_id.filter(|id| *id > 0 && owner > 0).ok_or_else(no_ship)?;
    let system = expected.system_id.filter(|id| *id > 0).ok_or_else(no_ship)?;
    if expected.station_id.is_some() { return Err(refuse()); }
    selection::lock_character(ex, owner).await.map_err(db_err)?;
    let current = character_ship::current(ex, owner).await.map_err(db_err)?.ok_or_else(refuse)?;
    if current.ship != Some(ship) || current.system != Some(system) || current.station.is_some() { return Err(refuse()); }
    let items = locked_targets(ex, sde, ship, owner, system, SPACE_SHIP_FLAG).await?;
    let modules: Vec<_> = items.iter().filter(|item| item.item_id != ship).map(|item| item.item_id).collect();
    let overrides = if modules.is_empty() { Vec::new() } else { dogma::overrides_for(ex, &modules).await.map_err(db_err)? };
    let mut damage = overrides.into_iter().filter(|row| row.attribute_id == ATTR_HULL_DAMAGE).peekable();
    let mut module_changes = Vec::new();
    for &item in &modules {
        let old = if damage.peek().is_some_and(|row| row.item_id == item) { damage.next().unwrap().value } else { 0.0 };
        if old != 0.0 {
            module_changes.push(write_damage(ex, owner, item, ATTR_HULL_DAMAGE, old, 0.0).await?.ok_or_else(refuse)?);
        }
    }
    outbox.operation(SpaceRepair {
        patch: crate::services::fit::attributes::CommittedFitting::repaired_modules(modules.into_iter()),
        module_changes,
    });
    Ok(())
}

fn dispatch_space_heal(
    world: &World, expected: infinity_core::session::OnlineSession, generation: u64,
    actor_gate: Arc<tokio::sync::Mutex<()>>, outbox: Outbox<SpaceRepair>,
) -> tokio::sync::oneshot::Receiver<bool> {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let mut entries = outbox.into_entries().into_iter();
    let Some(outbox::Effect::Operation(repair)) = entries.next() else { unreachable!("space repair operation missing") };
    assert!(entries.next().is_none(), "unexpected space repair effect");
    let system = expected.system_id.expect("validated space system");
    world.post(system, move |s| {
        let applied = s.publish_space_heal(expected, generation, actor_gate, repair.patch, repair.module_changes);
        let _ = sender.send(applied);
    });
    receiver
}

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

/// Client inventoryUtil.GetComputedBasePrice(typeID, 1), including its material portion rule.
pub(in crate::services) fn computed_base_price(sde: &Sde, id: i32) -> f64 {
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

fn refuse() -> CallError { notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION) }
fn no_ship() -> CallError { notify_error(messages::gm::GM_DOGMA_NO_SHIP) }
fn not_docked() -> CallError { notify_error(messages::gm::GM_SHIP_DOCKED_REQUIRED) }

/// Project the loaded rows onto the repair set: the ship row plus direct children
/// in fitting slots. Malformed members refuse instead of being silently skipped;
/// cargo and other non-fitted contents never join the set.
fn targets(sde: &Sde, items: &mut Vec<GameItem>, ship: i64, owner: i64, location: i64, flag: i32) -> Result<(), CallError> {
    items.retain(|item| item.item_id == ship
        || (item.location_id == ship && inventory::is_fitting_flag(item.flag_id)));
    let mut found = false;
    for item in items.iter() {
        if item.item_id == ship {
            if !item.singleton || item.owner_id != owner || item.location_id != location || item.flag_id != flag
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

async fn locked_targets(
    ex: &(impl Executor + ?Sized), sde: &Sde, ship: i64, owner: i64, location: i64, flag: i32,
) -> Result<Vec<GameItem>, CallError> {
    let mut before = crate::dao::items::loading::load_items(ex, ItemQuery::IdOrLocation(ship)).await.map_err(db_err)?;
    targets(sde, &mut before, ship, owner, location, flag)?;
    let requests: Vec<_> = before.iter().map(|item| LockRequest { item_id: item.item_id, strength: LockStrength::Update }).collect();
    let locks = destination::lock_items(ex, sde, &requests, &[]).await?;
    if before.iter().any(|item| locks.row(item.item_id).is_none_or(|row| row.jump_clone_id.is_some() || !same_snapshot(item, row))) {
        return Err(refuse());
    }
    let mut reloaded = crate::dao::items::loading::load_items(ex, ItemQuery::IdOrLocation(ship)).await.map_err(db_err)?;
    targets(sde, &mut reloaded, ship, owner, location, flag)?;
    if reloaded.len() != before.len() || reloaded.iter().zip(&before).any(|(a, b)| !same_item(a, b)) {
        return Err(refuse());
    }
    Ok(before)
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
    let before = locked_targets(ex, &sde, ship, owner, station, FLAG_HANGAR).await?;

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

#[cfg(all(test, any(feature = "tests-effects", feature = "tests-ships")))] pub(in crate::services) mod tests;
