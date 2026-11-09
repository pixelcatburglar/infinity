//! Cloaking activation and decloaking gates for targeting and module use.
//! Client fittingScreen/pythonEffects.py checks fitted cloak suitability; destiny
//! CloakBall and UncloakBall own visibility. Cooldowns are transient actor state.
//! An active Entosis link prevents module cloak activation until link cleanup.
use std::sync::{Arc, atomic::Ordering};
use super::{EffectKind, lifecycle::{self, Cycle, CycleOutcome, Module}, registry::Effect};
use infinity_core::messages;
use crate::services::{combat::{self, Ctx, targeting_charges::clear_locks_on}, fit::{Fit, Key}, notify_error, CallError};
use crate::space::System;

/// dogma/const.py: attributeCanCloak.
const ATTR_CAN_CLOAK: i32 = 1163;
/// dogma/const.py: attributeDisallowCloaking.
const ATTR_DISALLOW_CLOAKING: i32 = 2454;
/// dogma/const.py: attributeDisallowEarlyDeactivation.
const ATTR_DISALLOW_EARLY_DEACTIVATION: i32 = 906;
/// dogma/const.py: attributeModuleReactivationDelay, milliseconds.
const ATTR_REACTIVATION_DELAY: i32 = 669;
/// dogma/const.py: attributeTargetingDelay, milliseconds.
const ATTR_TARGETING_DELAY: i32 = 560;


pub(super) async fn validate(cx: &Ctx, fit: &Fit, module: &Module, _effect: &Effect, _target: Option<i64>) -> Result<(), CallError> {
    if fit.value(Key::Ship, ATTR_CAN_CLOAK) <= 0.0 || fit.value(Key::Ship, ATTR_DISALLOW_CLOAKING) > 0.0 {
        return Err(notify_error(messages::effects::CLOAK_UNSUITABLE));
    }
    if combat::state().active.iter().any(|(&id, active)| active.char_id == cx.char_id && id != module.id
        && fit.value(Key::Module(id), ATTR_DISALLOW_EARLY_DEACTIVATION) > 0.0) {
        return Err(notify_error(messages::effects::CLOAK_UNSUITABLE));
    }
    let ship = cx.ship_id;
    let id = module.id;
    let status = cx.world.with(cx.system_id, move |s| {
        if crate::space::sovereignty::links::blocks(s, ship) { 4 }
        else if s.ball(ship).is_none_or(|b| b.cloak != 0) || s.has_module_cloak(ship) { 1 }
        else if !s.cloak_reactivation_ready(id) { 2 }
        else if s.cloak_obstructed(ship) { 3 }
        else { 0 }
    }).await.unwrap_or(1);
    match status {
        1 => Err(notify_error(messages::effects::CLOAK_ALREADY_ACTIVE)),
        2 => Err(notify_error(messages::effects::CLOAK_REACTIVATION_DELAY)),
        3 => Err(notify_error(messages::effects::CLOAK_TOO_CLOSE)),
        4 => Err(notify_error(messages::effects::ENTOSIS_ACTION_BLOCKED)),
        _ => Ok(()),
    }
}

pub(in crate::services) async fn before_lock(cx: &Ctx) -> Result<(), CallError> {
    let ship = cx.ship_id;
    let ready = cx.world.with(cx.system_id, move |s| {
        if s.has_module_cloak(ship) { s.uncloak(ship); }
        s.cloak_targeting_ready(ship)
    }).await.unwrap_or(false);
    if ready { Ok(()) } else { Err(notify_error(messages::effects::CLOAK_TARGETING_DELAY)) }
}

pub(in crate::services) async fn before_activate(cx: &Ctx, _module: i64, kind: EffectKind) -> Result<(), CallError> {
    if kind == EffectKind::Cloak { return Ok(()); }
    let ship = cx.ship_id;
    cx.world.with(cx.system_id, move |s| { if s.has_module_cloak(ship) { s.uncloak(ship); } }).await;
    Ok(())
}

pub(super) async fn start(cx: &Ctx, fit: &Fit, cycle: &mut Cycle) -> Result<CycleOutcome, CallError> {
    if cycle.cycles != 0 { return Ok(CycleOutcome::Continue); }
    let module = cycle.modules[0].id;
    let duration = |attr| fit.value(Key::Module(module), attr).max(0.0) as i64;
    let reactivation = duration(ATTR_REACTIVATION_DELAY);
    let targeting = duration(ATTR_TARGETING_DELAY);
    let generation = cycle.stop.clone();
    let ship = cx.ship_id;
    let cleanup_cx = cx.clone();
    let result = cx.world.with(cx.system_id, move |s| {
        if crate::space::sovereignty::links::blocks(s, ship) {
            return Err(notify_error(messages::effects::ENTOSIS_ACTION_BLOCKED));
        }
        if s.cloak_obstructed(ship) { return Ok(false); }
        let current = combat::state().active.get(&module).is_some_and(|a| Arc::ptr_eq(&a.stop, &generation));
        if !current { return Ok(false); }
        let guard_generation = generation.clone();
        Ok(s.begin_module_cloak(ship, module, generation, reactivation, targeting, move |s| {
            !guard_generation.load(Ordering::Relaxed) && !s.cloak_obstructed(ship)
                && !crate::space::sovereignty::links::blocks(s, ship)
                && combat::state().active.get(&module).is_some_and(|a| Arc::ptr_eq(&a.stop, &guard_generation))
        }, Some(Box::new(move |s| s.defer(async move {
            lifecycle::stop_offensive(&cleanup_cx, module).await;
            clear_locks_on(&cleanup_cx.world, cleanup_cx.system_id, cleanup_cx.ship_id, cleanup_cx.char_id);
        })))))
    }).await.unwrap_or(Ok(false))?;
    Ok(if result { CycleOutcome::Continue } else { CycleOutcome::Stop("cloak conditions changed") })
}

pub(super) fn stop(s: &mut System, cx: &Ctx, cycle: &Cycle) {
    s.stop_module_cloak(cx.ship_id, cycle.modules[0].id, &cycle.stop);
}
