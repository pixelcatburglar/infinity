//! Duration-bounded activations for the pure-modifier consumers: assault
//! damage control (SDE effect `moduleBonusAssaultDamageControl`), emergency
//! hull energizer (`emergencyHullEnergizer`) and signature radius suppressor
//! (`targetSpectrumBreakerBonus`). Mirrors `godma.py` activation and the
//! `shipmodulebutton.py` reactivation gate.
//!
//! One module-item keyed record in the shared combat state tracks each
//! activation: an admitted activation is `Pending` until its actor accepts it
//! (no bonus, effect animation or cooldown), then `Running` until the frozen
//! duration ends, then `Cooling` for the fitted `moduleReactivationDelay`
//! (attribute 669) captured at acceptance. Deadlines are runtime-only: they
//! survive refit, ship swap, system change and logout, and are cleared on
//! server restart; expiry is evaluated lazily on admission and removal through
//! one ordered deadline index, never by polling.
//!
//! `finish_current` is the one terminal for the nominal end; forced endings
//! (committed offline, committed fitting remove or replace, offensive stop,
//! reset and ship departure) land on `finish_module`/`ship_departure` with the
//! same generation fence, so a stale callback can never clear a new
//! generation, publish a duplicate stop or alter a live deadline. Removal
//! refusals live once in inventory `move_core`; activation refusals use the
//! client's `ModuleReactivationDelayed2` and
//! `CannotRemoveModuleReactivationDelayed2` catalogue entries.
//!
//! The energizer additionally writes module damage equal to the module's
//! effective hit points once per activation through the shared heat-damage
//! authority (`eve/common/script/dogma/pythonEffects/emergencyHullEnergizer.py:22-24`);
//! the burn persists after the effect stops, an unrepaired module is refused
//! by the existing burnt validation, and while that same Running generation is
//! burned the ordinary burnt-out reconcile must not offline it early.
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
use infinity_core::consts::dogma::{ATTR_REACTIVATION_DELAY, ATTR_STRUCTURE_HP,
    EFFECT_ASSAULT_DAMAGE_CONTROL, EFFECT_EMERGENCY_HULL_ENERGIZER, EFFECT_TARGET_SPECTRUM_BREAKER_BONUS};
use infinity_core::messages;
use marshal::Value;
use crate::client::{filetime_from_unix_ms, filetime_now};
use crate::services::{user_error, CallError};
use crate::services::combat::{self, ActiveCycle, Ctx};
use crate::lua::events::ModuleCycleOutcome;
use crate::space::{now_ms, System};
use crate::space::system::presentation::FxRetention;
use super::lifecycle::{self, Cycle};
use super::fx::weapon_fx;
use super::notify::{self, ship_effect};
use super::parameters::{ActiveCycleInputs, MasterPacket};

/// The three approved consumers, identified by SDE effect ID. Everything else
/// keeps the generic effect lifecycle.
pub(super) fn consumer(effect: i64) -> bool {
    effect == i64::from(EFFECT_ASSAULT_DAMAGE_CONTROL)
        || effect == i64::from(EFFECT_EMERGENCY_HULL_ENERGIZER)
        || effect == i64::from(EFFECT_TARGET_SPECTRUM_BREAKER_BONUS)
}

/// Drop expired cooling records at the front of the ordered deadline index.
fn expire(st: &mut combat::State, now: i64) {
    while let Some(&(deadline, module)) = st.cycle_deadlines.iter().next() {
        if deadline > now { break; }
        st.cycle_deadlines.remove(&(deadline, module));
        if matches!(st.active_cycles.get(&module), Some(ActiveCycle::Cooling { deadline: existing, .. }) if *existing == deadline) {
            st.active_cycles.remove(&module);
        }
    }
}

