//! Tractor beam cycles for inventory-backed wrecks and cargo containers.
//! Destiny FOLLOW and movement restoration are owned by the system actor; target
//! existence and positioning come from game.items. Loot-right restrictions are not
//! modelled, matching the inventory service's existing access policy.
//! Tractor beams follow to surface contact; the client supplies no stand-off distance.
use super::{lifecycle::{Cycle, CycleOutcome, Module, subject_valid}, registry::{Effect, EffectAttribute, EffectFlag}, notify::fire_fx};
use crate::consts::{dogma::ATTR_MAX_RANGE, dogma::ATTR_MAX_TRACTOR_VELOCITY, CARGO_CONTAINER_GROUPS, GROUP_WRECK};
use crate::dao::items::loading::ItemQuery;
use crate::messages;
use crate::services::{combat::{Ctx, state, targeting_charges::targets_of}, fit::{Fit, Key}, items::load_items, user_error, db_err, CallError};
use crate::space::{dist, System, MODE_WARP};
use std::sync::Arc;


fn container_group(group: i64) -> bool {
    group == i64::from(GROUP_WRECK) || i32::try_from(group).ok().is_some_and(|id| CARGO_CONTAINER_GROUPS.contains(&id))
}

fn in_range(s: &System, source: i64, target: i64, range: f64) -> bool {
    if !range.is_finite() || range < 0.0 || source == target { return false; }
    let Some((a, b)) = s.sampled_ball(source).zip(s.sampled_ball(target)) else { return false; };
    a.mode != MODE_WARP && b.mode != MODE_WARP && (dist(a.pos, b.pos) - f64::from(a.radius) - f64::from(b.radius)).max(0.0) <= range
}

async fn target_exists(cx: &Ctx, target: i64) -> Result<bool, CallError> {
    let item = load_items(&cx.db, cx.world.sde(), ItemQuery::Id(target)).await.map_err(db_err)?.into_iter().next();
    if !item.is_some_and(|item| container_group(item.group_id) && item.location_id == cx.system_id) { return Ok(false); }
    Ok(crate::dao::items::social::position(&cx.db, target).await.map_err(db_err)?.is_some_and(|p| p.location_id == cx.system_id))
}

fn range(fit: &Fit, module: &Module, effect: &Effect) -> f64 {
    fit.value(Key::Module(module.id), effect.attribute(EffectAttribute::Range).unwrap_or(ATTR_MAX_RANGE))
}

pub(super) async fn validate(cx: &Ctx, fit: &Fit, module: &Module, effect: &Effect, target: Option<i64>) -> Result<(), CallError> {
    let refuse = || user_error(messages::MSG_DENIED_ACTIVATE_TARGET_NOT_PRESENT, vec![]);
    let target = target.filter(|id| targets_of(cx.char_id).contains(id)).ok_or_else(refuse)?;
    if !target_exists(cx, target).await? { return Err(refuse()); }
    let (source, range) = (cx.ship_id, range(fit, module, effect));
    if !cx.world.with(cx.system_id, move |s| in_range(s, source, target, range)).await.unwrap_or(false) { return Err(refuse()); }
    Ok(())
}

pub(super) async fn start(cx: &Ctx, fit: &Fit, cycle: &mut Cycle) -> Result<CycleOutcome, CallError> {
    if !subject_valid(cx, &cycle.modules[0], &cycle.effect).await? { return Ok(CycleOutcome::Stop("target lost")); }
    if validate(cx, fit, &cycle.modules[0], &cycle.effect, cycle.target).await.is_err() { return Ok(super::lifecycle::target_loss(cycle).await); }
    let velocity = fit.value(Key::Module(cycle.modules[0].id), ATTR_MAX_TRACTOR_VELOCITY) as f32;
    if !velocity.is_finite() || velocity <= 0.0 { return Ok(CycleOutcome::Stop("invalid tractor velocity")); }
    let (target, master, generation, source, character, range) = (cycle.target.unwrap(), cycle.modules[0].id, cycle.stop.clone(), cx.ship_id, cx.char_id, range(fit, &cycle.modules[0], &cycle.effect));
    if cycle.cycles == 0 {
        let accepted = cx.world.with(cx.system_id, move |s| {
            let active = state().active.get(&master).is_some_and(|active| Arc::ptr_eq(&active.stop, &generation));
            if !active || !targets_of(character).contains(&target) || !in_range(s, source, target, range) { return false; }
            s.begin_tractor(target, master, generation, velocity, 0.0, source)
        }).await.unwrap_or(false);
        if !accepted { return Ok(CycleOutcome::Stop("tractor unavailable")); }
        if let Some(guid) = cycle.effect.guid.as_deref().and_then(|g| g.split(',').next()).map(str::to_owned) {
            let (modules, duration, repeat, offensive) = (cycle.modules.clone(), cycle.duration_ms, cycle.repeat, cycle.effect.has(EffectFlag::Offensive));
            cx.world.broadcast(cx.system_id, source, move |stamp| modules.iter().map(|m| fire_fx(stamp, source, Some((m.id, m.type_id)), &guid, Some(target), None, offensive, true, duration, repeat)).collect());
        }
    }
    Ok(CycleOutcome::Continue)
}

pub(super) fn stop(s: &mut System, cx: &Ctx, cycle: &Cycle) {
    if let Some(target) = cycle.target { s.end_tractor(target, cycle.modules[0].id, &cycle.stop); }
    if let Some(guid) = cycle.effect.guid.as_deref().and_then(|g| g.split(',').next()) {
        let events = cycle.modules.iter().map(|m| fire_fx(s.current_time(), cx.ship_id, Some((m.id, m.type_id)), guid, cycle.target, None, cycle.effect.has(EffectFlag::Offensive), false, cycle.duration_ms, cycle.repeat)).collect();
        s.broadcast(cx.ship_id, events);
    }
}

#[cfg(test)] mod tests;
