//! Salvager cycles and transactional wreck recovery.
//! The client dogma salvaging effect uses fitted accessDifficultyBonus and the
//! wreck's SDE accessDifficulty. Native wreck yield tables do not ship: rewards
//! are a configured random selection of published Salvaged Materials types.
//! Cargo occupancy follows `inventorycommon.util.GetItemVolume` for each item.
//! Loose wreck contents are retained in a positioned cargo container.
use super::{lifecycle::{Cycle, CycleOutcome, Module, subject_valid}, notify::fire_fx, registry::{Effect, EffectAttribute, EffectFlag}};
use crate::consts::{dogma::ATTR_CAPACITY, dogma::ATTR_MAX_RANGE, FLAG_CARGO, GROUP_CARGO_CONTAINER, GROUP_WRECK, IX_LOCATION_ID};
use crate::dao::items::{creation, inventory, loading::{self, ItemQuery}, salvage, ship};
use crate::db::outbox::{Effect as OutboxEffect, Outbox};
use crate::messages;
use crate::services::{combat::{Ctx, state, targeting_charges::targets_of}, db_err, fit::{Fit, Key, PERCENT_SCALE}, inventory::notify_item_change, items::{deposit, load_items}, market, rand01, user_error, CallError};
use crate::space::{dist, Ball, System, MODE_RIGID, MODE_WARP};
use marshal::Value;
use std::sync::Arc;
use crate::lua::events::ModuleCycleOutcome;

/// SDE dgmAttributeTypes: accessDifficulty, highIsGood=true, unit 121 (realPercent).
/// The additive sign and percent scaling are server policy.
const ATTR_ACCESS_DIFFICULTY: i32 = 901;
/// SDE dgmAttributeTypes: accessDifficultyBonus, highIsGood=true, unit 121 (realPercent).
/// The additive sign and percent scaling are server policy.
const ATTR_ACCESS_DIFFICULTY_BONUS: i32 = 902;
/// SDE invGroups: Salvaged Materials, excluding Ancient Salvage.
const GROUP_SALVAGED_MATERIALS: i32 = 754;

fn chance(bonus: f64, difficulty: f64) -> f64 {
    ((bonus + difficulty) / PERCENT_SCALE).clamp(0.0, 1.0)
}

fn active(master: i64, generation: &Arc<std::sync::atomic::AtomicBool>, character: i64, target: i64) -> bool {
    let current = state().active.get(&master).is_some_and(|active| Arc::ptr_eq(&active.stop, generation));
    current && targets_of(character).contains(&target)
}

fn range(fit: &Fit, module: &Module, effect: &Effect) -> f64 {
    fit.value(Key::Module(module.id), effect.attribute(EffectAttribute::Range).unwrap_or(ATTR_MAX_RANGE))
}

fn target_ball(s: &System, source: i64, target: i64, range: f64) -> Option<Ball> {
    if source == target || !range.is_finite() || range < 0.0 { return None; }
    let (a, b) = s.sampled_ball(source).zip(s.sampled_ball(target))?;
    if a.mode == MODE_WARP || b.mode == MODE_WARP || (dist(a.pos, b.pos) - f64::from(a.radius) - f64::from(b.radius)).max(0.0) > range { return None; }
    Some(b)
}

pub(super) async fn validate(cx: &Ctx, fit: &Fit, module: &Module, effect: &Effect, target: Option<i64>) -> Result<(), CallError> {
    let refuse = || user_error(messages::MSG_DENIED_ACTIVATE_TARGET_NOT_PRESENT, vec![]);
    let target = target.filter(|id| targets_of(cx.char_id).contains(id)).ok_or_else(refuse)?;
    let item = load_items(&cx.db, cx.world.sde(), ItemQuery::Id(target)).await.map_err(db_err)?.into_iter().next().ok_or_else(refuse)?;
    if item.group_id != i64::from(GROUP_WRECK) || item.location_id != cx.system_id { return Err(refuse()); }
    let (source, range) = (cx.ship_id, range(fit, module, effect));
    if cx.world.with(cx.system_id, move |s| target_ball(s, source, target, range)).await.flatten().is_none() { return Err(refuse()); }
    Ok(())
}

