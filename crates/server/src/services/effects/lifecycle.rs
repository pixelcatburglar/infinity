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
use super::{EffectKind, registry, unsupported, repair, capacitor, ecm};
use super::registry::{Effect, EffectAttribute, EffectFlag};
use super::notify::{fire_fx, ship_effect};
use super::propulsion::apply_propulsion;
use super::behaviors::{Activation, on_activate, on_cycle_start, on_cycle_end, on_stop};
use crate::services::combat::{state, ActiveModule, Ctx};
use crate::services::combat::groups_health::{drain_capacitor, inject};
use crate::services::combat::targeting_charges::{bank, charge_key, loaded};
use crate::services::fit::{Fit, Key};
use crate::services::items::load_items;
use crate::services::{db_err, notify_error, user_error, CallError, CallResult};
use crate::services::identity::Identity;
use crate::dao::items::loading::{Item, ItemQuery};
use crate::client::filetime_now;
use crate::consts::{dogma::ATTR_ACTIVATION_BLOCKED, dogma::ATTR_CAPACITOR_NEED, CATEGORY_MODULE, CATEGORY_SHIP, dogma::EFFECT_MICROWARPDRIVE, dogma::EFFECT_ONLINE, FLAG_HANGAR};
use crate::consts::{dogma::EFFECT_ANCILLARY_REMOTE_ARMOR_REPAIRER, dogma::EFFECT_ANCILLARY_REMOTE_SHIELD_BOOSTER, dogma::EFFECT_MUTADAPTIVE_REMOTE_ARMOR_REPAIRER, dogma::ATTR_CHARGED_ARMOR_DAMAGE_MULTIPLIER};
use crate::messages;
use crate::space::{ms_to_seconds, seconds_to_ms, NotifyTx, System, World, MODE_WARP};
use marshal::Value;
use std::collections::HashMap;
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
use tracing::{debug, info, warn};
use crate::db::Db;
use crate::db::executor::Executor;
use crate::db::outbox::Notification;
use crate::sde::Sde;
use crate::lua::events::{GameEvent, ModuleCycleOutcome};
use super::online::OnlinePolicy;


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
                if force_inactive(module, &generation) { s.refresh_projection_target(ship); }
            });
            break;
        }
    }
}


/// Existing server module duration fallback, in milliseconds.
const DEFAULT_MODULE_DURATION_MS: f64 = 1000.0;
/// Existing server minimum module cycle duration.
const MIN_MODULE_DURATION_MS: f64 = 250.0;
/// Client UserError type-ID substitution tag.
pub(super) const UE_TYPEID: i64 = 4;



#[derive(Clone)]
pub(super) struct Module {
    pub(super) id: i64,
    pub(super) type_id: i32,
    pub(super) flag: i32,
}
#[allow(clippy::too_many_arguments)]
pub(super) fn weapon_fx(cx: &Ctx, modules: &[Module], effect: &Effect, target: Option<i64>, charge: Option<i32>, start: bool, duration_ms: f64, repeat: i64) {
    let Some(guid) = effect.guid.as_deref().and_then(|g| g.split(',').next()).map(str::to_owned) else { return };
    let modules = modules.to_vec();
    let ship = cx.ship_id;
    let is_offensive = effect.has(EffectFlag::Offensive);
    cx.world.broadcast(cx.system_id, ship, move |stamp| {
        modules.iter().map(|m| fire_fx(stamp, ship, Some((m.id, m.type_id)), &guid, target, charge, is_offensive, start, duration_ms, repeat)).collect()
    });
}

/// Owns the markers for one activation, even while a callback or worker is queued.
pub(super) struct ActivationOwner {
    pub(super) master: i64,
    pub(super) generation: Arc<AtomicBool>,
    pub(super) armed: bool,
}

