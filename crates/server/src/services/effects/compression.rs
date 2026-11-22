//! Industrial compression facility markers advertised on the ship slim item.
//! The in-space compression client reads compressibleItemsTypeList and maxRange;
//! both are fitted dogma attributes. Facility markers are transient actor state.
//! A positive activationRequiresActiveIndustrialCore requires a genuinely
//! admitted industrial or compact core on the same ship.
use std::sync::Arc;
use super::lifecycle::{Cycle, CycleOutcome, Module};
use super::parameters::MasterPacket;
use super::registry::Effect;
use infinity_core::consts::dogma::{ATTR_ACTIVATION_REQUIRES_ACTIVE_INDUSTRIAL_CORE, ATTR_COMPRESSIBLE_ITEMS_TYPE_LIST, ATTR_MAX_RANGE};
use infinity_core::messages;
use crate::services::{combat::{self, ActiveCycle, Ctx}, fit::{Fit, Key}, notify_error, CallError};
use crate::space::System;

pub(super) fn values(fit: &Fit, module: i64) -> Result<(i64, f64, f64), CallError> {
    let list = fit.value(Key::Module(module), ATTR_COMPRESSIBLE_ITEMS_TYPE_LIST);
    let range = fit.value(Key::Module(module), ATTR_MAX_RANGE);
    let core = fit.value(Key::Module(module), ATTR_ACTIVATION_REQUIRES_ACTIVE_INDUSTRIAL_CORE);
    if !list.is_finite() || list <= 0.0 || list.fract() != 0.0 || list > i64::MAX as f64 || !range.is_finite() || range <= 0.0 || !core.is_finite() {
        return Err(CallError::Internal(anyhow::anyhow!("invalid compression facility attributes for module {module}")));
    }
    Ok((list as i64, range, core))
}

/// A positive captured requirement is the only core gate. Missing and zero
/// requirements keep the facility's prior admission.
fn requires_core(core: f64) -> bool { core > 0.0 }

/// One same-ship industrial or compact core that is Running on the resident
/// membership, owned by that pilot, and still online and active with its
/// presented effect. An ordinary stop request keeps the capability until the
/// terminal actually retires it. Pending, cooling, forced-inactive and a
/// replaced generation are not live. Callers must not hold `combat::state`.
pub(super) fn live_core(s: &System, ship: i64) -> bool {
    if !matches!(s.combat_phase(ship), Some(crate::space::combat::CombatPhase::Live)) { return false; }
    let Some(membership) = s.combat_membership_generation(ship) else { return false; };
    let Some(owner) = s.ball(ship).and_then(|ball| ball.char_id) else { return false; };
    let state = combat::state();
    s.resident_fit(ship, |fit, _| fit.fitted_modules().any(|(module, type_id)| {
        let Some(ActiveCycle::Running { ship: fitted, effect, generation, membership: bound, owner: recorded, .. }) = state.active_cycles.get(&module) else { return false };
        *fitted == ship && *bound == membership && *recorded == owner && super::active_cycle::industrial_core(*effect)
            && state.active.get(&module).is_some_and(|active| active.char_id == owner && !active.forced_inactive && Arc::ptr_eq(&active.stop, generation))
            && fit.module_state(module) == Some((true, true))
            && super::lifecycle::has_effect(fit.sde().types.effects(type_id), *effect as i32)
    })).unwrap_or(false)
}

pub(super) async fn validate(cx: &Ctx, fit: &Fit, module: &Module, _effect: &Effect, _target: Option<i64>) -> Result<(), CallError> {
    let (_, _, core) = values(fit, module.id)?;
    if !requires_core(core) { return Ok(()); }
    let (ship, module_id) = (cx.ship_id, module.id);
    let admitted = cx.world.with(cx.system_id, move |s| live_core(s, ship) && s.resident_fit(ship, |resident, _| resident.fitted_modules().any(|(id, _)| id == module_id)).unwrap_or(false)).await.unwrap_or(false);
    if admitted { Ok(()) } else { Err(notify_error(messages::effects::INDUSTRIAL_CORE_REQUIRED)) }
}

pub(super) async fn start(cx: &Ctx, cycle: &mut Cycle) -> Result<CycleOutcome, CallError> {
    let module = cycle.modules[0].id;
    let (list, range, core) = match cycle.inputs.frame.as_ref().map(|frame| &frame.packet) {
        Some(MasterPacket::Compression(Ok(inputs))) => (inputs.list, inputs.range, inputs.core),
        Some(MasterPacket::Compression(Err(_))) => {
            return Err(CallError::Internal(anyhow::anyhow!("invalid compression facility attributes for module {module}")));
        }
        _ => return Err(CallError::Internal(anyhow::anyhow!("compression cycle captured without compression inputs"))),
    };
    let ship = cx.ship_id;
    let membership = cycle.source_membership;
    let generation = cycle.stop.clone();
    let active = cx.world.with(cx.system_id, move |s| {
        let owned = {
            let state = combat::state();
            state.active.get(&module).is_some_and(|active| Arc::ptr_eq(&active.stop, &generation)) && s.contains(ship)
        };
        if !owned { return false; }
        if requires_core(core) && (s.combat_membership_generation(ship) != membership || !live_core(s, ship)) { return false; }
        s.set_compressor(ship, module, generation, list, range);
        true
    }).await.unwrap_or(false);
    Ok(if active { CycleOutcome::Continue } else { CycleOutcome::Stop("source lost") })
}

pub(super) fn stop(s: &mut System, cx: &Ctx, cycle: &Cycle) {
    s.remove_compressor(cx.ship_id, cycle.modules[0].id, &cycle.stop);
}

/// Withdraw one admitted compression facility as soon as its core is gone.
/// The resident requirement must be positive now: a compressor admitted at
/// zero and later changed to positive is withdrawn, while zero stays. The
/// owned cycle still publishes its own Godma stop, FX retirement and
/// activation cleanup when its existing stop boundary runs. This does not
/// read a captured packet because the terminal has no cycle handle.
pub(super) fn withdraw_facility(s: &mut System, ship: i64, module: i64, membership: Option<u64>) -> bool {
    if s.combat_membership_generation(ship) != membership || live_core(s, ship) { return false; }
    let Some(owner) = s.ball(ship).and_then(|ball| ball.char_id) else { return false; };
    let generation = {
        let state = combat::state();
        let Some(active) = state.active.get(&module) else { return false };
        if active.char_id != owner || active.forced_inactive { return false; }
        active.stop.clone()
    };
    let admitted = s.resident_fit(ship, |fit, _| {
        fit.module_state(module) == Some((true, true))
            && fit.fitted_modules().any(|(id, type_id)| id == module && super::lifecycle::has_effect(fit.sde().types.effects(type_id), infinity_core::consts::dogma::EFFECT_INDUSTRIAL_ITEM_COMPRESSION))
            && fit.value(Key::Module(module), ATTR_ACTIVATION_REQUIRES_ACTIVE_INDUSTRIAL_CORE) > 0.0
    }).unwrap_or(false);
    if !admitted || !super::lifecycle::force_inactive(module, &generation) { return false; }
    s.fitted_module_active(ship, module, false);
    s.remove_compressor(ship, module, &generation);
    true
}