/// The `ModuleReactivationDelayed2` dict the client's `shipmodulebutton.py`
/// reads: the module type tuple, the remaining delay in seconds, the item ID
/// and the elapsed milliseconds since the terminal stop.
fn delayed_error(module: i64, type_id: i32, stopped: i64, deadline: i64, now: i64) -> CallError {
    user_error(messages::MSG_MODULE_REACTIVATION_DELAYED, vec![
        ("item".into(), Value::tuple([Value::Int(lifecycle::UE_TYPEID), Value::Int(i64::from(type_id))])),
        ("delay".into(), Value::Float((deadline - now) as f64 / 1000.0)),
        ("itemID".into(), Value::Int(module)),
        ("timeSinceLastStop".into(), Value::Int(now - stopped)),
    ])
}

/// Activation-time cooldown check, run again inside the registration critical
/// section. A live cooling record refuses with `ModuleReactivationDelayed2`.
pub(super) fn admission(st: &mut combat::State, module: i64, type_id: i32) -> Result<(), CallError> {
    expire(st, now_ms());
    let now = now_ms();
    let Some(ActiveCycle::Cooling { stopped, deadline }) = st.active_cycles.get(&module) else { return Ok(()) };
    if *deadline > now {
        return Err(delayed_error(module, type_id, *stopped, *deadline, now));
    }
    Ok(())
}

/// Register an admitted activation. Runs inside the shared activation
/// critical section: any earlier record or stale index for the module is
/// dropped before the Pending marker lands.
pub(super) fn register(st: &mut combat::State, module: i64, ship: i64, generation: &Arc<AtomicBool>) {
    st.active_cycles.remove(&module);
    st.cycle_deadlines.retain(|&(_, other)| other != module);
    st.active_cycles.insert(module, ActiveCycle::Pending { generation: generation.clone(), ship });
}

/// The `CannotRemoveActivatedModule` / `CannotRemoveModuleReactivationDelayed2`
/// refusal one removal must hit. Called from inventory `move_core`; callers
/// have already proven the item leaves its fitted location.
pub(in crate::services) fn removal_refusal(module: i64) -> Result<(), CallError> {
    let now = now_ms();
    let mut st = combat::state();
    expire(&mut st, now);
    match st.active_cycles.get(&module) {
        Some(ActiveCycle::Pending { .. } | ActiveCycle::Running { .. }) =>
            Err(user_error(messages::MSG_CANNOT_REMOVE_ACTIVATED_MODULE, vec![])),
        Some(ActiveCycle::Cooling { deadline, .. }) if *deadline > now =>
            Err(user_error(messages::MSG_CANNOT_REMOVE_MODULE_REACTIVATION_DELAYED,
                vec![("timePeriod".into(), Value::Float((deadline - now) as f64 / 1000.0))])),
        _ => Ok(()),
    }
}

/// True while the module's energizer activation is Running: the narrow
/// exception that keeps `online::reconcile_core` from offlining the
/// activation-burned module before its accepted duration ends. Only the
/// energizer burns itself; an ordinary module that heat-burnt out while
/// running keeps the standard offline reconcile.
pub(super) fn running(module: i64) -> bool {
    matches!(combat::state().active_cycles.get(&module),
        Some(ActiveCycle::Running { effect, .. }) if *effect == i64::from(EFFECT_EMERGENCY_HULL_ENERGIZER))
}

/// The Running record fields `ShipGetInfo` reports as `activeEffects`.
/// `Pending` and `Cooling` are never listed.
pub(in crate::services) fn running_effect(module: i64) -> Option<(i64, i64, f64, i64)> {
    let st = combat::state();
    match st.active_cycles.get(&module) {
        Some(ActiveCycle::Running { effect, start, duration_ms, repeat, .. }) =>
            Some((*effect, *start, *duration_ms, *repeat)),
        _ => None,
    }
}

/// The captured begin parameters one consumer cycle carries.
fn packet_inputs(cycle: &Cycle) -> Option<ActiveCycleInputs> {
    match &cycle.inputs.frame.as_ref()?.packet {
        MasterPacket::ActiveCycle(inputs) => Some(*inputs),
        _ => None,
    }
}

