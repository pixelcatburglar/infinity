//! Atomic station clone transitions for jumpCloneSvc. Real implant rows and their
//! attributes stay with their bodies; boosters cannot enter dormant custody.
//! Uses game characters, items, clones, skills and wallet through their DAOs.
//! Clone bays in ships and death transitions are outside this mechanism.
use std::{collections::{HashMap, HashSet}, sync::Arc};
use marshal::Value;
use super::{lifecycle, policy, CloneEffect, Transition};
use super::super::{db_err, notify_error, user_error, CallError};
use infinity_core::{consts::{dogma::{ATTR_FOLLOWS_JUMP_CLONES, ATTR_IMPLANTNESS}, FLAG_BOOSTER, FLAG_HANGAR, FLAG_IMPLANT, TYPE_CAPSULE}, messages};
use crate::{dao::{characters::clones::{self as clones, CloneRow}, items::{brain::{self, BrainItem}, clones as custody, creation, dogma, fit, loading::ItemQuery, ship}}, db::{executor::Executor, outbox::Outbox}, sde::Sde, services::{effects::online::{self, OnlineChanged}, identity::Identity, inventory::notifications::notify_item_removed, items::load_items, skills::body_change, trade, wallets}};

fn stale() -> CallError { notify_error(messages::clones::STALE_BODY) }
fn invalid_slots() -> CallError { notify_error(messages::clones::INVALID_IMPLANT_SLOTS) }

fn attribute(sde: &Sde, item: &BrainItem, overrides: &HashMap<(i64, i32), f64>, attr: i32) -> Option<f64> {
    overrides.get(&(item.item_id, attr)).copied().or_else(|| sde.types.attribute(item.type_id, attr))
}

fn implant_slot(sde: &Sde, item: &BrainItem, overrides: &HashMap<(i64, i32), f64>) -> Result<i32, CallError> {
    if let Some(slot) = item.slot { return if slot > 0 { Ok(slot) } else { Err(invalid_slots()) }; }
    let value = attribute(sde, item, overrides, ATTR_IMPLANTNESS).ok_or_else(invalid_slots)?;
    if !value.is_finite() || value <= 0.0 || value.fract() != 0.0 || value > i32::MAX as f64 { return Err(invalid_slots()); }
    Ok(value as i32)
}

fn follows(sde: &Sde, item: &BrainItem, overrides: &HashMap<(i64, i32), f64>) -> Result<bool, CallError> {
    let value = attribute(sde, item, overrides, ATTR_FOLLOWS_JUMP_CLONES).unwrap_or(0.0);
    if !value.is_finite() { return Err(invalid_slots()); }
    Ok(value != 0.0)
}

/// Caller holds character, clone and implant locks and has created an empty source.
pub(super) async fn swap_body_into_source_tx<E: From<OnlineChanged> + Send>(ex: &(impl Executor + ?Sized), outbox: &mut Outbox<E>, sde: &Arc<Sde>, fences: &online::OnlineFences, identity: &Identity, target: &CloneRow, source: &CloneRow, now: i64, change: body_change::BodyChange) -> Result<(), CallError> {
    let owner = identity.char_id;
    if target.owner_id != owner || source.owner_id != owner || target.clone_id == source.clone_id { return Err(stale()); }
    let active = brain::list(ex, owner).await.map_err(db_err)?;
    let dormant = custody::list(ex, owner, target.clone_id).await.map_err(db_err)?;
    let ids: Vec<_> = active.iter().map(|item| item.item_id).collect();
    let mut attrs: HashMap<_, _> = dogma::overrides_for(ex, &ids).await.map_err(db_err)?.into_iter().map(|a| ((a.item_id, a.attribute_id), a.value)).collect();
    let ids: Vec<_> = dormant.iter().map(|item| item.item_id).collect();
    attrs.extend(custody::overrides(ex, owner, target.clone_id, &ids).await.map_err(db_err)?.into_iter().map(|a| ((a.item_id, a.attribute_id), a.value)));
    let mut active_slots = HashSet::new();
    let mut destination_slots = HashSet::new();
    let mut park = Vec::new();
    let mut boosters = Vec::new();
    for item in &active {
        if item.flag_id == FLAG_IMPLANT {
            let slot = implant_slot(sde, item, &attrs)?;
            if !active_slots.insert(slot) { return Err(invalid_slots()); }
            if follows(sde, item, &attrs)? { destination_slots.insert(slot); } else { park.push((item, slot)); }
        } else if item.flag_id == FLAG_BOOSTER && !follows(sde, item, &attrs)? { boosters.push(item); }
    }
    for item in &dormant {
        if item.flag_id != FLAG_IMPLANT || !destination_slots.insert(implant_slot(sde, item, &attrs)?) { return Err(invalid_slots()); }
    }
    for (item, slot) in &park {
        if item.slot.is_none() && !brain::update_metadata(ex, owner, item, Some(*slot), item.activated_at, item.expires_at, item.effect_ids.as_deref()).await.map_err(db_err)? { return Err(stale()); }
        if !custody::park(ex, owner, source.clone_id, item.item_id).await.map_err(db_err)? { return Err(stale()); }
    }
    for item in &dormant {
        if !custody::activate(ex, owner, target.clone_id, item.item_id).await.map_err(db_err)? { return Err(stale()); }
    }
    for item in &boosters {
        let row = load_items(ex, sde, ItemQuery::Id(item.item_id)).await.map_err(db_err)?.into_iter().next().ok_or_else(stale)?;
        if !brain::remove(ex, owner, item).await.map_err(db_err)? { return Err(stale()); }
        notify_item_removed(outbox, row);
    }
    body_change::finish_tx(ex, outbox, sde, fences, owner, now, change, !park.is_empty() || !dormant.is_empty(), !boosters.is_empty()).await
}

