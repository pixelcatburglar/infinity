//! Implant and booster activation mirrors menuSvcExtras/invItemFunctions.py and
//! implantsBoostersPanel.py, using game.items, game.item_attributes, game.skills,
//! game.skill_queue and game.jobs through the shared skill timeline and Fit.
//! Activation prerequisites use the effective skill snapshot at the operation time.
//! Brain mutations reconcile owned online ships in the same transaction; resulting
//! fitting changes and notifications are dispatched only after commit.
//! Booster duration is attribute 330 evaluated by Fit with static-data modifiers.
//! The static data has no skill modifier to duration, so Biology does not lengthen boosters here.
//! Only passiveFit effects are applied. Native comparisons and RNG are unavailable:
//! eligibility cutoffs are inclusive and chance effects use independent Bernoulli draws.
//! Legacy untimed boosters have no known expiry; dormant implants belong to their clone.
//! Static datetime follows dogma/attributes/datetime.py:6-12 (days since Unix epoch).
use super::{attributes::character_attributes_at, completion, db_err, effective, requirements::requirements, trading::{available_credits, refusal, source}, training, CallError, CallResult};
use crate::{client::{filetime_from_dogma_days, filetime_now, filetime_ticks_from_hours, filetime_ticks_from_ms, ms_from_filetime_ticks, Client}, consts::{dogma::ATTR_BOOSTER_DURATION, dogma::ATTR_BOOSTER_LAST_INJECTION_DATETIME, dogma::ATTR_BOOSTER_MAX_CHAR_AGE_HOURS, dogma::ATTR_BOOSTERNESS, dogma::ATTR_IMPLANTNESS, dogma::ATTR_NON_DIMINISHING_SKILL_INJECTOR_USES, CATEGORY_IMPLANT, FILETIME_SECOND, FILETIME_UNIX_EPOCH, FLAG_BOOSTER, FLAG_IMPLANT, GROUP_BOOSTER, IX_FLAG, IX_LOCATION_ID, IX_SINGLETON, IX_STACK_SIZE}, dao::{characters::social, items::{brain, dogma, inventory, loading::ItemQuery}}, db::{executor::Executor, outbox::Outbox}, sde::Sde, services::{effects::online::{self, OnlineChanged}, fit::{Fit, Key}, inventory::{notify_item_change, notify_item_removed, split_stack}, items::load_items}};
use marshal::Value;
use std::sync::Arc;

fn candidates(sde: &Sde, type_id: i32) -> Vec<(i32, i32)> {
    let mut effects: Vec<_> = sde.types.effects(type_id).iter().filter_map(|e| {
        let chance = sde.dogma.effect(e.effect_id)?.fitting_usage_chance_attribute_id?;
        sde.types.attribute(type_id, chance)?;
        Some((e.effect_id, chance))
    }).collect();
    effects.sort_unstable();
    effects.dedup();
    effects
}

fn slot(sde: &Sde, type_id: i32, attribute: i32) -> Result<i32, CallError> {
    let value = sde.types.attribute(type_id, attribute).ok_or_else(refusal)?;
    if !value.is_finite() || value <= 0.0 || value.fract() != 0.0 || value > i32::MAX as f64 { return Err(refusal()); }
    Ok(value as i32)
}

