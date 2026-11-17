//! Actor-owned activation cycles for supported dogma effects.
//! Mirrors clientDogmaLocation and godma; effect metadata comes from the SDE.
//! Subject authorization is shared by activation, deactivation, analyzers and online changes.
//! Projected effects and targeted ECM require a locked, present target; burst
//! jammers do not. Runtime effects are transient; unsupported effect families are
//! refused, and fitting limits use the shared policy.
//! Accepted running activations cancel arrival jump cloak; online changes are not cycles.
//! Authorized supported activations of another module break module cloak before validation.
//! Offlining a bank member destroys the whole persisted bank; unfit removes a slave
//! or destroys a master's bank. Committed changes stop affected cycles after the
//! current cycle, and slave weapon activation resolves to its authorized master.
//! Cycles are runtime-only and are not resumed after restart.
//! A non-targeted module's first-cycle FX may be unseen by observers until the next
//! destiny boundary applies the queued uncloak (under one second); gameplay is unaffected.
//! Activation and retirement update the system actor's cached fitted defenses.
//! Duration-bounded pure-modifier activations additionally carry module-item
//! keyed Pending/Running/Cooling records in shared combat state (active_cycle.rs).
//! Weapon animations are published through the system's ordered scene channel.
use super::{EffectKind, parameters, registry, spool, repair};
use super::registry::{Effect, EffectFlag};
use super::notify::ship_effect;
use super::behaviors::{on_cycle_end, on_stop};
use crate::services::combat::{state, ActiveModule, Ctx};
use crate::services::combat::groups_health::inject;
use crate::services::fit::Key;
use crate::services::items::load_items;
use crate::services::{db_err, notify_error, CallError};
use crate::dao::items::loading::{Item, ItemQuery};
use infinity_core::consts::{dogma::ATTR_ACTIVATION_BLOCKED, CATEGORY_MODULE, CATEGORY_SHIP, dogma::EFFECT_MICROWARPDRIVE, FLAG_HANGAR};
use infinity_core::consts::{dogma::EFFECT_ANCILLARY_REMOTE_ARMOR_REPAIRER, dogma::EFFECT_ANCILLARY_REMOTE_SHIELD_BOOSTER, dogma::ATTR_CHARGED_ARMOR_DAMAGE_MULTIPLIER};
use crate::client::filetime_from_unix_ms;
use infinity_core::messages;
use crate::space::{ms_to_seconds, seconds_to_ms, System, World, MODE_WARP};
use marshal::Value;
use std::collections::HashMap;
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
use tracing::{debug, warn};
use crate::db::Db;
use crate::db::executor::Executor;
use crate::sde::Sde;
use crate::lua::events::{GameEvent, ModuleCycleOutcome};
use super::online::OnlinePolicy;
mod activation;
pub(crate) use activation::{activate, activate_inner, deactivate};
mod begin;
pub(super) mod end;
pub(in crate::services::effects) mod combat_source;


pub(in crate::services) fn stop_generation(stop: &AtomicBool) {
    stop.store(true, Ordering::Relaxed);
}
pub(crate) fn force_inactive(module: i64, generation: &Arc<AtomicBool>) -> bool {
    let mut st = state();
    let Some(active) = st.active.get_mut(&module).filter(|active| Arc::ptr_eq(&active.stop, generation)) else { return false };
    active.forced_inactive = true;
    generation.store(true, Ordering::Relaxed);
    true
}

/// Check one activation's owner and generation without exposing combat state.
pub(crate) fn activation_current(module: i64, character: i64, generation: &Arc<AtomicBool>) -> bool {
    state().active.get(&module).is_some_and(|active|
        active.char_id == character && !active.forced_inactive && Arc::ptr_eq(&active.stop, generation))
}