/// Accept an admitted activation in its actor: `Pending` becomes `Running` at
/// this turn's `System::now_ms`, start and effect animation publish through
/// the existing constructors, and the nominal end is scheduled by the
/// caller's returned deadline. `None` means the admission went stale (stop
/// requested, forced offline, ship or membership gone).
pub(super) fn accept(s: &mut System, cycle: &mut Cycle) -> Option<i64> {
    let master = cycle.modules[0].id;
    let inputs = packet_inputs(cycle)?;
    if cycle.stop.load(Ordering::Relaxed) || !s.contains(cycle.cx.ship_id)
        || s.combat_membership_generation(cycle.cx.ship_id) != cycle.source_membership {
        return None;
    }
    let start = s.now_ms();
    {
        let mut st = combat::state();
        if !matches!(st.active_cycles.get(&master), Some(ActiveCycle::Pending { generation, .. })
                if Arc::ptr_eq(generation, &cycle.stop)) {
            return None;
        }
        st.active_cycles.insert(master, ActiveCycle::Running {
            generation: cycle.stop.clone(),
            owner: cycle.cx.char_id,
            ship: cycle.cx.ship_id,
            effect: cycle.effect.id,
            start,
            duration_ms: cycle.duration_ms,
            delay_ms: inputs.delay_ms,
            repeat: cycle.repeat,
        });
    }
    cycle.started = filetime_from_unix_ms(start);
    s.begin_fx_activation(master, cycle.cx.ship_id, cycle.target, cycle.stop.clone());
    ship_effect(&cycle.cx, master, cycle.effect.id, cycle.target, cycle.other.clone(),
        true, cycle.started, cycle.duration_ms, cycle.repeat);
    weapon_fx(&cycle.cx, &cycle.modules, &cycle.effect, cycle.target, None,
        true, cycle.duration_ms, cycle.repeat, master, &cycle.stop, FxRetention::Current, Value::None);
    Some(lifecycle::cycle_deadline(start, cycle.duration_ms))
}

/// The one-off energizer write: the module's captured effective hit points
/// become its damage through the shared heat-damage authority. A refused
/// transaction emits no start, bonus or cooldown; the caller refunds the
/// activation's capacitor precharge.
pub(super) async fn burn(cx: &Ctx, cycle: &Cycle) -> Result<(), CallError> {
    let Some(inputs) = packet_inputs(cycle) else {
        return Err(CallError::Internal(anyhow::anyhow!("energizer cycle without captured burn inputs")));
    };
    super::heat::damage::burn_for_activation(cx, cycle.modules[0].id, inputs.hp, cycle.source_membership).await
}

/// Generation-fenced record transition at a terminal stop: a `Pending` record
/// is cancelled without cooldown, a `Running` record starts its captured
/// delay, and anything else passes (stale callbacks never alter a newer
/// deadline or a different record).
pub(super) fn stopped(now: i64, st: &mut combat::State, module: i64, generation: &Arc<AtomicBool>) {
    match st.active_cycles.get(&module) {
        Some(ActiveCycle::Pending { generation: current, .. }) if Arc::ptr_eq(current, generation) => {
            st.active_cycles.remove(&module);
        }
        Some(ActiveCycle::Running { generation: current, delay_ms, .. }) if Arc::ptr_eq(current, generation) => {
            let delay_ms = *delay_ms;
            if delay_ms > 0 {
                st.active_cycles.insert(module, ActiveCycle::Cooling { stopped: now, deadline: now + delay_ms });
                st.cycle_deadlines.insert((now + delay_ms, module));
            } else {
                st.active_cycles.remove(&module);
            }
        }
        _ => {}
    }
}

/// The terminal owner for the nominal end of one consumer cycle: the record
/// transition rides the shared cycle retirement, which removes the active
/// modifier and effect animation synchronously in this actor turn.
pub(super) fn finish_current(s: &mut System, cycle: Cycle) {
    if matches!(combat::state().active_cycles.get(&cycle.modules[0].id),
            Some(ActiveCycle::Running { generation, .. }) if Arc::ptr_eq(generation, &cycle.stop)) {
        lifecycle::publish_cycle(&cycle, ModuleCycleOutcome::Completed);
    }
    super::lifecycle::end::cycle_stop(s, cycle);
}

