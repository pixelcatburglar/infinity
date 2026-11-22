//! Tracked activations: the duration-bounded pure-modifier consumers:
//! assault damage control (SDE effect `moduleBonusAssaultDamageControl`),
//! emergency hull energizer (`emergencyHullEnergizer`), signature radius
//! suppressor (`targetSpectrumBreakerBonus`) and interdiction nullifier
//! (`interdictionNullifierBonus`), and the repeating ship modes
//! (`moduleBonusBastionModule`, `moduleBonusSacribisBastion`,
//! `moduleBonusSiegeModule`, `industrialCoreEffect2`,
//! `industrialCompactCoreEffect2`, `moduleBonusTriageModule`,
//! `moduleBonusStellianTriageModule`). Mirrors
//! `godma.py` activation and the `shipmodulebutton.py` reactivation gate.
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
//! Normal, forced and dropped endings share one generation-owned terminal:
//! the record is consumed once, and a successor activation on the same
//! membership is never cleared. `finish_current` is the bounded consumer's
//! nominal end; ship modes stay on the generic cycle scheduler and reach the
//! same terminal through it. Removal refusals live once in inventory
//! `move_core`; activation refusals use the client's
//! `ModuleReactivationDelayed2` and
//! `CannotRemoveModuleReactivationDelayed2` catalogue entries.
//!
//! The energizer additionally writes module damage equal to the module's
//! effective hit points once per activation through the shared heat-damage
//! authority (`eve/common/script/dogma/pythonEffects/emergencyHullEnergizer.py:22-24`);
//! the burn persists after the effect stops, an unrepaired module is refused
//! by the existing burnt validation, and while that same Running generation is
//! burned the ordinary burnt-out reconcile must not offline it early.
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
use infinity_core::consts::dogma::{ATTR_DISALLOW_EARLY_DEACTIVATION, ATTR_REACTIVATION_DELAY, ATTR_STRUCTURE_HP,
    EFFECT_ASSAULT_DAMAGE_CONTROL, EFFECT_BASTION, EFFECT_EMERGENCY_HULL_ENERGIZER, EFFECT_INDUSTRIAL_COMPACT_CORE,
    EFFECT_INDUSTRIAL_CORE, EFFECT_INTERDICTION_NULLIFIER_BONUS, EFFECT_SACRIBIS_BASTION, EFFECT_SIEGE,
    EFFECT_STELLIAN_TRIAGE, EFFECT_TARGET_SPECTRUM_BREAKER_BONUS, EFFECT_TRIAGE};
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

/// The four duration-bounded consumers, identified by SDE effect ID: each
/// runs its accepted duration once through `finish_current` and cools for
/// its fitted reactivation delay. Everything else keeps the generic effect
/// lifecycle.
pub(super) fn duration_bounded(effect: i64) -> bool {
    effect == i64::from(EFFECT_ASSAULT_DAMAGE_CONTROL)
        || effect == i64::from(EFFECT_EMERGENCY_HULL_ENERGIZER)
        || effect == i64::from(EFFECT_TARGET_SPECTRUM_BREAKER_BONUS)
        || effect == i64::from(EFFECT_INTERDICTION_NULLIFIER_BONUS)
}

/// Ship modes identified by SDE effect ID: Bastion, Sacribis, siege, the
/// industrial and compact cores, and ordinary and Stellian triage.
/// Legacy `triageMode` and `triageMode7` stay outside this set.
pub(crate) fn ship_mode(effect: i64) -> bool {
    effect == i64::from(EFFECT_BASTION) || effect == i64::from(EFFECT_SACRIBIS_BASTION)
        || effect == i64::from(EFFECT_SIEGE)
        || effect == i64::from(EFFECT_TRIAGE) || effect == i64::from(EFFECT_STELLIAN_TRIAGE)
        || industrial_core(effect)
}

/// The industrial and compact cores. Compression dependents follow only these.
pub(super) fn industrial_core(effect: i64) -> bool {
    effect == i64::from(EFFECT_INDUSTRIAL_CORE) || effect == i64::from(EFFECT_INDUSTRIAL_COMPACT_CORE)
}