pub(in crate::services) async fn stop_offensive(cx: &Ctx, except_module: i64) {
    let Ok(items) = load_items(&cx.db, cx.world.sde(), ItemQuery::Location(cx.ship_id)).await else { return };
    let policy = OnlinePolicy { owner: cx.char_id, active_ship: Some(cx.ship_id), station: None, notify: cx.notify.clone() };
    for item in items {
        if item.item_id == except_module { continue; }
        let Some(generation) = state().active.get(&item.item_id).filter(|active| active.char_id == cx.char_id).map(|active| active.stop.clone()) else { continue };
        for row in cx.world.sde().types.effects(item.type_id) {
            let Some(effect) = registry::resolve(cx.world.sde(), row.effect_id) else { continue };
            if !effect.has(EffectFlag::Offensive) || effect.kind == EffectKind::Cloak || !effect.manually_activatable() { continue; }
            if subject_core(&cx.db, cx.world.sde(), &policy, cx.ship_id, item.item_id, row.effect_id, true).await.is_err() { continue; }
            let module = item.item_id;
            let ship = cx.ship_id;
            cx.world.post(cx.system_id, move |s| {
                if force_inactive(module, &generation) {
                    super::active_cycle::finish_module(s, ship, module);
                    s.fitted_module_active(ship, module, false);
                    s.refresh_projection_target(ship);
                }
            });
            break;
        }
    }
}


/// Existing server minimum module cycle duration.
pub(super) const MIN_MODULE_DURATION_MS: f64 = 250.0;
/// Client UserError type-ID substitution tag.
pub(super) const UE_TYPEID: i64 = 4;



#[derive(Clone)]
pub(super) struct Module {
    pub(super) id: i64,
    pub(super) type_id: i32,
    pub(super) flag: i32,
}

/// Replay identity owned by one actual fitted member for the whole activation.
pub(super) struct NativeCombatGeneration {
    pub(super) module_id: i64,
    pub(super) generation: u64,
    pub(super) sequence: u64,
}


pub(in crate::services::effects) fn retire_combat_generations(s: &mut System, generations: Vec<NativeCombatGeneration>) {
    for member in generations {
        let _ = s.retire_combat_source(member.generation);
    }
}

/// Owns the markers for one activation, even while a callback or worker is queued.
pub(super) struct ActivationOwner {
    pub(super) master: i64,
    pub(super) generation: Arc<AtomicBool>,
    pub(super) armed: bool,
    pub(super) combat_generations: Vec<NativeCombatGeneration>,
    pub(super) pending_combat: Option<super::space_source::PreparedCombatAttempt>,
    pub(super) world: std::sync::Weak<World>,
    pub(super) system: i64,
}

impl Drop for ActivationOwner {
    fn drop(&mut self) {
        if self.armed {
            let mut st = state();
            if st.active.get(&self.master).is_some_and(|current| Arc::ptr_eq(&current.stop, &self.generation)) {
                st.active.remove(&self.master);
            }
            if st.activation_generation.get(&self.master).is_some_and(|current| Arc::ptr_eq(current, &self.generation)) {
                st.activation_generation.remove(&self.master);
            }
            // When no world remains to run the posted terminal, the dropping
            // generation's record still ends: Pending cancels and Running
            // starts its cooling against the wall clock. A live system's
            // posted finish_record owns the published terminal instead.
            if self.world.upgrade().is_none() {
                super::active_cycle::stopped(crate::space::now_ms(), &mut st, self.master, &self.generation);
            }
        }
        drop(self.pending_combat.take());
        let generations = std::mem::take(&mut self.combat_generations);
        if !generations.is_empty() {
            if let Some(world) = self.world.upgrade() {
                let (master, generation) = (self.master, self.generation.clone());
                world.post(self.system, move |s| {
                    retire_combat_generations(s, generations);
                    s.retire_fx_activation(master, &generation);
                    super::active_cycle::finish_record(s, master, &generation);
                });
            }
        }
    }
}

/// One activation moves between actor callbacks and its deferred phases.
pub(super) struct Cycle {
    pub(super) cx: Ctx,
    pub(super) modules: Vec<Module>,
    pub(super) effect: Effect,
    pub(super) target: Option<i64>,
    pub(super) repeat: i64,
    pub(super) stop: Arc<AtomicBool>,
    pub(super) owner: ActivationOwner,
    pub(super) started: i64,
    pub(super) charge_type: Option<i32>,
    pub(super) cycles: i64,
    pub(super) duration_ms: f64,
    pub(super) nominal_start: i64,
    pub(super) other: Option<Value>,
    pub(super) repair_amount: f64,
    pub(super) illegal: bool,
    /// Membership admitted at the first capture; binds once, never refreshed.
    pub(super) source_membership: Option<u64>,
    /// Target ball type bound at capture for the mining destination check.
    pub(super) target_type: Option<i32>,
    /// This cycle's typed parameters captured from the resident fit.
    pub(super) inputs: super::parameters::CycleInputs,
}

