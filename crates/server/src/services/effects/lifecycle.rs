//! Actor-owned activation cycles for supported dogma effects.
//! Mirrors clientDogmaLocation and godma; effect metadata comes from the SDE.
//! Subject authorization is shared by activation, deactivation, analyzers and online changes.
//! Projected effects require a locked, present target and use transient system state;
//! Later-wave behavior families are refused; fitting limits use the shared policy.
//! Accepted running activations cancel arrival jump cloak; online changes are not cycles.
//! Offlining a bank member destroys the whole persisted bank; unfit removes a slave
//! or destroys a master's bank. Committed changes stop affected cycles after the
//! current cycle, and slave weapon activation resolves to its authorized master.
//! Cycles are runtime-only and are not resumed after restart.
//! A non-targeted module's first-cycle FX may be unseen by observers until the next
//! destiny boundary applies the queued uncloak (under one second); gameplay is unaffected.
use super::{EffectKind, registry, unsupported, repair, mining, capacitor, projected, range};
use super::registry::{Effect, EffectAttribute, EffectFlag};
use super::notify::{fire_fx, ship_effect};
use super::propulsion::apply_propulsion;
use super::weapons::{consume_charge, empty_charge, fire_missile, fire_turret};
use crate::services::combat::{ctx, state, ActiveModule, Ctx};
use crate::services::combat::groups_health::{drain_capacitor, inject};
use crate::services::combat::targeting_charges::{bank, charge_key, loaded, targets_of};
use crate::services::fit::{Fit, Key};
use crate::services::items::load_items;
use crate::services::{db_err, notify_error, user_error, Call, CallError, CallResult};
use crate::dao::items::loading::{Item, ItemQuery};
use crate::client::{filetime_now, Client};
use crate::consts::{ATTR_ACTIVATION_BLOCKED, ATTR_CAPACITOR_NEED, CATEGORY_MODULE, CATEGORY_SHIP, EFFECT_MICROWARPDRIVE, EFFECT_ONLINE, FLAG_HANGAR};
use crate::consts::{EFFECT_ANCILLARY_REMOTE_ARMOR_REPAIRER, EFFECT_ANCILLARY_REMOTE_SHIELD_BOOSTER, EFFECT_MUTADAPTIVE_REMOTE_ARMOR_REPAIRER, ATTR_CHARGED_ARMOR_DAMAGE_MULTIPLIER};
use crate::messages;
use crate::space::{ms_to_seconds, seconds_to_ms, System, World, MODE_WARP};
use marshal::Value;
use std::collections::HashMap;
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
use tracing::{debug, info, warn};
use crate::db::executor::Executor;
use crate::sde::Sde;
use super::online::OnlinePolicy;


pub(in crate::services) fn stop_generation(stop: &AtomicBool) {
    stop.store(true, Ordering::Relaxed);
}

/// Existing server module duration fallback, in milliseconds.
const DEFAULT_MODULE_DURATION_MS: f64 = 1000.0;
/// Existing server minimum module cycle duration.
const MIN_MODULE_DURATION_MS: f64 = 250.0;
/// Client UserError type-ID substitution tag.
const UE_TYPEID: i64 = 4;



#[derive(Clone)]
pub(super) struct Module {
    pub(super) id: i64,
    pub(super) type_id: i32,
    pub(super) flag: i32,
}
#[allow(clippy::too_many_arguments)]
fn weapon_fx(cx: &Ctx, modules: &[Module], effect: &Effect, target: Option<i64>, charge: Option<i32>, start: bool, duration_ms: f64, repeat: i64) {
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

pub(super) fn cycle_deadline(start: i64, duration_ms: f64) -> i64 {
    start.saturating_add(seconds_to_ms(ms_to_seconds(duration_ms)) as i64)
}

pub(super) fn cycle_is_final(stopped: bool, disallow_auto_repeat: bool, repeat: i64, cycles: i64) -> bool {
    stopped || disallow_auto_repeat || repeat <= 0 || cycles >= repeat
}

pub(super) fn cycle_should_stop(finished: bool, stop: &AtomicBool) -> bool {
    finished || stop.load(Ordering::Relaxed)
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
        cycle_stop(s, cycle);
        return;
    }
    cycle.nominal_start = nominal_start;
    s.defer(async move { cycle_begin_work(cycle).await; });
}