pub(super) async fn start(cx: &Ctx, fit: &Fit, cycle: &mut Cycle) -> Result<CycleOutcome, CallError> {
    if !subject_valid(cx, &cycle.modules[0], &cycle.effect).await? { return Ok(CycleOutcome::Stop("target lost")); }
    if validate(cx, fit, &cycle.modules[0], &cycle.effect, cycle.target).await.is_err() { return Ok(super::lifecycle::target_loss(cycle).await); }
    if cycle.cycles == 0 {
        if let Some(guid) = cycle.effect.guid.as_deref().and_then(|g| g.split(',').next()).map(str::to_owned) {
            let (modules, ship, target, duration, repeat, offensive) = (cycle.modules.clone(), cx.ship_id, cycle.target, cycle.duration_ms, cycle.repeat, cycle.effect.has(EffectFlag::Offensive));
            cx.world.broadcast(cx.system_id, ship, move |stamp| modules.iter().map(|m| fire_fx(stamp, ship, Some((m.id, m.type_id)), &guid, target, None, offensive, true, duration, repeat)).collect());
        }
    }
    Ok(CycleOutcome::Continue)
}

pub(super) async fn end(cx: &Ctx, fit: &Fit, cycle: &mut Cycle) -> Result<CycleOutcome, CallError> {
    let Some(target) = cycle.target else { return Ok(CycleOutcome::Stop("target lost")); };
    if !subject_valid(cx, &cycle.modules[0], &cycle.effect).await? { return Ok(CycleOutcome::Stop("target lost")); }
    if validate(cx, fit, &cycle.modules[0], &cycle.effect, Some(target)).await.is_err() { return Ok(super::lifecycle::target_loss(cycle).await); }
    let (source, character, master, generation, range) = (cx.ship_id, cx.char_id, cycle.modules[0].id, cycle.stop.clone(), range(fit, &cycle.modules[0], &cycle.effect));
    let generation_check = generation.clone();
    let ball = cx.world.with(cx.system_id, move |s| {
        if !active(master, &generation_check, character, target) { return None; }
        target_ball(s, source, target, range)
    }).await.flatten();
    let Some(ball) = ball else { return Ok(super::lifecycle::target_loss(cycle).await); };
    let sde = cx.world.sde().clone();
    let Some(difficulty) = sde.types.attribute(ball.type_id, ATTR_ACCESS_DIFFICULTY) else { return Ok(CycleOutcome::Stop("wreck difficulty missing")); };
    if rand01() >= chance(fit.value(Key::Module(master), ATTR_ACCESS_DIFFICULTY_BONUS), difficulty) { return Ok(CycleOutcome::Continue); }
    let mut pool: Vec<_> = sde.types.group_types(GROUP_SALVAGED_MATERIALS).iter().copied().filter(|id| sde.types.get(*id).is_some_and(|t| t.published == Some(true))).collect();
    if pool.is_empty() { return Err(CallError::Internal(anyhow::anyhow!("published salvage materials are missing"))); }
    let policy = &crate::config::game().salvage;
    let count = (policy.salvage_material_stacks as usize).min(pool.len());
    let mut yields = Vec::with_capacity(count);
    for i in 0..count {
        let j = i + (rand01() * (pool.len() - i) as f64) as usize;
        pool.swap(i, j);
        let width = u64::from(policy.salvage_quantity_max) - u64::from(policy.salvage_quantity_min) + 1;
        let quantity = u64::from(policy.salvage_quantity_min) + (rand01() * width as f64) as u64;
        yields.push((pool[i], quantity as i32));
    }
    let container = sde.types.iter().filter(|t| t.group_id == Some(GROUP_CARGO_CONTAINER)).min_by_key(|t| t.type_id).map(|t| (t.type_id, t.radius));
    let _lock = market::lock().await;
    let capacity = Fit::load_in_space(&cx.db, &cx.world, cx.system_id, cx.char_id, source).await?
        .value(Key::Ship, ATTR_CAPACITY);
    let world = cx.world.clone();
    let sender = cx.notify.clone();
    let system = cx.system_id;
    let outcome = cx.db.transaction(
        move |tx, outbox: &mut Outbox<Option<Ball>>| {
            let (sde, yields, ball, generation) = (sde.clone(), yields.clone(), ball.clone(), generation.clone());
            Box::pin(async move {
                if !active(master, &generation, character, target) { return Ok(false); }
                if !ship::lock_reload_ship(tx, source).await.map_err(db_err)? { return Ok(false); }
                let cargo = loading::load_items(tx, ItemQuery::LocationFlags(source, &[FLAG_CARGO])).await.map_err(db_err)?;
                let volume = |id, singleton, quantity| {
                    let value = crate::services::item_volume::item_volume(&sde, id, singleton, quantity, None)?;
                    if !value.is_finite() || value < 0.0 { return Err(CallError::Internal(anyhow::anyhow!("invalid item volume for type {id}"))); }
                    Ok(value)
                };
                let used = cargo.iter().try_fold(0.0, |sum, item| Ok::<_, CallError>(sum + volume(item.type_id, item.singleton, i64::from(item.quantity))?))?;
                let added = yields.iter().try_fold(0.0, |sum, (id, quantity)| Ok::<_, CallError>(sum + volume(*id, false, i64::from(*quantity))?))?;
                if !capacity.is_finite() || !used.is_finite() || !added.is_finite() || used + added > capacity { return Ok(false); }
                if !salvage::claim(tx, target, system, ball.type_id).await.map_err(db_err)? { return Ok(false); }
                let loot = load_items(tx, &sde, ItemQuery::Location(target)).await.map_err(db_err)?;
                if loot.len() != loading::load_items(tx, ItemQuery::Location(target)).await.map_err(db_err)?.len() {
                    return Err(CallError::Internal(anyhow::anyhow!("wreck contains an item with missing SDE type")));
                }
                let preserved = if loot.is_empty() { None } else {
                    let (kind, radius) = container.ok_or_else(|| CallError::Internal(anyhow::anyhow!("cargo container type missing")))?;
                    let id = creation::positioned(tx, kind, ball.owner_id, system, ball.pos).await.map_err(db_err)?;
                    for item in loot {
                        if !inventory::move_item(tx, &item, id, item.flag_id, item.owner_id, item.singleton).await.map_err(db_err)? { return Err(user_error(messages::MSG_DENIED_ACTIVATE_TARGET_NOT_PRESENT, vec![])); }
                        notify_item_change(tx, &sde, outbox, item.item_id, vec![(IX_LOCATION_ID, Value::Int(target))]).await?;
                    }
                    let mut replacement = ball.clone();
                    replacement.id = id; replacement.type_id = kind; replacement.mode = MODE_RIGID;
                    replacement.radius = radius.unwrap_or(f64::from(ball.radius)) as f32;
                    replacement.vel = [0.0; 3]; replacement.old_vel = [0.0; 3]; replacement.old_pos = replacement.pos;
                    replacement.slim.clear();
                    Some(replacement)
                };
                for (kind, quantity) in yields { deposit(tx, &sde, outbox, character, source, FLAG_CARGO, kind, quantity, target).await?; }
                if !active(master, &generation, character, target) {
                    return Err(user_error(messages::MSG_DENIED_ACTIVATE_TARGET_NOT_PRESENT, vec![]));
                }
                outbox.operation(preserved);
                Ok(true)
            })
        },
        move |value, outbox| async move {
            for entry in outbox.into_entries() {
                match entry {
                    OutboxEffect::Notification(notification) => notification.dispatch_sender(&world, &sender)?,
                    OutboxEffect::Event(event) => crate::lua::on_event(event),
                    OutboxEffect::Operation(replacement) => {
                        world.remove_ball(system, target).await;
                        if let Some(ball) = replacement { world.add_ship(system, ball).await.map_err(CallError::Internal)?; }
                    }
                }
            }
            Ok(value)
        },
    ).await?;
    Ok(if outcome { CycleOutcome::Ended { reason: "salvage complete", outcome: ModuleCycleOutcome::Completed } } else { CycleOutcome::Stop("salvage unavailable") })
}

pub(super) fn stop(s: &mut System, cx: &Ctx, cycle: &Cycle) {
    if let Some(guid) = cycle.effect.guid.as_deref().and_then(|g| g.split(',').next()) {
        let events = cycle.modules.iter().map(|m| fire_fx(s.current_time(), cx.ship_id, Some((m.id, m.type_id)), guid, cycle.target, None, cycle.effect.has(EffectFlag::Offensive), false, cycle.duration_ms, cycle.repeat)).collect();
        s.broadcast(cx.ship_id, events);
    }
}

#[cfg(test)] mod tests;