impl Drop for Cycle {
    /// A dropped cycle still returns every unconsumed loaded admission, and a
    /// cycle carrying a bound spool clears only its own generation's readout.
    fn drop(&mut self) {
        let permits: Vec<_> = self.inputs.members.iter_mut().filter_map(|member| member.permit.take()).collect();
        let spooled = self.inputs.members.iter().any(|member| member.spool.is_some());
        if !permits.is_empty() || spooled {
            let ship = self.cx.ship_id;
            let generation = self.stop.clone();
            self.cx.world.post(self.cx.system_id, move |s| {
                for permit in permits { s.release_loaded_use(permit); }
                if spooled { s.spool_readout_clear(ship, &generation); }
            });
        }
    }
}

pub(super) fn cycle_valid(s: &System, cycle: &Cycle) -> bool {
    s.contains(cycle.cx.ship_id)
        && cycle.target.is_none_or(|target| s.contains(target))
        && state().active.get(&cycle.modules[0].id).is_some_and(|v| Arc::ptr_eq(&v.stop, &cycle.stop))
}

pub(in crate::services::effects) fn cycle_source_current(s: &System, cycle: &Cycle) -> bool {
    !cycle.stop.load(Ordering::Relaxed)
        && s.ball(cycle.cx.ship_id).is_some_and(|ball| ball.char_id == Some(cycle.cx.char_id))
        && state().active.get(&cycle.modules[0].id).is_some_and(|active| Arc::ptr_eq(&active.stop, &cycle.stop))
}

pub(in crate::services::effects) fn cycle_target_lost(s: &System, cycle: &Cycle) -> bool {
    cycle_source_current(s, cycle) && cycle.target.is_some_and(|target| !s.contains(target))
}

pub(super) async fn target_loss(cycle: &Cycle) -> CycleOutcome {
    let (ship, character, master, target, generation) = (cycle.cx.ship_id, cycle.cx.char_id, cycle.modules[0].id, cycle.target, cycle.stop.clone());
    let absent = cycle.cx.world.with(cycle.cx.system_id, move |s| {
        !generation.load(Ordering::Relaxed)
            && s.ball(ship).is_some_and(|ball| ball.char_id == Some(character))
            && state().active.get(&master).is_some_and(|active| Arc::ptr_eq(&active.stop, &generation))
            && target.is_some_and(|target| !s.contains(target))
    }).await.unwrap_or(false);
    if absent { CycleOutcome::Ended { reason: "target lost", outcome: ModuleCycleOutcome::TargetLost } }
    else { CycleOutcome::Stop("target lost") }
}

pub(in crate::services::effects) fn publish_cycle(cycle: &Cycle, outcome: ModuleCycleOutcome) {
    for module in &cycle.modules {
        crate::progress::publish_live(cycle.cx.world.progress_registry(), &GameEvent::ModuleCycleEnded {
            char_id: cycle.cx.char_id, module_item_id: module.id, effect_id: cycle.effect.id, outcome,
        });
    }
}

pub(super) fn cycle_deadline(start: i64, duration_ms: f64) -> i64 {
    start.saturating_add(seconds_to_ms(ms_to_seconds(duration_ms)) as i64)
}

pub(super) fn cycle_is_final(stopped: bool, disallow_auto_repeat: bool, repeat: i64, cycles: i64) -> bool {
    stopped || disallow_auto_repeat || repeat <= 0 || cycles >= repeat
}

pub(super) fn cycle_should_stop(finished: bool, stop: &AtomicBool) -> bool {
    finished || stop.load(Ordering::Relaxed)
}