impl Drop for ActivationOwner {
    fn drop(&mut self) {
        if !self.armed { return; }
        let mut st = state();
        if st.active.get(&self.master).is_some_and(|current| Arc::ptr_eq(&current.stop, &self.generation)) {
            st.active.remove(&self.master);
        }
        if st.activation_generation.get(&self.master).is_some_and(|current| Arc::ptr_eq(current, &self.generation)) {
            st.activation_generation.remove(&self.master);
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
}

pub(super) fn cycle_valid(s: &System, cycle: &Cycle) -> bool {
    s.contains(cycle.cx.ship_id)
        && cycle.target.is_none_or(|target| s.contains(target))
        && state().active.get(&cycle.modules[0].id).is_some_and(|v| Arc::ptr_eq(&v.stop, &cycle.stop))
}

fn cycle_source_current(s: &System, cycle: &Cycle) -> bool {
    !cycle.stop.load(Ordering::Relaxed)
        && s.ball(cycle.cx.ship_id).is_some_and(|ball| ball.char_id == Some(cycle.cx.char_id))
        && state().active.get(&cycle.modules[0].id).is_some_and(|active| Arc::ptr_eq(&active.stop, &cycle.stop))
}

fn cycle_target_lost(s: &System, cycle: &Cycle) -> bool {
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

fn publish_cycle(cycle: &Cycle, outcome: ModuleCycleOutcome) {
    for module in &cycle.modules {
        crate::lua::on_event(GameEvent::ModuleCycleEnded {
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

pub(super) fn retire_activation(active: &mut HashMap<i64, ActiveModule>, master: i64, stop: &Arc<AtomicBool>, publish: impl FnOnce()) -> bool {
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

/// Stop running microwarpdrives after a blocking scrambler lands. Database work
/// runs outside the actor; the actor rechecks the blocker and activation generation.
pub(crate) async fn stop_blocked_propulsion(db: &crate::db::Db, world: &Arc<World>, system: i64, target: i64) {
    let Some(character) = world.with(system, move |s| s.ball(target).and_then(|b| b.char_id)).await.flatten() else { return };
    let modules = match load_items(db, world.sde(), ItemQuery::Location(target)).await {
        Ok(items) => items,
        Err(error) => { warn!("loading blocked propulsion modules: {error}"); return; }
    };
    let modules: Vec<_> = modules.into_iter().filter(|module| {
        super::super::inventory::is_fitting_flag(module.flag_id)
            && world.sde().types.effects(module.type_id).iter().any(|effect| effect.effect_id == EFFECT_MICROWARPDRIVE)
    }).filter_map(|module| {
        let generation = state().active.get(&module.item_id).map(|active| active.stop.clone())?;
        Some((module.item_id, generation))
    }).collect();
    let sde = world.sde().clone();
    world.post(system, move |s| {
        if !propulsion_blocked(&sde, &s.projections_on(target)) { return; }
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



pub(super) fn cycle_start(s: &mut System, mut cycle: Cycle, nominal_start: i64) {
    if cycle.effect.id == i64::from(EFFECT_MICROWARPDRIVE) && propulsion_blocked(cycle.cx.world.sde(), &s.projections_on(cycle.cx.ship_id)) {
        cycle.stop.store(true, Ordering::Relaxed);
        cycle_stop(s, cycle);
        return;
    }
    if matches!(cycle.effect.kind, EffectKind::Projected { .. })
        && s.ball(cycle.cx.ship_id).is_some_and(|ball| ball.mode == MODE_WARP) {
        cycle_stop(s, cycle);
        return;
    }
    if !cycle_valid(s, &cycle) {
        if cycle_target_lost(s, &cycle) { publish_cycle(&cycle, ModuleCycleOutcome::TargetLost); }
        cycle_stop(s, cycle);
        return;
    }
    cycle.nominal_start = nominal_start;
    super::heat::begin_cycle(s, &cycle);
    s.defer(async move { cycle_begin_work(cycle).await; });
}

#[derive(Debug, PartialEq)]
pub(super) enum CycleOutcome {
    Continue,
    Stop(&'static str),
    Ended { reason: &'static str, outcome: ModuleCycleOutcome },
}

fn cycle_discharge(kind: EffectKind, charged: bool, need: f64) -> f64 {
    if matches!(kind, EffectKind::FueledLocalRepair { layer: 0, .. }) && charged { 0.0 } else { need }
}

fn needs_charge(kind: EffectKind) -> bool { matches!(kind, EffectKind::CapBooster | EffectKind::Bomb | EffectKind::Vorton) }

fn charge_sensitive(kind: EffectKind) -> bool {
    matches!(kind, EffectKind::CapBooster | EffectKind::FueledLocalRepair { .. } | EffectKind::Bomb | EffectKind::Vorton)
}

pub(super) fn charge_kind(id: i64, kind: EffectKind) -> EffectKind {
    match id as i32 {
        EFFECT_ANCILLARY_REMOTE_SHIELD_BOOSTER => EffectKind::FueledLocalRepair { layer: 0, amount_attr: repair::amount_attribute(0), charge_bonus_attr: None },
        EFFECT_ANCILLARY_REMOTE_ARMOR_REPAIRER => EffectKind::FueledLocalRepair { layer: 1, amount_attr: repair::amount_attribute(1), charge_bonus_attr: Some(ATTR_CHARGED_ARMOR_DAMAGE_MULTIPLIER) },
        _ => kind,
    }
}

async fn refund_refused_charge(cx: &Ctx, amount: f64) -> Result<(), CallError> {
    if amount > 0.0 {
        inject(&cx.db, &cx.world, cx.system_id, cx.ship_id, amount).await?;
    }
    Ok(())
}


pub(super) async fn cycle_begin_work(mut cycle: Cycle) {
    let cx = cycle.cx.clone();
    let master = cycle.modules[0].id;
    if cycle.effect.kind == EffectKind::Propulsion {
        apply_propulsion(&cx, master, cycle.stop.clone(), true).await;
    }
    let mut public = None;
    let mut check_target = false;
    let result = async {
        let fit = match Fit::load_in_space(&cx.db, &cx.world, cx.system_id, cx.char_id, cx.ship_id).await {
            Ok(fit) => fit,
            Err(_) => return Err("fit unavailable"),
        };
        let resource_kind = charge_kind(cycle.effect.id, cycle.effect.kind);
        if charge_sensitive(resource_kind) && cycle.modules.iter().any(|m| state().reloading.get(&m.id).is_some_and(|until| *until > filetime_now())) {
            return Err("module reloading");
        }
        if charge_sensitive(resource_kind)
            && !crate::dao::items::pending_reloads::for_flags(&cx.db, cx.ship_id, &[cycle.modules[0].flag]).await.map_err(|_| "reload unavailable")?.is_empty() {
            return Err("module reloading");
        }
        let charge = if charge_sensitive(resource_kind) {
            loaded(&cx.db, cx.ship_id, cycle.modules[0].flag).await.map_err(|_| "charge unavailable")?.filter(|(_, quantity)| *quantity > 0)
        } else { None };
        if charge_sensitive(resource_kind) && charge.map(|(kind, _)| kind) != fit.source_type(Key::Charge(master)) {
            return Err("charge changed since fit snapshot");
        }
        if needs_charge(cycle.effect.kind) && charge.is_none() { return Err("out of charges"); }
        let charged = charge.is_some();
        if charge_sensitive(resource_kind) { cycle.charge_type = charge.map(|(kind, _)| kind); }
        if charge_sensitive(resource_kind) {
            cycle.other = charge.map(|(kind, _)| charge_key(cx.ship_id, cycle.modules[0].flag, kind));
        }
        cycle.duration_ms = cycle.effect.attribute(EffectAttribute::Duration).map(|a| fit.value(Key::Module(master), a)).unwrap_or(cycle.duration_ms).max(MIN_MODULE_DURATION_MS);
        if matches!(cycle.effect.kind, EffectKind::CapTransfer | EffectKind::Neutralizer | EffectKind::Nosferatu)
            && capacitor::validate(&cx, &fit, &cycle.modules[0], &cycle.effect, cycle.target).await.is_err() {
            check_target = true;
            return Err("target lost");
        }
        if matches!(cycle.effect.kind, EffectKind::Ecm | EffectKind::BurstJammer) && ecm::validate(&cx, &fit, &cycle.modules[0], &cycle.effect, cycle.target).await.is_err() { check_target = true; return Err("target lost"); }
        let discharge = cycle.effect.attribute(EffectAttribute::Discharge).unwrap_or(ATTR_CAPACITOR_NEED);
        let need = cycle_discharge(resource_kind, charged, cycle.modules.iter().map(|m| fit.value(Key::Module(m.id), discharge)).sum());
        if !drain_capacitor(&cx, &fit, need) {
            cx.notify("OnEffectMessage", vec![Value::str("NotEnoughCharge"), Value::Dict(vec![])]);
            public = Some(ModuleCycleOutcome::OutOfCapacitor);
            return Err("out of capacitor");
        }
        match on_cycle_start(&cx, &fit, &mut cycle, charged).await {
            Ok(CycleOutcome::Continue) => {}
            Ok(result) => {
                let reason = match result {
                    CycleOutcome::Stop(reason) => reason,
                    CycleOutcome::Ended { reason, outcome } => { public = Some(outcome); reason }
                    CycleOutcome::Continue => unreachable!(),
                };
                if (charge_sensitive(resource_kind) && matches!(reason, "out of charges" | "source lost or in warp")
                    || matches!(cycle.effect.kind, EffectKind::RemoteRepair { .. } | EffectKind::CapTransfer | EffectKind::Neutralizer | EffectKind::Nosferatu) && reason == "target lost") && need > 0.0 {
                    if let Err(error) = refund_refused_charge(&cx, need).await {
                        warn!("module {master} capacitor refund failed: {}", match error { CallError::Internal(e) => format!("{e:#}"), CallError::User { msg, .. } => msg });
                    }
                }
                return Err(reason);
            }
            Err(e) => { warn!("module {master} cycle start failed: {}", match e { CallError::Internal(e) => format!("{e:#}"), CallError::User { msg, .. } => msg }); return Err("phase failed"); }
        }
        if let Err(e) = super::heat::connect_cycle(&cx, &fit, &cycle).await {
            warn!("module {master} heat connection failed: {}", match e { CallError::Internal(e) => format!("{e:#}"), CallError::User { msg, .. } => msg });
            return Err("heat connection failed");
        }
        if cycle.effect.kind == EffectKind::Smartbomb || cycle.cycles == 0 && matches!(cycle.effect.kind, EffectKind::Bomb | EffectKind::FueledLocalRepair { .. } | EffectKind::RemoteRepair { .. } | EffectKind::CapTransfer | EffectKind::Neutralizer | EffectKind::Nosferatu | EffectKind::CapBooster | EffectKind::Ecm | EffectKind::BurstJammer) {
            weapon_fx(&cx, &cycle.modules, &cycle.effect, cycle.target, cycle.charge_type, true, cycle.duration_ms, cycle.repeat);
        }
        Ok(fit)
    }.await;
    match result {
        Ok(fit) => {
            cycle.cycles += 1;
            let (start, duration) = (cycle.nominal_start, cycle.duration_ms);
            cx.world.post(cx.system_id, move |s| {
                schedule_cycle_end(s, start, duration, move |s, end| cycle_end(s, cycle, fit, end));
            });
        }
        Err(reason) => {
            debug!("module {master} stopped: {reason}");
            cx.world.post(cx.system_id, move |s| {
                if cycle_source_current(s, &cycle) {
                    if let Some(outcome) = public { publish_cycle(&cycle, outcome); }
                    else if check_target && cycle_target_lost(s, &cycle) { publish_cycle(&cycle, ModuleCycleOutcome::TargetLost); }
                }
                cycle_stop(s, cycle);
            });
        }
    }
}

pub(super) fn cycle_end(s: &mut System, cycle: Cycle, fit: Fit, end: i64) {
    if !cycle_valid(s, &cycle) {
        if cycle_target_lost(s, &cycle) { publish_cycle(&cycle, ModuleCycleOutcome::TargetLost); }
        cycle_stop(s, cycle);
        return;
    }
    let final_cycle = cycle_is_final(cycle.stop.load(Ordering::Relaxed), cycle.effect.has(EffectFlag::DisallowAutoRepeat), cycle.repeat, cycle.cycles);
    let Some(receiver) = super::heat::finish_cycle(s, &cycle, end, final_cycle) else { cycle_stop(s, cycle); return };
    s.defer(async move { cycle_end_work(cycle, fit, end, final_cycle, receiver).await; });
}

/// Family end, then heat damage, then one actor closure that completes heat
/// immediately before the repeat or the retirement.
async fn cycle_end_work(mut cycle: Cycle, fit: Fit, end: i64, final_cycle: bool, receiver: tokio::sync::oneshot::Receiver<Option<Vec<super::heat::SourceSnapshot>>>) {
    let cx = cycle.cx.clone();
    let captured = match receiver.await {
        Ok(Some(sources)) => Some(sources),
        Ok(None) | Err(_) => None,
    };
    let mut public = None;
    let (sources, finished) = if let Some(sources) = captured {
        let mut finished = match on_cycle_end(&cx, &fit, &mut cycle).await {
            Ok(CycleOutcome::Continue) => { public = Some(ModuleCycleOutcome::Completed); final_cycle }
            Ok(CycleOutcome::Ended { outcome, .. }) => { public = Some(outcome); true }
            Ok(CycleOutcome::Stop(_)) => true,
            Err(e) => { warn!("module {} cycle end failed: {}", cycle.modules[0].id, match e { CallError::Internal(e) => format!("{e:#}"), CallError::User { msg, .. } => msg }); true }
        };
        if let Err(e) = super::heat::damage::apply(&cx, sources).await {
            warn!("module {} heat damage failed: {}", cycle.modules[0].id, match e { CallError::Internal(e) => format!("{e:#}"), CallError::User { msg, .. } => msg });
            finished = true;
        }
        (true, finished)
    } else {
        (false, true)
    };
    cx.world.post(cx.system_id, move |s| {
        let continuing = boundary_continues(sources, finished, &cycle.stop);
        super::heat::complete_cycle(s, &cycle, !continuing);
        if let Some(outcome) = public { publish_cycle(&cycle, outcome); }
        if continuing {
            cycle_start(s, cycle, end);
        } else {
            cycle_stop(s, cycle);
        }
    });
}

pub(super) fn cycle_stop(s: &mut System, mut cycle: Cycle) {
    let master = cycle.modules[0].id;
    let cx = cycle.cx.clone();
    debug!("module {master} stopped after {} cycles", cycle.cycles);
    super::heat::stop_cycle(s, &cycle);
    let mut st = state();
    let retired = retire_activation(&mut st.active, master, &cycle.stop, || {
        ship_effect(&cx, master, cycle.effect.id, cycle.target, cycle.other.clone(), false, cycle.started, cycle.duration_ms, cycle.repeat);
    });
    if retired && cycle.effect.kind != EffectKind::Propulsion && st.activation_generation.get(&master).is_some_and(|current| Arc::ptr_eq(current, &cycle.stop)) {
        st.activation_generation.remove(&master);
    }
    if retired { cycle.owner.armed = false; }
    drop(st);
    if retired && matches!(cycle.effect.kind, EffectKind::Cloak | EffectKind::MicroJumpDrive) {
        s.refresh_projection_target(cx.ship_id);
    }
    if retired { on_stop(s, &cx, &cycle); }
    if retired && matches!(charge_kind(cycle.effect.id, cycle.effect.kind), EffectKind::FueledLocalRepair { .. }) {
        s.defer(async move {
            if let Err(error) = crate::services::combat::targeting_charges::auto_reload(&cx, master).await {
                warn!("module {master} automatic reload failed: {}", match error { CallError::Internal(e) => format!("{e:#}"), CallError::User { msg, .. } => msg });
            }
        });
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

pub(super) async fn subject_valid(cx: &Ctx, module: &Module, effect: &Effect) -> Result<bool, CallError> {
    let policy = OnlinePolicy { owner: cx.char_id, active_ship: Some(cx.ship_id), station: None, notify: cx.notify.clone() };
    match subject_core(&cx.db, cx.world.sde(), &policy, cx.ship_id, module.id, effect.id as i32, true).await {
        Ok((_, item)) => Ok(item.type_id == module.type_id && item.flag_id == module.flag),
        Err(CallError::User { .. }) => Ok(false),
        Err(error) => Err(error),
    }
}

pub(super) async fn subject(db: &Db, sde: &Sde, policy: &OnlinePolicy, ship: i64, item_id: i64, effect: i32, require_online: bool) -> Result<Item, CallError> {
    Ok(subject_core(db, sde, policy, ship, item_id, effect, require_online).await?.1)
}

/// Activate an effect. `login_user` feeds the activation, online and hacking-start info logs.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn activate(
    identity: &Identity, db: &Db, world: &Arc<World>, sde: &Arc<Sde>, space_system: Option<i64>,
    sender: NotifyTx, login_user: i64, mut item_id: i64, effect_name: &str, target: Option<i64>, repeat: i64,
    notify: impl FnMut(Notification) -> Result<(), CallError> + Send,
) -> CallResult {
    let Some(cx) = Ctx::from_identity(identity, db, world, space_system, sender.clone()) else { return Err(notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION)) };
    let module = load_items(db, sde, ItemQuery::Id(item_id)).await.map_err(db_err)?.into_iter().next()
        .ok_or_else(|| notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION))?;
    let mut matches = cx.world.sde().dogma.effects_by_name(effect_name);
    let Some(row) = matches.next() else { return Err(unsupported::refusal(None, effect_name, module.type_id)) };
    if matches.next().is_some() { return Err(CallError::Internal(anyhow::anyhow!("multiple effects named {effect_name}"))); }
    drop(matches);
    let id = row.effect_id;
    // Fitting mutations and non-online activation share this lock through registration and FX queuing.
    // Online activation takes it inside set_online instead.
    let _lock = if id != EFFECT_ONLINE as i32 { Some(crate::services::market::lock().await) } else { None };
    let policy = OnlinePolicy::from_identity(identity, sender.clone())?;
    let mut module = subject(db, sde, &policy, cx.ship_id, item_id, id, id != EFFECT_ONLINE as i32).await?;
    let Some(effect) = registry::resolve(cx.world.sde(), id) else {
        return Err(unsupported::refusal(Some(id), effect_name, module.type_id));
    };
    let kind = effect.kind;
    let target = if kind == EffectKind::BurstJammer { None } else { target };
    if id == EFFECT_MUTADAPTIVE_REMOTE_ARMOR_REPAIRER || !registry::activation_in_scope(module.category_id, module.group_id, kind) || !effect.manually_activatable() {
        return Err(unsupported::refusal(Some(id), effect_name, module.type_id));
    }
    // Bank selection is serialized with fitting mutations by the shared lock above.
    if matches!(kind, EffectKind::Turret | EffectKind::Missile) {
        let master = crate::services::combat::targeting_charges::master_for_member(db, cx.ship_id, item_id).await?;
        if master != item_id {
            module = subject(db, sde, &policy, cx.ship_id, master, id, true).await?;
            if !registry::activation_in_scope(module.category_id, module.group_id, kind) {
                return Err(unsupported::refusal(Some(id), effect_name, module.type_id));
            }
            item_id = master;
        }
    }
    match kind {
        EffectKind::Hacking => {
            super::cloak::before_activate(&cx, item_id, kind).await?;
            return super::super::hacking::start(identity, db, world, sde, space_system, sender, login_user, item_id, target, notify).await;
        }
        EffectKind::Online => {
            super::cloak::before_activate(&cx, item_id, kind).await?;
            return super::online::set_online(identity, db, sde, sender, login_user, cx.ship_id, item_id, true, notify).await;
        }
        EffectKind::Modifiers | EffectKind::Turret | EffectKind::Missile |
        EffectKind::LocalRepair { .. } | EffectKind::FueledLocalRepair { .. } |
        EffectKind::Propulsion | EffectKind::Mining | EffectKind::Projected { .. } | EffectKind::CapTransfer |
        EffectKind::Neutralizer | EffectKind::Nosferatu | EffectKind::CapBooster | EffectKind::RemoteRepair { .. } |
        EffectKind::Ecm | EffectKind::BurstJammer |
        EffectKind::Smartbomb | EffectKind::Vorton | EffectKind::Cloak |
        EffectKind::MicroJumpDrive | EffectKind::Salvage | EffectKind::Tractor |
        EffectKind::CargoScan | EffectKind::ShipScan | EffectKind::SurveyScan |
        EffectKind::AutoTarget | EffectKind::PassiveTarget | EffectKind::Bomb | EffectKind::Compression => {}
        EffectKind::ProbeLaunch => return Err(unsupported::refusal(Some(id), effect_name, module.type_id)),
    }
    super::cloak::before_activate(&cx, item_id, kind).await?;
    if state().reloading.get(&item_id).is_some_and(|until| *until > filetime_now()) {
        return Err(user_error(messages::MSG_CANT_ACTIVATE_RELOADING_MODULE, vec![("moduleTypeID".into(), Value::tuple([Value::Int(UE_TYPEID), Value::Int(module.type_id as i64)]))]));
    }
    let fit = Fit::load_in_space(&cx.db, &cx.world, cx.system_id, cx.char_id, cx.ship_id).await?;
    let activation = Activation { effect: effect.clone(), module: Module { id: item_id, type_id: module.type_id, flag: module.flag_id }, target };
    if state().active.contains_key(&item_id) {
        return Ok(Value::None);
    }
    let duration_ms = effect.attribute(EffectAttribute::Duration).map(|a| fit.value(Key::Module(item_id), a)).unwrap_or(DEFAULT_MODULE_DURATION_MS).max(MIN_MODULE_DURATION_MS);
    // The group fires together.
    let mut modules = vec![Module { id: item_id, type_id: module.type_id, flag: module.flag_id }];
    if matches!(kind, EffectKind::Turret | EffectKind::Missile) {
        for slave in bank(&cx.db, cx.ship_id, item_id).await?.into_iter().skip(1) {
            let m = subject(db, sde, &policy, cx.ship_id, slave, id, true).await?;
            modules.push(Module { id: m.item_id, type_id: m.type_id, flag: m.flag_id });
        }
    }
    for module in &modules { super::heat::damage::validate(&fit, module.id)?; }
    let masters: Vec<i64> = state().active.iter().filter(|(_, active)| active.char_id == cx.char_id && !active.forced_inactive).map(|(&id, _)| id).collect();
    let mut active = Vec::new();
    for master in masters { active.extend(bank(&cx.db, cx.ship_id, master).await?); }
    active.sort_unstable();
    active.dedup();
    let activating: Vec<i64> = modules.iter().map(|module| module.id).collect();
    super::fitting::validate_active(&cx.db, sde.clone(), &fit, cx.ship_id, &active, &activating).await?;
    on_activate(&cx, &fit, &activation).await?;
    if matches!(kind, EffectKind::Turret | EffectKind::Missile | EffectKind::Neutralizer | EffectKind::Nosferatu) {
        let t = target.unwrap_or(0);
        let ship = cx.ship_id;
        let (own_type, victim) = cx.world.with(cx.system_id, move |s| {
            (s.ball(ship).map(|b| b.type_id).unwrap_or(0), s.ball(t).and_then(|b| b.char_id))
        }).await.unwrap_or((0, None));
        if let Some(victim) = victim {
            super::super::crimewatch::aggress(&cx.db, &cx.world, cx.system_id, cx.char_id, own_type, module.type_id, victim, t).await?;
        }
    }
    let other = loaded(&cx.db, cx.ship_id, module.flag_id).await?.map(|(type_id, _)| charge_key(cx.ship_id, module.flag_id, type_id));
    let stop = Arc::new(AtomicBool::new(false));
    {
        let mut st = state();
        if st.active.contains_key(&item_id) { return Ok(Value::None); }
        if st.reloading.get(&item_id).is_some_and(|until| *until > filetime_now()) {
            return Err(user_error(messages::MSG_CANT_ACTIVATE_RELOADING_MODULE, vec![("moduleTypeID".into(), Value::tuple([Value::Int(UE_TYPEID), Value::Int(module.type_id as i64)]))]));
        }
        st.active.insert(item_id, ActiveModule { char_id: cx.char_id, stop: stop.clone(), forced_inactive: false });
        st.activation_generation.insert(item_id, stop.clone());
    }
    let ship = cx.ship_id;
    cx.world.post(cx.system_id, move |s| {
        s.drop_jump_cloak(ship);
        if matches!(kind, EffectKind::Cloak | EffectKind::MicroJumpDrive) {
            s.refresh_projection_target(ship);
        }
    });
    let owner = ActivationOwner { master: item_id, generation: stop.clone(), armed: true };
    ship_effect(&cx, item_id, effect.id, target, other.clone(), true, filetime_now(), duration_ms, repeat);
    info!(user = login_user, "module {item_id} activated {effect_name} on {target:?} (x{}, {duration_ms:.0} ms, repeat {repeat})", modules.len());
    let world = cx.world.clone();
    let cycle = Cycle { cx, modules, effect, target, repeat, stop, owner, started: filetime_now(), charge_type: None, cycles: 0, duration_ms, nominal_start: 0, other, repair_amount: 0.0 };
    world.post(cycle.cx.system_id, move |s| {
        let now = s.now_ms();
        cycle_start(s, cycle, now);
    });
    Ok(Value::None)
}


/// Stop after the current cycle. `login_user` is read only by the online/offline info log.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn deactivate(
    identity: &Identity, db: &Db, world: &Arc<World>, sde: &Arc<Sde>, space_system: Option<i64>,
    sender: NotifyTx, login_user: i64, item_id: i64, effect_name: &str,
    notify: impl FnMut(Notification) -> Result<(), CallError> + Send,
) -> CallResult {
    let Some(cx) = Ctx::from_identity(identity, db, world, space_system, sender.clone()) else { return Err(notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION)) };
    let module = load_items(db, sde, ItemQuery::Id(item_id)).await.map_err(db_err)?.into_iter().next().ok_or_else(|| notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION))?;
    let name = effect_name;
    let mut rows = sde.dogma.effects_by_name(name);
    let Some(row) = rows.next() else { return Err(unsupported::refusal(None, name, module.type_id)) };
    if rows.next().is_some() { return Err(CallError::Internal(anyhow::anyhow!("multiple effects named {name}"))); }
    let id = row.effect_id;
    drop(rows);
    let policy = OnlinePolicy::from_identity(identity, sender.clone())?;
    let module = subject(db, sde, &policy, cx.ship_id, item_id, id, false).await?;
    let Some(kind) = registry::kind(sde, id) else { return Err(unsupported::refusal(Some(id), name, module.type_id)) };
    if !sde.types.effects(module.type_id).iter().any(|effect| effect.effect_id == id)
        || !registry::activation_in_scope(module.category_id, module.group_id, kind) {
        return Err(unsupported::refusal(Some(id), name, module.type_id));
    }
    if kind == EffectKind::Online { return super::online::set_online(identity, db, sde, sender, login_user, cx.ship_id, item_id, false, notify).await; }
    if let Some(stop) = state().active.get(&item_id) {
        stop_generation(&stop.stop);
    }
    if kind == EffectKind::Cloak {
        let generation = state().active.get(&item_id).map(|active| active.stop.clone());
        let ship = cx.ship_id;
        cx.world.post(cx.system_id, move |s| {
            if let Some(generation) = generation { s.stop_module_cloak(ship, item_id, &generation); }
        });
    }
    if matches!(kind, EffectKind::Projected { .. }) {
        let generation = state().active.get(&item_id).map(|active| active.stop.clone());
        cx.world.post(cx.system_id, move |s| {
            if generation.as_ref().is_some_and(|generation| state().active.get(&item_id).is_some_and(|active| Arc::ptr_eq(&active.stop, generation))) {
                s.remove_projection(item_id);
            }
        });
    }
    Ok(Value::None)
}

#[cfg(test)] mod tests;