/// One mode source check: the shared subject authority plus a live phase,
/// no warp and no module cloak. Callers pass the values they already hold.
pub(super) fn mode_source_current(s: &mut System, ship: i64, owner: i64, membership: u64, module: &super::lifecycle::Module, effect: i64, generation: &Arc<AtomicBool>) -> bool {
    super::lifecycle::source_subject_current(s, ship, owner, membership, module, effect, generation)
        && matches!(s.combat_phase(ship), Some(crate::space::combat::CombatPhase::Live))
        && s.ball(ship).is_some_and(|ball| ball.mode != crate::space::MODE_WARP)
        && !s.has_module_cloak(ship)
}

/// Every effect whose activation owns a tracked record: the duration-bounded
/// consumers plus the ship modes. Modes share pending registration, cooldown
/// admission, forced terminals and the early-deactivation lock but keep the
/// generic cycle scheduler; they are different questions, not aliases.
pub(super) fn tracked(effect: i64) -> bool {
    duration_bounded(effect) || ship_mode(effect)
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
pub(super) fn register(st: &mut combat::State, module: i64, ship: i64, effect: i64, generation: &Arc<AtomicBool>) {
    st.active_cycles.remove(&module);
    st.cycle_deadlines.retain(|&(_, other)| other != module);
    st.active_cycles.insert(module, ActiveCycle::Pending { generation: generation.clone(), ship, effect });
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
    // A mode's final admission revalidates the whole shared source authority
    // (generation, owner, live phase, online module and presented effect)
    // plus the mode preconditions in the same actor turn that publishes.
    if ship_mode(cycle.effect.id) {
        let Some(membership) = cycle.source_membership else { return None };
        if !mode_source_current(s, cycle.cx.ship_id, cycle.cx.char_id, membership, &cycle.modules[0], cycle.presented_effect, &cycle.stop) {
            return None;
        }
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
            effect: cycle.presented_effect,
            membership: cycle.source_membership.unwrap_or(0),
            start,
            duration_ms: cycle.duration_ms,
            delay_ms: inputs.delay_ms,
            repeat: cycle.repeat,
        });
    }
    cycle.started = filetime_from_unix_ms(start);
    s.begin_fx_activation(master, cycle.cx.ship_id, cycle.target, cycle.stop.clone());
    ship_effect(&cycle.cx, master, cycle.presented_effect, cycle.target, cycle.other.clone(),
        true, cycle.started, cycle.duration_ms, cycle.repeat);
    if ship_mode(cycle.effect.id) {
        // The mode FX publishes inline in this same actor turn: posting the
        // shared adapter again would deliver the start in a second turn.
        if let Some(guid) = cycle.effect.guid.as_deref().and_then(|g| g.split(',').next()) {
            super::fx::publish_current_module_fx(s, &cycle.modules, guid, cycle.charge_type,
                cycle.effect.has(super::registry::EffectFlag::Offensive), cycle.duration_ms,
                cycle.repeat, Value::None, true, master, &cycle.stop);
        }
        // First acceptance only: native Stop clears follow and orbit.
        // Repeats never reach accept. The fitted-active bit published after
        // this turn is the one kinematics rebase.
        let ship = cycle.cx.ship_id;
        let membership = cycle.source_membership;
        let generation = cycle.stop.clone();
        s.command(ship, move |ball, stamp, system| {
            if system.combat_membership_generation(ship) != membership
                || !combat::state().active.get(&master).is_some_and(|active| Arc::ptr_eq(&active.stop, &generation)) {
                return Vec::new();
            }
            ball.stop();
            vec![crate::space::event(stamp, "Stop", vec![Value::Int(ship)])]
        });
    } else {
        weapon_fx(&cycle.cx, &cycle.modules, &cycle.effect, cycle.target, None,
            true, cycle.duration_ms, cycle.repeat, master, &cycle.stop, FxRetention::Current, Value::None);
    }
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

/// The small terminal one consumed tracked record leaves for the actor: the
/// owning generation plus, for a `Running` record, the data its published
/// stop needs. `Pending` cancels silently and carries no publication data.
pub(super) struct Terminal {
    generation: Arc<AtomicBool>,
    module: i64,
    ship: i64,
    /// The combat membership bound at acceptance; a mode terminal applies
    /// only while the ship still owns that original incarnation.
    membership: Option<u64>,
    /// (owner, effect, start, duration_ms, repeat) of an accepted record.
    running: Option<(i64, i64, i64, f64, i64)>,
}