/// Settled snapshots may repeat; a missing or canceled capture must retire.
pub(super) fn boundary_continues(captured: bool, finished: bool, stop: &AtomicBool) -> bool {
    captured && !cycle_should_stop(finished, stop)
}

pub(super) fn schedule_cycle_end(s: &mut System, start: i64, duration_ms: f64, f: impl FnOnce(&mut System, i64) + Send + 'static) {
    let end = cycle_deadline(start, duration_ms);
    s.schedule_at(end, move |s| f(s, end));
}

pub(in crate::services::effects) fn retire_activation(active: &mut HashMap<i64, ActiveModule>, master: i64, stop: &Arc<AtomicBool>, publish: impl FnOnce()) -> bool {
    if !active.get(&master).is_some_and(|current| Arc::ptr_eq(&current.stop, stop)) { return false; }
    publish();
    active.remove(&master);
    true
}
fn blocker_rows(values: &[(i32, f64)], rows: &[crate::sde::dogma::Modifier]) -> bool {
    rows.iter().any(|row| row.domain == "targetID" && row.modified == ATTR_ACTIVATION_BLOCKED
        && values.iter().any(|&(attribute, value)| attribute == row.modifying && value > 0.0))
}

fn propulsion_blocked(sde: &crate::sde::Sde, projections: &[super::projection::Projection]) -> bool {
    projections.iter().any(|p| sde.dogma.effect(p.effect_id)
        .is_some_and(|effect| blocker_rows(&p.values, effect.modifiers())))
}

/// Stop running microwarpdrives after a blocking scrambler lands. The resident
/// fit supplies the fitted module list; the actor rechecks the blocker and the
/// activation generation before forcing each member inactive.
pub(crate) async fn stop_blocked_propulsion(world: &Arc<World>, system: i64, target: i64) {
    let sde = world.sde().clone();
    world.post(system, move |s| {
        if !propulsion_blocked(&sde, &s.projections_on(target)) { return; }
        let Some(character) = s.ball(target).and_then(|ball| ball.char_id) else { return };
        let modules = s.with_combat_fit(target, |fit, _key, _stamp| {
            fit.fitted_modules()
                .filter(|(_, type_id)| has_effect(fit.sde().types.effects(*type_id), EFFECT_MICROWARPDRIVE))
                .filter_map(|(module, _)| state().active.get(&module).map(|active| (module, active.stop.clone())))
                .collect::<Vec<_>>()
        }).unwrap_or_default();
        for (module, generation) in modules {
            let mut st = state();
            let Some(active) = st.active.get_mut(&module).filter(|active| active.char_id == character && Arc::ptr_eq(&active.stop, &generation)) else { continue };
            active.forced_inactive = true;
            generation.store(true, Ordering::Relaxed);
            drop(st);
            s.refresh_projection_target(target);
        }
    });
}