pub(super) async fn activate_tx(tx: &(impl Executor + ?Sized), outbox: &mut Outbox<OnlineChanged>, sde: &Arc<Sde>, char_id: i64, ship: Option<i64>, item_id: i64, location: Option<i64>, now: i64, draws: &[(i32, f64)], booster: bool) -> Result<Value, CallError> {
    crate::dao::characters::selection::lock_character(tx, char_id).await.map_err(db_err)?;
    let mut progress = completion::advance_tx(tx, outbox, sde, char_id, now).await?;
    let before = character_attributes_at(tx, sde, char_id, now).await?;
    let item = source(tx, sde, char_id, ship, item_id, false).await?;
    if booster && location != Some(item.location_id) || !booster && location.is_some() { return Err(refusal()); }
    let data = sde.types.get(item.type_id).ok_or_else(refusal)?;
    if data.published != Some(true) || data.category_id != Some(CATEGORY_IMPLANT) || booster && data.group_id != Some(GROUP_BOOSTER) || !booster && data.group_id == Some(GROUP_BOOSTER) { return Err(refusal()); }
    let (flag, slot_attr) = if booster { (FLAG_BOOSTER, ATTR_BOOSTERNESS) } else { (FLAG_IMPLANT, ATTR_IMPLANTNESS) };
    let slot = slot(sde, item.type_id, slot_attr)?;
    for existing in brain::list(tx, char_id).await.map_err(db_err)? {
        if existing.flag_id == flag && (existing.slot == Some(slot) || existing.slot.is_none() && sde.types.attribute(existing.type_id, slot_attr) == Some(slot as f64)) { return Err(refusal()); }
    }
    let snapshot = effective::snapshot(tx, char_id).await?;
    if requirements(sde, item.type_id).any(|(id, level)| snapshot.level(id) < level) { return Err(refusal()); }
    if booster {
        if let Some(age) = data.attribute(ATTR_BOOSTER_MAX_CHAR_AGE_HOURS) {
            let duration = filetime_ticks_from_hours(age).filter(|_| age >= 0.0).ok_or_else(refusal)?;
            let created = social::public_info(tx, char_id, FILETIME_SECOND, FILETIME_UNIX_EPOCH).await.map_err(db_err)?.ok_or_else(refusal)?.created;
            if now > created.checked_add(duration).ok_or_else(refusal)? { return Err(refusal()); }
        }
        if let Some(deadline) = data.attribute(ATTR_BOOSTER_LAST_INJECTION_DATETIME) {
            if now > filetime_from_dogma_days(deadline).ok_or_else(refusal)? { return Err(refusal()); }
        }
    }
    let credits_before = if booster { available_credits(tx, sde, char_id, now).await? } else { 0 };
    let overrides = dogma::overrides(tx, item.item_id).await.map_err(db_err)?;
    let id = if item.quantity > 1 {
        let id = split_stack(tx, &item, 1, char_id, item.location_id, item.flag_id, true).await?;
        for attr in &overrides { if !dogma::set_override(tx, id, attr.attribute_id, attr.value).await.map_err(db_err)? { return Err(refusal()); } }
        notify_item_change(tx, sde, outbox, item.item_id, vec![(IX_STACK_SIZE, Value::Int(i64::from(item.quantity)))]).await?;
        id
    } else {
        if !inventory::move_item(tx, &item, item.location_id, item.flag_id, char_id, true).await.map_err(db_err)? { return Err(refusal()); }
        item.item_id
    };
    if booster && !overrides.iter().any(|a| a.attribute_id == ATTR_NON_DIMINISHING_SKILL_INJECTOR_USES) {
        if let Some(value) = data.attribute(ATTR_NON_DIMINISHING_SKILL_INJECTOR_USES) {
            if !value.is_finite() || value < 0.0 || value.fract() != 0.0 || value >= i64::MAX as f64 { return Err(refusal()); }
            if !dogma::set_override(tx, id, ATTR_NON_DIMINISHING_SKILL_INJECTOR_USES, value).await.map_err(db_err)? { return Err(refusal()); }
        }
    }
    let empty: &[i32] = &[];
    if !brain::install(tx, id, char_id, item.location_id, item.flag_id, flag, slot, Some(now), None, booster.then_some(empty)).await.map_err(db_err)? { return Err(refusal()); }
    if booster {
        let fit = Fit::load_character_at(tx, sde.clone(), char_id, now).await?;
        let mut selected = Vec::new();
        for (effect, chance) in candidates(sde, item.type_id) {
            let draw = draws.binary_search_by_key(&effect, |v| v.0).ok().map(|i| draws[i].1).ok_or_else(refusal)?;
            let probability = fit.value(Key::Booster(id), chance);
            if !probability.is_finite() || !draw.is_finite() || !(0.0..1.0).contains(&draw) { return Err(refusal()); }
            if draw < probability.clamp(0.0, 1.0) { selected.push(effect); }
        }
        let old = brain::get(tx, char_id, id).await.map_err(db_err)?.ok_or_else(refusal)?;
        if !brain::update_metadata(tx, char_id, &old, Some(slot), Some(now), None, Some(&selected)).await.map_err(db_err)? { return Err(refusal()); }
        let fit = Fit::load_character_at(tx, sde.clone(), char_id, now).await?;
        let duration = fit.value(Key::Booster(id), ATTR_BOOSTER_DURATION);
        if !duration.is_finite() || duration <= 0.0 || duration >= ms_from_filetime_ticks(i64::MAX) as f64 { return Err(refusal()); }
        let ticks = filetime_ticks_from_ms(duration);
        if ticks <= 0 { return Err(refusal()); }
        let expiry = now.checked_add(ticks).ok_or_else(refusal)?;
        let old = brain::get(tx, char_id, id).await.map_err(db_err)?.ok_or_else(refusal)?;
        if !brain::update_metadata(tx, char_id, &old, Some(slot), Some(now), Some(expiry), Some(&selected)).await.map_err(db_err)? { return Err(refusal()); }
        let after = available_credits(tx, sde, char_id, now).await?;
        if after > credits_before { outbox.notify_pilot(char_id, "OnNonDiminishingInjectionsAdded", vec![Value::Int(after - credits_before)]); }
        progress.boosters_changed = true;
    } else { outbox.notify_pilot(char_id, "OnServerImplantsChanged", vec![]); }
    let previous_location = if id == item.item_id { item.location_id } else { 0 };
    notify_item_change(tx, sde, outbox, id, vec![(IX_LOCATION_ID, Value::Int(previous_location)), (IX_FLAG, Value::Int(i64::from(item.flag_id))), (IX_SINGLETON, Value::Bool(item.singleton))]).await?;
    training::retime_after_change_tx(tx, sde, char_id, now, &before, &mut progress).await?;
    progress.brain_changed = true;
    completion::finish_tx(tx, outbox, sde, char_id, now, &progress, &[]).await?;
    Ok(Value::None)
}