/// A terminal may publish the fitted stop only while this generation still
/// owns the module. A replacement activation, even on the same membership,
/// keeps its own bit and Godma start; the old FX record still retires.
fn terminal_owns(terminal: &Terminal) -> bool {
    !combat::state().active.get(&terminal.module)
        .is_some_and(|active| !Arc::ptr_eq(&active.stop, &terminal.generation))
}

/// Phase one of the shared tracked terminal, inside one short combat-state
/// critical section: fence on the activation generation, consume the record
/// once (`Pending` cancels, `Running` starts its captured delay), and detach
/// the matching active and activation-generation markers. `None` means no
/// record was bound to this generation; the markers still detach, so a
/// dropping owner runs this unconditionally and keeps no global cleanup
/// queue when no actor can run the second phase.
pub(super) fn terminate(st: &mut combat::State, module: i64, generation: &Arc<AtomicBool>) -> Option<Terminal> {
    let terminal = match st.active_cycles.get(&module) {
        Some(ActiveCycle::Pending { generation: current, ship, .. }) if Arc::ptr_eq(current, generation) =>
            Some(Terminal { generation: generation.clone(), module, ship: *ship, membership: None, running: None }),
        Some(ActiveCycle::Running { generation: current, ship, owner, effect, membership, start, duration_ms, repeat, .. })
            if Arc::ptr_eq(current, generation) =>
            Some(Terminal { generation: generation.clone(), module, ship: *ship, membership: Some(*membership),
                running: Some((*owner, *effect, *start, *duration_ms, *repeat)) }),
        _ => None,
    };
    if terminal.is_some() {
        stopped(now_ms(), st, module, generation);
    }
    if st.active.get(&module).is_some_and(|active| Arc::ptr_eq(&active.stop, generation)) {
        st.active.remove(&module);
    }
    if st.activation_generation.get(&module).is_some_and(|current| Arc::ptr_eq(current, generation)) {
        st.activation_generation.remove(&module);
    }
    terminal
}

/// Phase two of the shared tracked terminal, in the actor: an admitted
/// activation's stop publishes only while the ship still owns the combat
/// membership the record bound at acceptance, so a replaced incarnation or a
/// successor activation is never mutated. The generation's own FX record
/// always retires; expiry and retirement stay idempotent.
pub(super) fn apply_terminal(s: &mut System, terminal: Terminal) {
    let mut retired_core = None;
    if let Some((owner, effect, start, duration_ms, repeat)) = terminal.running {
        let same_membership = terminal.membership.is_some_and(|membership|
            s.combat_membership_generation(terminal.ship) == Some(membership));
        if same_membership && terminal_owns(&terminal) {
            s.fitted_module_active(terminal.ship, terminal.module, false);
            if industrial_core(effect) { retired_core = Some((terminal.ship, terminal.membership)); }
            let environment = super::environment::build(terminal.module, owner, terminal.ship, None, None, effect);
            s.notify_pilot(owner, "OnGodmaShipEffect", notify::ship_effect_args(
                terminal.module, effect, environment, false, filetime_from_unix_ms(start), duration_ms, repeat, filetime_now()));
        }
    }
    s.retire_fx_activation(terminal.module, &terminal.generation);
    // Dependents are withdrawn only after this core is inactive, and only
    // when no other admitted core remains on the same membership.
    if let Some((ship, membership)) = retired_core {
        if !super::compression::live_core(s, ship) {
            let fitted = s.resident_fit(ship, |fit, _| fit.fitted_modules().map(|(module, _)| module).filter(|&module| module != terminal.module).collect::<Vec<_>>()).unwrap_or_default();
            for module in fitted { super::compression::withdraw_facility(s, ship, module, membership); }
        }
    }
}