/// Capture this cycle's frozen inputs through the one mutable resident-fit
/// gate. The scoped borrow raises only this cycle's inactive module flags for
/// the capture and restores them before returning, so first-round admission
/// never publishes fitted output ahead of resource commitment. A missing or
/// stale record, or a membership replaced since admission, is an ordinary
/// miss, not an allocated error; membership binds once at the first admitted
/// capture and is never refreshed.
pub(super) fn capture_cycle_inputs(s: &mut System, cycle: &mut Cycle) -> Result<bool, CallError> {
    cycle.target_type = cycle.target.and_then(|target| s.ball(target).map(|ball| ball.type_id));
    // Return every previous round's unconsumed admission before the buffer is
    // taken; truncated members and failed captures cannot leak a finite use.
    for member in &mut cycle.inputs.members {
        if let Some(permit) = member.permit.take() { s.release_loaded_use(permit); }
    }
    let reuse = std::mem::take(&mut cycle.inputs);
    let captured = s.with_combat_fit_active(cycle.cx.ship_id,
        cycle.modules.iter().map(|module| module.id), |fit, _key, stamp| {
        // A member replaced since admission never reaches parameter evaluation.
        (!cycle.source_membership.is_some_and(|membership| membership != stamp.membership))
            .then(|| parameters::capture_cycle(fit, cycle, stamp, reuse))
            .map(|inputs| (inputs, stamp))
    });
    let Some(Some((captured, stamp))) = captured else { return Ok(false) };
    let inputs = captured?;
    cycle.source_membership.get_or_insert(stamp.membership);
    cycle.inputs = inputs;
    // A spool-capable member binds once to this activation's target
    // incarnation on its first successful capture; a bound member whose
    // captured identity no longer matches refuses the capture and ends the
    // activation, never zeroing then firing on a successor.
    if cycle.inputs.members.iter().any(|member|
        matches!(&member.packet, parameters::MemberPacket::Turret(inputs) if inputs.spool.is_some()))
    {
        let (Some(target), Some(target_membership)) = (cycle.target, cycle.target.and_then(|target| s.combat_membership_generation(target)))
            else { return Ok(false) };
        let nominal = filetime_from_unix_ms(cycle.nominal_start);
        for index in 0..cycle.modules.len() {
            let module_id = cycle.modules[index].id;
            let Some(member) = cycle.inputs.members.get_mut(index) else { break };
            let parameters::MemberPacket::Turret(inputs) = &member.packet else { continue };
            let Some(parameters) = inputs.spool else { continue };
            let Some(native) = cycle.owner.combat_generations.iter().find(|member| member.module_id == module_id) else {
                return Err(CallError::Internal(anyhow::anyhow!("spool member missing combat generation")));
            };
            let state = member.spool.get_or_insert_with(Default::default);
            match state.binding() {
                None => {
                    state.bind(spool::SpoolBinding {
                        source_membership: stamp.membership,
                        activation_generation: native.generation,
                        target,
                        target_membership,
                    });
                    // max_at uses this capture's frame duration, not the
                    // previous/default value cycle.duration_ms still holds.
                    s.spool_readout_begin(cycle.cx.ship_id, member.module, cycle.modules[0].id,
                        stamp.membership, &cycle.stop,
                        parameters.maximum_at(nominal, 0.0, cycle.inputs.frame.as_ref().map(|frame| frame.duration_ms).unwrap_or(cycle.duration_ms)),
                        cycle.cx.char_id);
                }
                Some(binding) => {
                    if binding.source_membership != stamp.membership
                        || binding.activation_generation != native.generation
                        || binding.target != target
                        || binding.target_membership != target_membership {
                        return Ok(false);
                    }
                }
            }
        }
    }
    if cycle.effect.kind == EffectKind::SuperWeapon && cycle.inputs.aim.is_none() {
        // A target without a combat record captures generation 0, which no
        // real membership generation can equal: the hit is suppressed later.
        cycle.inputs.aim = cycle.target.map(|id| parameters::SuperWeaponAim::NormalTarget { id, ball_generation: s.combat_membership_generation(id).unwrap_or(0) });
    }
    // Admit each member's one payable loaded use; consume or stop releases it.
    let kind = charge_kind(cycle.effect.id, cycle.effect.kind);
    let ship = cycle.cx.ship_id;
    for (index, (member, module)) in cycle.inputs.members.iter_mut().zip(&cycle.modules).enumerate() {
        if let Some(permit) = member.permit.take() { s.release_loaded_use(permit); }
        member.permit = (member.charge_token.is_some() && match kind {
            EffectKind::Turret => !matches!(&member.packet, parameters::MemberPacket::Turret(inputs) if inputs.direct_damage),
            EffectKind::Missile | EffectKind::Bomb | EffectKind::Vorton => true,
            EffectKind::FueledLocalRepair { .. } | EffectKind::CapBooster => index == 0,
            _ => false,
        }).then(|| s.hold_loaded_use(ship, module.id)).flatten();
    }
    Ok(true)
}