pub(super) async fn destroy_tx(tx: &(impl Executor + ?Sized), outbox: &mut Outbox<OnlineChanged>, sde: &Arc<Sde>, char_id: i64, item_id: i64, now: i64) -> Result<Value, CallError> {
    crate::dao::characters::selection::lock_character(tx, char_id).await.map_err(db_err)?;
    let mut progress = completion::advance_tx(tx, outbox, sde, char_id, now).await?;
    let before = character_attributes_at(tx, sde, char_id, now).await?;
    let old = brain::get(tx, char_id, item_id).await.map_err(db_err)?.filter(|row| row.flag_id == FLAG_IMPLANT).ok_or_else(refusal)?;
    let item = load_items(tx, sde, ItemQuery::Id(item_id)).await.map_err(db_err)?.into_iter().next().ok_or_else(refusal)?;
    if !brain::remove(tx, char_id, &old).await.map_err(db_err)? { return Err(refusal()); }
    notify_item_removed(outbox, item);
    training::retime_after_change_tx(tx, sde, char_id, now, &before, &mut progress).await?;
    progress.brain_changed = true;
    outbox.notify_pilot(char_id, "OnServerImplantsChanged", vec![]);
    completion::finish_tx(tx, outbox, sde, char_id, now, &progress, &[]).await?;
    Ok(Value::None)
}

async fn activate(c: &mut Client, args: &[Value], booster: bool) -> CallResult {
    if args.len() != if booster { 2 } else { 1 } { return Err(refusal()); }
    let item_id = args[0].as_int().filter(|id| *id > 0).ok_or_else(refusal)?;
    let location = if booster { Some(args[1].as_int().filter(|id| *id > 0).ok_or_else(refusal)?) } else { None };
    let char_id = c.session.int("charid").filter(|id| *id > 0).ok_or_else(refusal)?;
    let ship = c.session.int("shipid");
    let now = filetime_now();
    let _lock = crate::services::market::lock().await;
    let db = c.db.clone();
    let sde = c.sde.clone();
    let draws = if booster {
        let item = load_items(&db, &sde, ItemQuery::Id(item_id)).await.map_err(db_err)?.into_iter().next().ok_or_else(refusal)?;
        candidates(&sde, item.type_id).into_iter().map(|(effect, _)| (effect, crate::services::rand01())).collect::<Vec<_>>()
    } else { Vec::new() };
    db.transaction(|tx, outbox: &mut Outbox<OnlineChanged>| { let sde = sde.clone(); let draws = draws.clone(); Box::pin(async move { activate_tx(tx, outbox, &sde, char_id, ship, item_id, location, now, &draws, booster).await }) }, |value, outbox| async move { let result = online::dispatch_changed(value, outbox, c); crate::scheduler::wake(); result }).await
}
pub(crate) async fn inject_implant(c: &mut Client, args: &[Value]) -> CallResult { activate(c, args, false).await }
pub(crate) async fn use_booster(c: &mut Client, args: &[Value]) -> CallResult { activate(c, args, true).await }
pub(crate) async fn destroy_implant(c: &mut Client, args: &[Value]) -> CallResult {
    if args.len() != 1 { return Err(refusal()); }
    let item_id = args[0].as_int().filter(|id| *id > 0).ok_or_else(refusal)?;
    let char_id = c.session.int("charid").filter(|id| *id > 0).ok_or_else(refusal)?;
    let now = filetime_now();
    let _lock = crate::services::market::lock().await;
    let db = c.db.clone(); let sde = c.sde.clone();
    db.transaction(|tx, outbox: &mut Outbox<OnlineChanged>| { let sde = sde.clone(); Box::pin(async move { destroy_tx(tx, outbox, &sde, char_id, item_id, now).await }) }, |value, outbox| async move { let result = online::dispatch_changed(value, outbox, c); crate::scheduler::wake(); result }).await
}

#[cfg(test)] mod tests;