/// Forced terminal without a cycle transport: committed offline, committed
/// fitting remove or replace, offensive stop and ship departure all retire
/// through the same generation fence. A `Running` record publishes its stop
/// and starts its delay; `Pending` is cancelled silently.
pub(crate) fn finish_module(s: &mut System, ship: i64, module: i64) {
    let now = s.now_ms();
    let (generation, running) = {
        let mut st = combat::state();
        match st.active_cycles.get(&module) {
            Some(ActiveCycle::Pending { generation, ship: fitted }) if *fitted == ship => {
                let generation = generation.clone();
                st.active_cycles.remove(&module);
                (generation, None)
            }
            Some(ActiveCycle::Running { generation, ship: fitted, owner, effect, start, duration_ms, delay_ms, repeat, .. })
                if *fitted == ship =>
            {
                let generation = generation.clone();
                let running = (*owner, *effect, *start, *duration_ms, *repeat);
                let delay_ms = *delay_ms;
                if delay_ms > 0 {
                    st.active_cycles.insert(module, ActiveCycle::Cooling { stopped: now, deadline: now + delay_ms });
                    st.cycle_deadlines.insert((now + delay_ms, module));
                } else {
                    st.active_cycles.remove(&module);
                }
                (generation, Some(running))
            }
            _ => return,
        }
    };
    let retired = {
        let mut st = combat::state();
        let retired = st.active.get(&module).is_some_and(|active| Arc::ptr_eq(&active.stop, &generation));
        if retired {
            st.active.remove(&module);
            if st.activation_generation.get(&module).is_some_and(|current| Arc::ptr_eq(current, &generation)) {
                st.activation_generation.remove(&module);
            }
        }
        retired
    };
    if !retired { return; }
    if let Some((owner, effect, start, duration_ms, repeat)) = running {
        s.fitted_module_active(ship, module, false);
        let environment = super::environment::build(module, owner, ship, None, None, effect);
        s.notify_pilot(owner, "OnGodmaShipEffect", notify::ship_effect_args(
            module, effect, environment, false, filetime_from_unix_ms(start), duration_ms, repeat, filetime_now()));
    }
    s.retire_fx_activation(module, &generation);
}

/// Module item IDs whose `Pending` or `Running` record names this ship.
/// `Cooling` records survive their ship's departure by contract.
pub(crate) fn ship_records(ship: i64) -> Vec<i64> {
    let st = combat::state();
    st.active_cycles.iter().filter_map(|(&module, record)| match record {
        ActiveCycle::Pending { ship: fitted, .. } | ActiveCycle::Running { ship: fitted, .. } if *fitted == ship => Some(module),
        _ => None,
    }).collect()
}

/// Drop fallback: run the forced terminal for the record still bound to the
/// dropping activation's own generation; a newer record passes untouched.
pub(super) fn finish_record(s: &mut System, module: i64, generation: &Arc<AtomicBool>) {
    let ship = {
        let st = combat::state();
        match st.active_cycles.get(&module) {
            Some(ActiveCycle::Pending { generation: current, ship, .. }
                | ActiveCycle::Running { generation: current, ship, .. })
                if Arc::ptr_eq(current, generation) => *ship,
            _ => return,
        }
    };
    finish_module(s, ship, module);
}

/// Terminate every activation record whose ship left the actor (departure,
/// reset, boarding and hull loss share this actor-boundary cleanup).
pub(crate) fn ship_departure(s: &mut System, ship: i64) {
    for module in ship_records(ship) {
        finish_module(s, ship, module);
    }
}

/// Capture the consumer's one packet from the resident fit at cycle capture:
/// the module's reactivation delay and its effective hit points (read only by
/// the energizer burn).
pub(super) fn capture(fit: &crate::services::fit::Fit, module: i64) -> ActiveCycleInputs {
    let key = crate::services::fit::Key::Module(module);
    ActiveCycleInputs {
        delay_ms: fit.value(key, ATTR_REACTIVATION_DELAY).max(0.0) as i64,
        hp: fit.value(key, ATTR_STRUCTURE_HP),
    }
}

#[cfg(all(test, feature = "tests-effects"))] mod tests;