pub(in crate::services::effects) fn cycle_start(s: &mut System, mut cycle: Cycle, nominal_start: i64) {
    if cycle.effect.id == i64::from(EFFECT_MICROWARPDRIVE) && propulsion_blocked(cycle.cx.world.sde(), &s.projections_on(cycle.cx.ship_id)) {
        cycle.stop.store(true, Ordering::Relaxed);
        end::cycle_stop(s, cycle);
        return;
    }
    if matches!(cycle.effect.kind, EffectKind::Projected { .. })
        && s.ball(cycle.cx.ship_id).is_some_and(|ball| ball.mode == MODE_WARP) {
        end::cycle_stop(s, cycle);
        return;
    }
    if !cycle_valid(s, &cycle) {
        if cycle_target_lost(s, &cycle) { publish_cycle(&cycle, ModuleCycleOutcome::TargetLost); }
        end::cycle_stop(s, cycle);
        return;
    }
    if cycle.cycles == 0 {
        if matches!(cycle.effect.kind,
            EffectKind::FueledLocalRepair { .. } | EffectKind::RemoteRepair { .. } | EffectKind::CapTransfer
            | EffectKind::Neutralizer | EffectKind::Nosferatu | EffectKind::CapBooster | EffectKind::Ecm
            | EffectKind::Mining | EffectKind::Salvage | EffectKind::CargoScan | EffectKind::ShipScan
            | EffectKind::SurveyScan | EffectKind::Tractor | EffectKind::Entosis)
        {
            s.begin_fx_activation(cycle.modules[0].id, cycle.cx.ship_id, cycle.target, cycle.stop.clone());
        }
    }
    cycle.nominal_start = nominal_start;
    super::heat::begin_cycle(s, &cycle);
    match capture_cycle_inputs(s, &mut cycle) {
        Ok(true) => s.defer(async move { begin::cycle_begin_work(cycle).await; }),
        Ok(false) => end::cycle_stop(s, cycle),
        Err(error) => {
            report_cycle_error(&cycle.cx, cycle.modules[0].id, "capture", error);
            end::cycle_stop(s, cycle);
        }
    }
}