/// Forced terminal without a cycle transport: committed offline, committed
/// fitting remove or replace, offensive stop and ship departure all retire
/// through the same generation fence. A `Running` record publishes its stop
/// and starts its delay; `Pending` is cancelled silently.
pub(crate) fn finish_module(s: &mut System, ship: i64, module: i64) {
    let terminal = {
        let mut st = combat::state();
        let generation = match st.active_cycles.get(&module) {
            Some(ActiveCycle::Pending { generation, ship: fitted, .. }
                | ActiveCycle::Running { generation, ship: fitted, .. }) if *fitted == ship => generation.clone(),
            _ => return,
        };
        terminate(&mut st, module, &generation)
    };
    if let Some(terminal) = terminal {
        apply_terminal(s, terminal);
    }
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

/// Terminate every activation record whose ship left the actor (departure,
/// reset, boarding and hull loss share this actor-boundary cleanup).
pub(crate) fn ship_departure(s: &mut System, ship: i64) {
    for module in ship_records(ship) {
        finish_module(s, ship, module);
    }
}

/// A mode subject module: its tracked record is a ship-mode `Pending` or
/// `Running`. Inventory movement uses it to cover same-ship flag and owner
/// changes, which must not evade the mode lock.
pub(in crate::services) fn mode_subject(module: i64) -> bool {
    matches!(combat::state().active_cycles.get(&module),
        Some(ActiveCycle::Pending { effect, .. } | ActiveCycle::Running { effect, .. }) if ship_mode(*effect))
}

/// Later mode cycles revalidate the accepted record instead of re-admitting:
/// the same generation must still own `Running`.
pub(super) fn running_generation(module: i64, generation: &Arc<AtomicBool>) -> bool {
    matches!(combat::state().active_cycles.get(&module),
        Some(ActiveCycle::Running { generation: current, .. }) if Arc::ptr_eq(current, generation))
}

/// Same-ship admitted ship mode, the movement and exit lock's authority: a
/// `Running` mode record whose resident module node is still online and
/// active on a live combat record. `Pending` carries no lock and an ordinary
/// stop request keeps the lock until actual retirement clears the bit. The
/// scan reads only this ship's fitted nodes and looks each mode record up by
/// module ID, so it never walks every live activation; it also works while an
/// action holds the ship's ball.
pub(crate) fn mode_locked(s: &System, ship: i64) -> bool {
    if !matches!(s.combat_phase(ship), Some(crate::space::combat::CombatPhase::Live)) { return false; }
    let st = combat::state();
    s.resident_fit(ship, |fit, _| fit.fitted_modules().any(|(module, _)|
        matches!(st.active_cycles.get(&module),
            Some(ActiveCycle::Running { ship: fitted, effect, .. }) if *fitted == ship && ship_mode(*effect))
        && fit.module_state(module) == Some((true, true)))).unwrap_or(false)
}

/// One activation still forbids early deactivation when its effective
/// `disallowEarlyDeactivation` (attribute 906) value stays positive on
/// `fit`. An ordinary stop request still locks; a forced-inactive record
/// does not. Cloak and voluntary hull exit scan this fit's modules; offline
/// and removal ask only about the subject module.
fn early_locked(fit: &crate::services::fit::Fit, module: i64, active: &combat::ActiveModule) -> bool {
    !active.forced_inactive
        && fit.value(crate::services::fit::Key::Module(module), ATTR_DISALLOW_EARLY_DEACTIVATION) > 0.0
}

/// The activating module itself is excluded so its own activation cannot
/// refuse the cloak that is starting. Only this fit's modules are examined,
/// each through its keyed activation, so another ship's activation never
/// participates.
pub(crate) fn early_deactivation_locked(fit: &crate::services::fit::Fit, owner: i64, except: i64) -> bool {
    let active = &combat::state().active;
    fit.fitted_modules().any(|(module, _)| module != except && active.get(&module)
        .is_some_and(|entry| entry.char_id == owner && early_locked(fit, module, entry)))
}
/// Subject-only form of the same predicate: a pending or running mode
/// reserves its own module, and an unrelated module on the same fit does not.
pub(crate) fn subject_early_locked(fit: &crate::services::fit::Fit, module: i64) -> bool {
    combat::state().active.get(&module).is_some_and(|active| early_locked(fit, module, active))
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