#[derive(Debug, PartialEq)]
pub(super) enum CycleOutcome { Continue, Stop(&'static str) }

fn cycle_discharge(kind: EffectKind, charged: bool, need: f64) -> f64 {
    if matches!(kind, EffectKind::FueledLocalRepair { layer: 0, .. }) && charged { 0.0 } else { need }
}

fn needs_charge(kind: EffectKind) -> bool { matches!(kind, EffectKind::CapBooster) }

fn charge_sensitive(kind: EffectKind) -> bool {
    matches!(kind, EffectKind::CapBooster | EffectKind::FueledLocalRepair { .. })
}

fn charge_kind(id: i64, kind: EffectKind) -> EffectKind {
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

fn repair_ranges(fit: &Fit, cycle: &Cycle) -> (f64, f64) {
    let module = Key::Module(cycle.modules[0].id);
    (cycle.effect.attribute(EffectAttribute::Range).map(|a| fit.value(module, a)).unwrap_or(0.0),
     cycle.effect.attribute(EffectAttribute::Falloff).map(|a| fit.value(module, a)).unwrap_or(0.0))
}

async fn remote_repair(cx: &Ctx, fit: &Fit, cycle: &Cycle, layer: usize) -> Result<CycleOutcome, CallError> {
    let (optimal, falloff) = repair_ranges(fit, cycle);
    let Ok((target, pilot, multiplier)) = repair::target(cx, cycle.target, optimal, falloff).await else {
        return Ok(CycleOutcome::Stop("target lost"));
    };
    let master = cycle.modules[0].id;
    if !state().active.get(&master).is_some_and(|active| Arc::ptr_eq(&active.stop, &cycle.stop)) {
        return Ok(CycleOutcome::Stop("activation lost"));
    }
    let generation = cycle.stop.clone();
    let source = cx.ship_id;
    let character = cx.char_id;
    let sde = cx.world.sde().clone();
    let guard = move |s: &System| {
        if !state().active.get(&master).is_some_and(|active| Arc::ptr_eq(&active.stop, &generation))
            || !targets_of(character).contains(&target) { return false; }
        let Some((ship, victim)) = s.sampled_ball(source).zip(s.sampled_ball(target)) else { return false; };
        if ship.mode == MODE_WARP || victim.mode == MODE_WARP || victim.char_id != pilot { return false; }
        let category = sde.types.get(victim.type_id).and_then(|t| t.category_id);
        if category != Some(CATEGORY_SHIP) && !(category == Some(crate::consts::CATEGORY_ENTITY) && s.is_npc(target)) { return false; }
        let gap = (crate::space::dist(ship.pos, victim.pos) - f64::from(ship.radius) - f64::from(victim.radius)).max(0.0);
        range::range_multiplier(gap, optimal, falloff) > 0.0
    };
    let amount = cycle.repair_amount * multiplier;
    if let Some(pilot) = pilot {
        crate::services::combat::groups_health::repair_player(&cx.db, &cx.world, cx.system_id, target, pilot, layer, amount, guard).await?;
    } else {
        let sde = cx.world.sde().clone();
        cx.world.with(cx.system_id, move |s| {
            if guard(s) { crate::services::combat::groups_health::repair_npc_in_system(s, &sde, target, layer, amount); }
        }).await;
    }
    Ok(CycleOutcome::Continue)
}

async fn on_cycle_start(cx: &Ctx, fit: &Fit, cycle: &mut Cycle, charged: bool) -> Result<CycleOutcome, CallError> {
    use EffectKind::*;
    match cycle.effect.kind {
        Turret | Missile => {
            let target = cycle.target.unwrap_or(0);
            if !targets_of(cx.char_id).contains(&target) { return Ok(CycleOutcome::Stop("target lost")); }
            let mut fired = false;
            for module in &cycle.modules {
                let charge = match consume_charge(cx, module, None).await { Ok(Some(t)) => t, _ => continue };
                cycle.charge_type = Some(charge);
                fired = true;
                let result = if cycle.effect.kind == Turret {
                    fire_turret(cx, fit, module, target).await
                } else {
                    fire_missile(cx, fit, module, charge, target).await
                };
                if let Err(e) = result {
                    warn!("weapon {} failed: {}", module.id, match e {
                        CallError::Internal(e) => format!("{e:#}"), CallError::User { msg, .. } => msg,
                    });
                }
            }
            if !fired { return Ok(CycleOutcome::Stop("out of charges")); }
            if cycle.cycles == 0 {
                weapon_fx(cx, &cycle.modules, &cycle.effect, Some(target), cycle.charge_type, true, cycle.duration_ms, cycle.repeat);
            }
            Ok(CycleOutcome::Continue)
        }
        LocalRepair { layer, amount_attr } => {
            cycle.repair_amount = repair::amount(fit, cycle.modules[0].id, amount_attr);
            repair::apply(cx, layer, cycle.repair_amount, true).await;
            Ok(CycleOutcome::Continue)
        }
        FueledLocalRepair { layer, amount_attr, charge_bonus_attr } => {
            let module = &cycle.modules[0];
            if !charged && !empty_charge(cx, module).await? { return Ok(CycleOutcome::Stop("out of charges")); }
            cycle.repair_amount = repair::fueled(fit, module.id, amount_attr, charge_bonus_attr, charged);
            cycle.charge_type = if charged {
                match consume_charge(cx, module, cycle.charge_type).await? {
                    Some(kind) => Some(kind),
                    None => return Ok(CycleOutcome::Stop("out of charges")),
                }
            } else { None };
            repair::apply(cx, layer, cycle.repair_amount, true).await;
            Ok(CycleOutcome::Continue)
        }
        RemoteRepair { layer } => {
            let (optimal, falloff) = repair_ranges(fit, cycle);
            if repair::target(cx, cycle.target, optimal, falloff).await.is_err() {
                return Ok(CycleOutcome::Stop("target lost"));
            }
            let module = &cycle.modules[0];
            cycle.repair_amount = if let FueledLocalRepair { amount_attr, charge_bonus_attr, .. } = charge_kind(cycle.effect.id, cycle.effect.kind) {
                if !charged && !empty_charge(cx, module).await? { return Ok(CycleOutcome::Stop("out of charges")); }
                let amount = repair::fueled(fit, module.id, amount_attr, charge_bonus_attr, charged);
                cycle.charge_type = if charged {
                    match consume_charge(cx, module, cycle.charge_type).await? {
                        Some(kind) => Some(kind),
                        None => return Ok(CycleOutcome::Stop("out of charges")),
                    }
                } else { None };
                amount
            } else { repair::amount(fit, module.id, repair::amount_attribute(layer)) };
            if repair::applies_at_start(layer) { remote_repair(cx, fit, cycle, layer).await } else { Ok(CycleOutcome::Continue) }
        }
        CapTransfer | Neutralizer | Nosferatu | CapBooster => capacitor::start(cx, fit, cycle).await,
        Mining => Ok(mining::start(cx, cycle)),
        Projected { .. } => {
            let target = cycle.target.unwrap_or(0);
            if !targets_of(cx.char_id).contains(&target) { return Ok(CycleOutcome::Stop("target lost")); }
            for module in &cycle.modules {
                let charge = fit.source_type(Key::Charge(module.id));
                projected::apply(cx, fit, module.id, module.type_id, &cycle.effect, target, cycle.duration_ms, charge).await?;
            }
            Ok(CycleOutcome::Continue)
        }
        Propulsion | Modifiers => Ok(CycleOutcome::Continue),
        Smartbomb | Vorton | Ecm | BurstJammer |
        Cloak | MicroJumpDrive | Salvage | Tractor | CargoScan | ShipScan | SurveyScan |
        AutoTarget | PassiveTarget | Bomb | ProbeLaunch | Hacking | Online => unreachable!("unsupported cycle kind"),
    }
}

async fn on_cycle_end(cx: &Ctx, fit: &Fit, cycle: &mut Cycle) -> Result<CycleOutcome, CallError> {
    use EffectKind::*;
    match cycle.effect.kind {
        LocalRepair { layer, .. } | FueledLocalRepair { layer, .. } => {
            repair::apply(cx, layer, cycle.repair_amount, false).await;
            Ok(CycleOutcome::Continue)
        }
        RemoteRepair { layer } => {
            if repair::applies_at_start(layer) { Ok(CycleOutcome::Continue) } else { remote_repair(cx, fit, cycle, layer).await }
        }
        Mining => Ok(mining::end(cx, fit, cycle).await),
        Modifiers | Turret | Missile | Propulsion | Projected { .. } | CapTransfer | Neutralizer | Nosferatu | CapBooster => Ok(CycleOutcome::Continue),
        Smartbomb | Vorton | Ecm | BurstJammer |
        Cloak | MicroJumpDrive | Salvage | Tractor | CargoScan | ShipScan | SurveyScan |
        AutoTarget | PassiveTarget | Bomb | ProbeLaunch | Hacking | Online => unreachable!("unsupported cycle kind"),
    }
}

fn on_stop(s: &mut System, cx: &Ctx, cycle: &Cycle) {
    use EffectKind::*;
    match cycle.effect.kind {
        Turret | Missile | FueledLocalRepair { .. } | RemoteRepair { .. } | CapTransfer | Neutralizer | Nosferatu | CapBooster => {
            if let Some(guid) = cycle.effect.guid.as_deref().and_then(|g| g.split(',').next()) {
                let events = cycle.modules.iter().map(|m| fire_fx(s.current_time(), cx.ship_id, Some((m.id, m.type_id)), guid, cycle.target, cycle.charge_type, cycle.effect.has(EffectFlag::Offensive), false, cycle.duration_ms, cycle.repeat)).collect();
                s.broadcast(cx.ship_id, events);
            }
        }
        Mining => mining::stop(s, cx, cycle),
        Projected { .. } => {
            for module in &cycle.modules { s.remove_projection(module.id); }
        }
        Propulsion => {
            let cx = cx.clone();
            let master = cycle.modules[0].id;
            let generation = cycle.stop.clone();
            s.defer(async move { apply_propulsion(&cx, master, generation, false).await; });
        }
        Modifiers | LocalRepair { .. } => {}
        Smartbomb | Vorton | Ecm | BurstJammer |
        Cloak | MicroJumpDrive | Salvage | Tractor | CargoScan | ShipScan | SurveyScan |
        AutoTarget | PassiveTarget | Bomb | ProbeLaunch | Hacking | Online => unreachable!("unsupported cycle kind"),
    }
}

pub(super) async fn cycle_begin_work(mut cycle: Cycle) {
    let cx = cycle.cx.clone();
    let master = cycle.modules[0].id;
    if cycle.cycles == 0 && cycle.effect.kind == EffectKind::Propulsion {
        apply_propulsion(&cx, master, cycle.stop.clone(), true).await;
    }
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
            return Err("target lost");
        }
        let discharge = cycle.effect.attribute(EffectAttribute::Discharge).unwrap_or(ATTR_CAPACITOR_NEED);
        let need = cycle_discharge(resource_kind, charged, cycle.modules.iter().map(|m| fit.value(Key::Module(m.id), discharge)).sum());
        if !drain_capacitor(&cx, &fit, need) {
            cx.notify("OnEffectMessage", vec![Value::str("NotEnoughCharge"), Value::Dict(vec![])]);
            return Err("out of capacitor");
        }
        match on_cycle_start(&cx, &fit, &mut cycle, charged).await {
            Ok(CycleOutcome::Continue) => {}
            Ok(CycleOutcome::Stop(reason)) => {
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
        if cycle.cycles == 0 && matches!(cycle.effect.kind, EffectKind::FueledLocalRepair { .. } | EffectKind::RemoteRepair { .. } | EffectKind::CapTransfer | EffectKind::Neutralizer | EffectKind::Nosferatu | EffectKind::CapBooster) {
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
            cx.world.post(cx.system_id, move |s| cycle_stop(s, cycle));
        }
    }
}

pub(super) fn cycle_end(s: &mut System, mut cycle: Cycle, fit: Fit, end: i64) {
    if !cycle_valid(s, &cycle) { cycle_stop(s, cycle); return; }
    let final_cycle = cycle_is_final(cycle.stop.load(Ordering::Relaxed), cycle.effect.has(EffectFlag::DisallowAutoRepeat), cycle.repeat, cycle.cycles);
    s.defer(async move {
        let cx = cycle.cx.clone();
        let finished = match on_cycle_end(&cx, &fit, &mut cycle).await {
            Ok(CycleOutcome::Continue) => final_cycle,
            Ok(CycleOutcome::Stop(_)) => true,
            Err(e) => { warn!("module {} cycle end failed: {}", cycle.modules[0].id, match e { CallError::Internal(e) => format!("{e:#}"), CallError::User { msg, .. } => msg }); true }
        };
        cx.world.post(cx.system_id, move |s| {
            if cycle_should_stop(finished, &cycle.stop) { cycle_stop(s, cycle); }
            else { cycle_start(s, cycle, end); }
        });
    });
}

pub(super) fn cycle_stop(s: &mut System, mut cycle: Cycle) {
    let master = cycle.modules[0].id;
    let cx = cycle.cx.clone();
    debug!("module {master} stopped after {} cycles", cycle.cycles);
    let mut st = state();
    let retired = retire_activation(&mut st.active, master, &cycle.stop, || {
        ship_effect(&cx, master, cycle.effect.id, cycle.target, cycle.other.clone(), false, cycle.started, cycle.duration_ms, cycle.repeat);
    });
    if retired && cycle.effect.kind != EffectKind::Propulsion && st.activation_generation.get(&master).is_some_and(|current| Arc::ptr_eq(current, &cycle.stop)) {
        st.activation_generation.remove(&master);
    }
    if retired { cycle.owner.armed = false; }
    drop(st);
    if retired { on_stop(s, &cx, &cycle); }
    if retired && matches!(charge_kind(cycle.effect.id, cycle.effect.kind), EffectKind::FueledLocalRepair { .. }) {
        s.defer(async move {
            if let Err(error) = crate::services::combat::targeting_charges::auto_reload(&cx, master).await {
                warn!("module {master} automatic reload failed: {}", match error { CallError::Internal(e) => format!("{e:#}"), CallError::User { msg, .. } => msg });
            }
        });
    }
}

struct Activation {
    effect: Effect,
    module: Module,
    target: Option<i64>,
}

async fn on_activate(cx: &Ctx, fit: &Fit, activation: &Activation) -> Result<(), CallError> {
    use EffectKind::*;
    match activation.effect.kind {
        Turret | Missile => {
            let Some(target) = activation.target.filter(|t| targets_of(cx.char_id).contains(t)) else {
                return Err(user_error(messages::MSG_DENIED_ACTIVATE_TARGET_NOT_PRESENT, vec![]));
            };
            let ship = cx.ship_id;
            let (in_warp, present) = cx.world.with(cx.system_id, move |s| {
                (s.ball(ship).is_some_and(|b| b.mode == MODE_WARP), s.ball(target).is_some())
            }).await.unwrap_or((false, false));
            if in_warp { return Err(user_error(messages::MSG_DENIED_ACTIVATE_IN_WARP, vec![])); }
            if !present { return Err(user_error(messages::MSG_DENIED_ACTIVATE_TARGET_NOT_PRESENT, vec![])); }
            if loaded(&cx.db, cx.ship_id, activation.module.flag).await?.is_none_or(|(_, q)| q <= 0) {
                return Err(user_error(messages::MSG_NO_CHARGES, vec![("launcher".into(), Value::tuple([Value::Int(UE_TYPEID), Value::Int(activation.module.type_id as i64)]))]));
            }
            Ok(())
        }
        Projected { .. } => {
            let Some(target) = activation.target.filter(|t| targets_of(cx.char_id).contains(t)) else {
                return Err(user_error(messages::MSG_DENIED_ACTIVATE_TARGET_NOT_PRESENT, vec![]));
            };
            let ship = cx.ship_id;
            let sample = cx.world.with(cx.system_id, move |s| {
                s.sampled_ball(ship).zip(s.sampled_ball(target)).map(|(a, b)| {
                    (a.mode == MODE_WARP, crate::space::dist(a.pos, b.pos) - a.radius as f64 - b.radius as f64)
                })
            }).await.flatten();
            let Some((in_warp, gap)) = sample else { return Err(user_error(messages::MSG_DENIED_ACTIVATE_TARGET_NOT_PRESENT, vec![])); };
            if in_warp { return Err(user_error(messages::MSG_DENIED_ACTIVATE_IN_WARP, vec![])); }
            let optimal = activation.effect.attribute(EffectAttribute::Range).map(|a| fit.value(Key::Module(activation.module.id), a)).unwrap_or(0.0);
            let falloff = activation.effect.attribute(EffectAttribute::Falloff).map(|a| fit.value(Key::Module(activation.module.id), a)).unwrap_or(0.0);
            if range::range_multiplier(gap.max(0.0), optimal, falloff) <= 0.0 {
                return Err(user_error(messages::MSG_DENIED_ACTIVATE_TARGET_NOT_PRESENT, vec![]));
            }
            Ok(())
        }
        Mining => mining::validate(cx, fit, &activation.module, activation.target).await,
        RemoteRepair { .. } => {
            let key = Key::Module(activation.module.id);
            let optimal = activation.effect.attribute(EffectAttribute::Range).map(|a| fit.value(key, a)).unwrap_or(0.0);
            let falloff = activation.effect.attribute(EffectAttribute::Falloff).map(|a| fit.value(key, a)).unwrap_or(0.0);
            repair::target(cx, activation.target, optimal, falloff).await.map(|_| ())
        }
        CapTransfer | Neutralizer | Nosferatu | CapBooster => capacitor::validate(cx, fit, &activation.module, &activation.effect, activation.target).await,
        Propulsion if activation.effect.id == i64::from(EFFECT_MICROWARPDRIVE) => {
            if fit.value(Key::Module(activation.module.id), ATTR_ACTIVATION_BLOCKED) > 0.0 {
                return Err(user_error(messages::MSG_MODULE_IS_BLOCKED, vec![]));
            }
            Ok(())
        }
        Modifiers | LocalRepair { .. } | FueledLocalRepair { .. } | Propulsion => Ok(()),
        Smartbomb | Vorton | Ecm | BurstJammer |
        Cloak | MicroJumpDrive | Salvage | Tractor | CargoScan | ShipScan | SurveyScan |
        AutoTarget | PassiveTarget | Bomb | ProbeLaunch | Hacking | Online => unreachable!("unsupported activation kind"),
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

pub(super) async fn subject(c: &Client, ship: i64, item_id: i64, effect: i32, require_online: bool) -> Result<Item, CallError> {
    let policy = OnlinePolicy::from_client(c)?;
    Ok(subject_core(&c.db, &c.sde, &policy, ship, item_id, effect, require_online).await?.1)
}

/// Activate(itemID, effectName, targetID, repeat)
pub(crate) async fn activate(c: &mut Client, call: &Call<'_>) -> CallResult {
    let mut item_id = call.args.first().and_then(|v| v.as_int()).unwrap_or(0);
    let effect_name = call.args.get(1).and_then(|v| v.as_text()).map(|s| s.into_owned()).unwrap_or_default();
    let target = call.args.get(2).and_then(|v| v.as_int());
    let repeat = call.args.get(3).and_then(|v| v.as_int()).unwrap_or(0);
    let Some(cx) = ctx(c) else { return Err(notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION)) };
    let module = load_items(&c.db, &c.sde, ItemQuery::Id(item_id)).await.map_err(db_err)?.into_iter().next()
        .ok_or_else(|| notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION))?;
    let mut matches = cx.world.sde().dogma.effects_by_name(&effect_name);
    let Some(row) = matches.next() else { return Err(unsupported::refusal(None, &effect_name, module.type_id)) };
    if matches.next().is_some() { return Err(CallError::Internal(anyhow::anyhow!("multiple effects named {effect_name}"))); }
    drop(matches);
    let id = row.effect_id;
    // Fitting mutations and non-online activation share this lock through registration and FX queuing.
    // Online activation takes it inside set_online instead.
    let _lock = if id != EFFECT_ONLINE as i32 { Some(crate::services::market::lock().await) } else { None };
    let mut module = subject(c, cx.ship_id, item_id, id, id != EFFECT_ONLINE as i32).await?;
    let Some(effect) = registry::resolve(cx.world.sde(), id) else {
        return Err(unsupported::refusal(Some(id), &effect_name, module.type_id));
    };
    let kind = effect.kind;
    if id == EFFECT_MUTADAPTIVE_REMOTE_ARMOR_REPAIRER || !registry::activation_in_scope(module.category_id, module.group_id, kind) || !effect.manually_activatable() {
        return Err(unsupported::refusal(Some(id), &effect_name, module.type_id));
    }
    // Bank selection is serialized with fitting mutations by the shared lock above.
    if matches!(kind, EffectKind::Turret | EffectKind::Missile) {
        let master = crate::services::combat::targeting_charges::master_for_member(&c.db, cx.ship_id, item_id).await?;
        if master != item_id {
            module = subject(c, cx.ship_id, master, id, true).await?;
            if !registry::activation_in_scope(module.category_id, module.group_id, kind) {
                return Err(unsupported::refusal(Some(id), &effect_name, module.type_id));
            }
            item_id = master;
        }
    }
    match kind {
        EffectKind::Hacking => return super::super::hacking::start(c, item_id, target).await,
        EffectKind::Online => return super::online::set_online(c, cx.ship_id, item_id, true).await,
        EffectKind::Modifiers | EffectKind::Turret | EffectKind::Missile |
        EffectKind::LocalRepair { .. } | EffectKind::FueledLocalRepair { .. } |
        EffectKind::Propulsion | EffectKind::Mining | EffectKind::Projected { .. } | EffectKind::CapTransfer |
        EffectKind::Neutralizer | EffectKind::Nosferatu | EffectKind::CapBooster | EffectKind::RemoteRepair { .. } => {}
        EffectKind::Smartbomb | EffectKind::Vorton |
        EffectKind::Ecm | EffectKind::BurstJammer | EffectKind::Cloak |
        EffectKind::MicroJumpDrive | EffectKind::Salvage | EffectKind::Tractor |
        EffectKind::CargoScan | EffectKind::ShipScan | EffectKind::SurveyScan |
        EffectKind::AutoTarget | EffectKind::PassiveTarget | EffectKind::Bomb |
        EffectKind::ProbeLaunch => return Err(unsupported::refusal(Some(id), &effect_name, module.type_id)),
    }
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
            let m = subject(c, cx.ship_id, slave, id, true).await?;
            modules.push(Module { id: m.item_id, type_id: m.type_id, flag: m.flag_id });
        }
    }
    let masters: Vec<i64> = state().active.iter().filter(|(_, active)| active.char_id == cx.char_id && !active.forced_inactive).map(|(&id, _)| id).collect();
    let mut active = Vec::new();
    for master in masters { active.extend(bank(&cx.db, cx.ship_id, master).await?); }
    active.sort_unstable();
    active.dedup();
    let activating: Vec<i64> = modules.iter().map(|module| module.id).collect();
    super::fitting::validate_active(&cx.db, c.sde.clone(), &fit, cx.ship_id, &active, &activating).await?;
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
    cx.world.post(cx.system_id, move |s| { s.drop_jump_cloak(ship); });
    let owner = ActivationOwner { master: item_id, generation: stop.clone(), armed: true };
    ship_effect(&cx, item_id, effect.id, target, other.clone(), true, filetime_now(), duration_ms, repeat);
    info!(user = c.login.user_id, "module {item_id} activated {effect_name} on {target:?} (x{}, {duration_ms:.0} ms, repeat {repeat})", modules.len());
    let world = cx.world.clone();
    let cycle = Cycle { cx, modules, effect, target, repeat, stop, owner, started: filetime_now(), charge_type: None, cycles: 0, duration_ms, nominal_start: 0, other, repair_amount: 0.0 };
    world.post(cycle.cx.system_id, move |s| {
        let now = s.now_ms();
        cycle_start(s, cycle, now);
    });
    Ok(Value::None)
}


/// Deactivate(itemID, effectName): stop after the current cycle.
pub(crate) async fn deactivate(c: &mut Client, call: &Call<'_>) -> CallResult {
    let item_id = call.args.first().and_then(|v| v.as_int()).unwrap_or(0);
    let Some(cx) = ctx(c) else { return Err(notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION)) };
    let module = load_items(&c.db, &c.sde, ItemQuery::Id(item_id)).await.map_err(db_err)?.into_iter().next().ok_or_else(|| notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION))?;
    let name = call.args.get(1).and_then(|v| v.as_text()).map(|s| s.into_owned()).unwrap_or_default();
    let mut rows = c.sde.dogma.effects_by_name(&name);
    let Some(row) = rows.next() else { return Err(unsupported::refusal(None, &name, module.type_id)) };
    if rows.next().is_some() { return Err(CallError::Internal(anyhow::anyhow!("multiple effects named {name}"))); }
    let id = row.effect_id;
    drop(rows);
    let module = subject(c, cx.ship_id, item_id, id, false).await?;
    let Some(kind) = registry::kind(&c.sde, id) else { return Err(unsupported::refusal(Some(id), &name, module.type_id)) };
    if !c.sde.types.effects(module.type_id).iter().any(|effect| effect.effect_id == id)
        || !registry::activation_in_scope(module.category_id, module.group_id, kind) {
        return Err(unsupported::refusal(Some(id), &name, module.type_id));
    }
    if kind == EffectKind::Online { return super::online::set_online(c, cx.ship_id, item_id, false).await; }
    if let Some(stop) = state().active.get(&item_id) {
        stop_generation(&stop.stop);
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