#[derive(Debug, PartialEq)]
pub(in crate::services::effects) enum CycleOutcome {
    Continue,
    Stop(&'static str),
    Ended { reason: &'static str, outcome: ModuleCycleOutcome },
}

pub(in crate::services::effects) fn cycle_discharge(kind: EffectKind, charged: bool, need: f64) -> f64 {
    if matches!(kind, EffectKind::FueledLocalRepair { layer: 0, .. }) && charged { 0.0 } else { need }
}

pub(in crate::services::effects) fn needs_charge(kind: EffectKind) -> bool { matches!(kind, EffectKind::CapBooster | EffectKind::Bomb | EffectKind::Vorton) }

pub(in crate::services::effects) fn charge_sensitive(kind: EffectKind) -> bool {
    matches!(kind, EffectKind::CapBooster | EffectKind::FueledLocalRepair { .. } | EffectKind::Bomb | EffectKind::Vorton)
}

pub(super) fn charge_kind(id: i64, kind: EffectKind) -> EffectKind {
    match id as i32 {
        EFFECT_ANCILLARY_REMOTE_SHIELD_BOOSTER => EffectKind::FueledLocalRepair { layer: 0, amount_attr: repair::amount_attribute(0), charge_bonus_attr: None },
        EFFECT_ANCILLARY_REMOTE_ARMOR_REPAIRER => EffectKind::FueledLocalRepair { layer: 1, amount_attr: repair::amount_attribute(1), charge_bonus_attr: Some(ATTR_CHARGED_ARMOR_DAMAGE_MULTIPLIER) },
        _ => kind,
    }
}

pub(in crate::services::effects) async fn refund_refused_charge(cx: &Ctx, amount: f64) -> Result<(), CallError> {
    if amount > 0.0 {
        inject(&cx.db, &cx.world, cx.system_id, cx.ship_id, amount).await?;
    }
    Ok(())
}

pub(in crate::services::effects) fn report_cycle_error(cx: &Ctx, module: i64, phase: &str, error: CallError) {
    if crate::services::combat::capacity::is_refusal(&error) {
        cx.notify("OnRemoteMessage", vec![Value::str(messages::MSG_CUSTOM_NOTIFY),
            Value::dict([("notify", Value::unicode(messages::effects::COMBAT_CAPACITY_FULL))])]);
    } else {
        match error {
            CallError::Internal(error) => warn!("module {module} cycle {phase} failed: {error:#}"),
            CallError::User { msg, .. } => warn!("module {module} cycle {phase} failed: {msg}"),
        }
    }
}

pub(in crate::services) fn has_effect(effects: &[crate::sde::types::TypeEffect], effect: i32) -> bool {
    effects.iter().any(|entry| entry.effect_id == effect)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn valid_subject(hull: &Item, module: &Item, owner: i64, ship: i64, active_ship: Option<i64>, station: Option<i64>, effects: &[crate::sde::types::TypeEffect], effect: i32, require_online: bool, online: bool) -> bool {
    hull.item_id == ship && hull.owner_id == owner && hull.category_id == i64::from(CATEGORY_SHIP)
        && (active_ship == Some(ship) || (hull.singleton && station == Some(hull.location_id) && hull.flag_id == FLAG_HANGAR))
        && module.owner_id == owner && module.location_id == ship && module.singleton
        && module.category_id == i64::from(CATEGORY_MODULE)
        && super::super::inventory::is_fitting_flag(module.flag_id)
        && has_effect(effects, effect)
        && (!require_online || online)
}

pub(super) async fn subject_core(ex: &(impl Executor + ?Sized), sde: &Sde, policy: &OnlinePolicy, ship: i64, item_id: i64, effect: i32, require_online: bool) -> Result<(Item, Item), CallError> {
    let refuse = || notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION);
    let hull = load_items(ex, sde, ItemQuery::Id(ship)).await.map_err(db_err)?.into_iter().next().ok_or_else(refuse)?;
    let module = load_items(ex, sde, ItemQuery::Id(item_id)).await.map_err(db_err)?.into_iter().next().ok_or_else(refuse)?;
    let online = if require_online {
        crate::dao::items::dogma::online(ex, ship).await.map_err(db_err)?.iter().any(|entry| entry.item_id == item_id)
    } else { false };
    if !valid_subject(&hull, &module, policy.owner, ship, policy.active_ship, policy.station, sde.types.effects(module.type_id), effect, require_online, online) {
        return Err(refuse());
    }
    Ok((hull, module))
}


pub(super) async fn subject(db: &Db, sde: &Sde, policy: &OnlinePolicy, ship: i64, item_id: i64, effect: i32, require_online: bool) -> Result<Item, CallError> {
    Ok(subject_core(db, sde, policy, ship, item_id, effect, require_online).await?.1)
}

/// Current source authority inside the actor: the same activation generation
/// and owner, a live resident record of the admitted membership, and the
/// actual module still fitted, online and carrying the SDE effect. An
/// ordinary stop still lets the in-flight end run; a stale activation, a
/// forced-inactive source or a replaced membership does not.
pub(super) fn source_subject_current(s: &mut System, ship: i64, owner: i64, membership: u64, module: &Module, effect: i64, activation: &Arc<AtomicBool>) -> bool {
    if !state().active.get(&module.id).is_some_and(|active| active.char_id == owner && !active.forced_inactive && Arc::ptr_eq(&active.stop, activation)) { return false; }
    if !s.ball(ship).is_some_and(|ball| ball.owner_id == owner && ball.char_id == Some(owner)) { return false; }
    s.with_combat_fit(ship, |fit, _key, stamp|
        stamp.membership == membership
            && fit.module_state(module.id).is_some_and(|(online, _)| online)
            && fit.source_type(Key::Module(module.id)) == Some(module.type_id)
            && has_effect(fit.sde().types.effects(module.type_id), effect as i32)
    ).unwrap_or(false)
}

/// The same source check for one in-flight cycle after deferred work returns.
pub(super) async fn cycle_subject_valid(cycle: &Cycle) -> bool {
    let Some(membership) = cycle.source_membership else { return false };
    let (world, system, cx, module, effect, activation) =
        (cycle.cx.world.clone(), cycle.cx.system_id, cycle.cx.clone(), cycle.modules[0].clone(), cycle.effect.id, cycle.stop.clone());
    world.with(system, move |s| source_subject_current(s, cx.ship_id, cx.char_id, membership, &module, effect, &activation)).await.unwrap_or(false)
}


#[cfg(all(test, feature = "tests-effects"))] mod tests;