/// The caller holds the market lock until all committed effects have been dispatched.
pub(super) async fn jump_tx(ex: &(impl Executor + ?Sized), outbox: &mut Outbox<CloneEffect>, sde: &Arc<Sde>, fences: &online::OnlineFences, id: &Identity, dest: i64, clone_id: i64, quote: f64, confirmed: bool, now: i64) -> Result<Transition, CallError> {
    let owner = id.char_id;
    let source_station = id.station_id.ok_or_else(stale)?;
    let old_ship = id.ship_id.ok_or_else(stale)?;
    if owner <= 0 || dest <= 0 || clone_id <= 0 { return Err(stale()); }
    let fee = policy::service_fee();
    if !quote.is_finite() || quote < 0.0 || quote != fee { return Err(notify_error(messages::INVALID_PRICE)); }
    let station = sde.map.station(dest).ok_or_else(stale)?;
    let system = station.solar_system_id.ok_or_else(stale)?;
    let constellation = station.constellation_id.ok_or_else(stale)?;
    let region = station.region_id.ok_or_else(stale)?;
    let mut hulls = fit::owner_online_ships(ex, owner).await.map_err(db_err)?;
    hulls.push(old_ship);
    hulls.sort_unstable(); hulls.dedup();
    for hull in hulls { if !ship::lock_reload_ship(ex, hull).await.map_err(db_err)? { return Err(stale()); } }
    let character = clones::lock_character(ex, owner).await.map_err(db_err)?.ok_or_else(stale)?;
    if character.ship_id != Some(old_ship) || character.station_id != Some(source_station) || sde.map.station(source_station).is_none() { return Err(stale()); }
    let capsule = load_items(ex, sde, ItemQuery::Id(old_ship)).await.map_err(db_err)?.into_iter().next().ok_or_else(stale)?;
    if capsule.owner_id != owner || capsule.type_id != TYPE_CAPSULE || capsule.location_id != source_station || !capsule.singleton { return Err(stale()); }
    let rows = clones::lock_clones(ex, owner).await.map_err(db_err)?;
    let target = rows.iter().find(|row| row.clone_id == clone_id && row.location_id == dest).ok_or_else(|| notify_error(messages::JUMP_CLONE_NOT_FOUND))?;
    let conflict = rows.iter().find(|row| row.location_id == source_station && row.clone_id != target.clone_id);
    policy::check_station_access_tx(ex, sde, owner, source_station).await?;
    policy::check_station_access_tx(ex, sde, owner, dest).await?;
    custody::lock(ex, owner).await.map_err(db_err)?;
    if conflict.is_some() && !confirmed { return Err(user_error(messages::clones::MSG_JUMP_CHECK_WILL_LOSE_EXISTING_CLONE, vec![])); }
    let change = body_change::begin_tx(ex, outbox, sde, owner, now).await?;
    policy::check_cooldown_tx(ex, sde, owner, now, character.last_clone_jump, source_station == dest).await?;
    wallets::debit(ex, outbox, owner, crate::isk::IskAmount::from_isk(fee)?).await?;
    let mut trade_effects = Outbox::default();
    trade::abort_for_char(ex, sde, &mut trade_effects, owner).await?;
    outbox.operation(CloneEffect::Trade(trade_effects));
    if let Some(conflict) = conflict { lifecycle::destroy_clone_and_implants_tx(ex, outbox, owner, conflict).await?; }
    clones::defer_location(ex).await.map_err(db_err)?;
    let source = lifecycle::create_empty_clone_tx(ex, owner, source_station).await?;
    swap_body_into_source_tx(ex, outbox, sde, fences, id, target, &source, now, change).await?;
    if !custody::list(ex, owner, target.clone_id).await.map_err(db_err)?.is_empty() || clones::destroy(ex, target.clone_id, owner).await.map_err(db_err)? != 1 { return Err(stale()); }
    clones::restore_location(ex).await.map_err(db_err)?;
    if custody::delete_capsule(ex, owner, old_ship, TYPE_CAPSULE).await.map_err(db_err)? != 1 { return Err(stale()); }
    let capsule = creation::rookie_ship(ex, TYPE_CAPSULE, owner, dest, FLAG_HANGAR).await.map_err(db_err)?;
    if clones::jump(ex, owner, capsule, dest, system, constellation, region, old_ship, source_station, (source_station != dest).then_some(now)).await.map_err(db_err)? != 1 { return Err(stale()); }
    outbox.notify_pilot(owner, "OnJumpCloneCacheInvalidated", vec![]);
    outbox.notify_pilot(owner, "OnStationJumpCloneCacheInvalidated", vec![Value::Int(source_station), Value::Int(owner)]);
    if dest != source_station { outbox.notify_pilot(owner, "OnStationJumpCloneCacheInvalidated", vec![Value::Int(dest), Value::Int(owner)]); }
    Ok(Transition { station: dest, system, constellation, region, capsule })
}

#[cfg(test)] mod tests;
