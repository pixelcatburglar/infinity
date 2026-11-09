//! Actor-owned paid Entosis links and their lifecycle.
//! Capture rows and source revisions come from the sovereignty DAO; score replay and
//! checkpoints are delegated to the actor's replay and worker modules. Owned Hub
//! capture and offline score accrual are not supported.
mod score;
mod replay;
mod worker;

use std::{collections::HashMap, sync::{Arc, atomic::{AtomicBool, Ordering}}};
use crate::{client::filetime_from_unix_ms, dao::sovereignty::{capture::CaptureRow, sources::SourceChange},
    services::sovereignty::{CaptureContributor, CaptureObservation}, space::{System, combat::{CombatPhase, valid_condition}, dist}};

#[derive(Clone)]
pub(crate) struct LinkCycle {
    pub target_item_id: i64,
    pub target_epoch: i64,
    pub contributor: CaptureContributor,
    pub membership_generation: u64,
    pub activation: Arc<AtomicBool>,
    pub range_m: f64,
    pub scoring: bool,
    pub expires_sim: i64,
}

#[derive(Clone)]
struct Segment { contributor: CaptureContributor, start: i64, end: i64 }

struct Inflight { token: u64, target_item_id: i64, epoch: i64, revision: i64, system_revision: i64, through: i64 }

#[derive(Default)]
pub(super) struct State {
    anchor: Option<CaptureRow>,
    current: Option<CaptureRow>,
    active: HashMap<i64, LinkCycle>,
    segments: Vec<Segment>,
    watermarks: HashMap<i64, i64>,
    dirty: bool,
    frozen: Option<CaptureObservation>,
    inflight: Option<Inflight>,
    next_token: u64,
    deadline_ms: i64,
    pressure: bool,
}

fn ready(system: &System) -> bool {
    system.sovereignty.readiness.as_ref().is_some_and(|ready| ready.load(Ordering::Acquire))
}

/// Snapshot actor membership and the neutral target before source proof admission.
pub(crate) fn admission(system: &System, character: i64, ship: i64, target: i64) -> Option<(i64, u64)> {
    let state = &system.sovereignty.links;
    let row = state.current.as_ref()?;
    if !ready(system) || state.frozen.is_some() || state.pressure
        || system.sovereignty.alliance.is_some() || system.sovereignty.hub != Some(target)
        || row.target_item_id != target || !system.visible_to(character, target)
        || !system.pilot_ball(character).is_some_and(|ball| ball.id == ship) {
        return None;
    }
    let record = system.actor_combat.get(&ship)?;
    if !matches!(record.phase, CombatPhase::Live) || !valid_condition(&record.condition) { return None; }
    Some((row.ownership_epoch, record.membership_generation))
}

/// Reuse the original source proof only for the same module activation.
pub(crate) fn cycle(system: &System, module: i64, activation: &Arc<AtomicBool>) -> Option<LinkCycle> {
    system.sovereignty.links.active.get(&module)
        .filter(|link| Arc::ptr_eq(&link.activation, activation)).cloned()
}

/// Check a prospective paid cycle without changing actor state.
pub(crate) fn valid_paid(system: &System, cycle: &LinkCycle) -> bool {
    !system.sovereignty.links.pressure && system.sovereignty.links.frozen.is_none()
        && valid(system, cycle)
}

fn valid(system: &System, cycle: &LinkCycle) -> bool {
    let source = &cycle.contributor;
    let Some(row) = system.sovereignty.links.current.as_ref() else { return false };
    if !ready(system) || system.sovereignty.alliance.is_some()
        || system.sovereignty.hub != Some(cycle.target_item_id)
        || row.target_item_id != cycle.target_item_id || row.ownership_epoch != cycle.target_epoch
        || !cycle.range_m.is_finite() || cycle.range_m < 0.0
        || system.sovereignty.links.watermarks.get(&source.character_id).is_some_and(|&revision| revision > source.source_revision)
        || !system.pilot_ball(source.character_id).is_some_and(|ball| ball.id == source.ship_id)
        || !system.visible_to(source.character_id, cycle.target_item_id)
        || !crate::services::combat::targeting_charges::has_target(source.character_id, cycle.target_item_id) { return false; }
    let Some(record) = system.actor_combat.get(&source.ship_id) else { return false };
    if !matches!(record.phase, CombatPhase::Live) || record.membership_generation != cycle.membership_generation
        || !valid_condition(&record.condition) || record.fit.module_state(source.module_id) != Some((true, true)) { return false; }
    let (Some(ship), Some(target)) = (system.ball(source.ship_id), system.ball(cycle.target_item_id)) else { return false };
    if ship.mode == crate::space::MODE_WARP || target.mode == crate::space::MODE_WARP { return false; }
    let distance = dist(ship.position_at(system.now_ms()), target.position_at(system.now_ms()));
    let radii = f64::from(ship.radius) + f64::from(target.radius);
    distance.is_finite() && radii.is_finite() && radii >= 0.0 && (distance - radii).max(0.0) <= cycle.range_m
}

fn truncate(state: &mut State, module: i64, at: i64) {
    for segment in state.segments.iter_mut().rev().filter(|segment| segment.contributor.module_id == module && segment.end > at) {
        segment.end = at.max(segment.start);
    }
}

fn force(system: &mut System, cycle: &LinkCycle) {
    if crate::services::effects::lifecycle::force_inactive(cycle.contributor.module_id, &cycle.activation) {
        system.fitted_module_active(cycle.contributor.ship_id, cycle.contributor.module_id, false);
        system.refresh_projection_target(cycle.contributor.ship_id);
    }
}

/// Called only after the module's resource payment has succeeded.
pub(crate) fn paid_cycle(system: &mut System, cycle: LinkCycle) -> bool {
    if !ready(system) || system.sovereignty.links.frozen.is_some() { return false; }
    let now = filetime_from_unix_ms(system.now_ms());
    if !valid(system, &cycle) {
        force(system, &cycle);
        stop(system, cycle.contributor.module_id, &cycle.activation);
        return false;
    }
    if cycle.expires_sim <= now || system.sovereignty.links.pressure { return false; }
    let module = cycle.contributor.module_id;
    if system.sovereignty.links.active.get(&module).is_some_and(|old| !Arc::ptr_eq(&old.activation, &cycle.activation)) { return false; }
    if cycle.scoring && system.sovereignty.links.segments.len() >= crate::config::game().space.sovereignty_max_pending_segments {
        replay::boundary(system, now);
        let state = &mut system.sovereignty.links;
        for segment in &mut state.segments {
            if segment.end > now { segment.end = now.max(segment.start); }
        }
        state.pressure = true;
        replay::boundary(system, now);
        return false;
    }
    let watermark = system.sovereignty.links.watermarks.entry(cycle.contributor.character_id).or_insert(cycle.contributor.source_revision);
    *watermark = (*watermark).max(cycle.contributor.source_revision);
    replay::boundary(system, now);
    let state = &mut system.sovereignty.links;
    truncate(state, module, now);
    if cycle.scoring {
        state.segments.push(Segment { contributor: cycle.contributor.clone(), start: now, end: cycle.expires_sim });
    }
    let ship = cycle.contributor.ship_id;
    let inserted = state.active.insert(module, cycle).is_none();
    if inserted { system.refresh_projection_target(ship); }
    replay::boundary(system, now);
    true
}

/// Explicit lifecycle retirement; a request flag alone does not retire the link.
pub(crate) fn stop(system: &mut System, module: i64, activation: &Arc<AtomicBool>) {
    if !system.sovereignty.links.active.get(&module).is_some_and(|link| Arc::ptr_eq(&link.activation, activation)) { return; }
    let now = filetime_from_unix_ms(system.now_ms());
    replay::boundary(system, now);
    let state = &mut system.sovereignty.links;
    truncate(state, module, now);
    let ship = state.active.remove(&module).expect("matching active link").contributor.ship_id;
    system.refresh_projection_target(ship);
    replay::boundary(system, now);
}

pub(crate) fn blocks(system: &System, ship: i64) -> bool {
    system.sovereignty.links.active.values().any(|link| link.contributor.ship_id == ship)
}

/// Capital-sized ships cannot receive assistance while an Entosis link is active.
pub(crate) fn assistance_blocked(system: &System, ship: i64) -> bool {
    blocks(system, ship) && system.actor_combat.get(&ship).is_some_and(|record| {
        let capital = record.fit.value(crate::services::fit::Key::Ship, infinity_core::consts::dogma::ATTR_IS_CAPITAL_SIZE);
        capital.is_finite() && capital > 0.0
    })
}

pub(super) fn install(system: &mut System, target: Option<CaptureRow>) {
    let old = std::mem::take(&mut system.sovereignty.links);
    for cycle in old.active.values() {
        force(system, cycle);
        system.refresh_projection_target(cycle.contributor.ship_id);
    }
    system.sovereignty.links.anchor = target.clone();
    system.sovereignty.links.next_token = old.next_token;
    system.sovereignty.links.watermarks = old.watermarks.into_iter()
        .filter(|(character, _)| system.pilot_ball(*character).is_some()).collect();
    system.sovereignty.links.current = target;
}

pub(in crate::space) fn removed(system: &mut System, item: i64) {
    if system.sovereignty.links.current.as_ref().is_some_and(|row| row.target_item_id == item) {
        install(system, None);
        return;
    }
    loop {
        let Some(cycle) = system.sovereignty.links.active.values().find(|link| link.contributor.ship_id == item || link.contributor.module_id == item).cloned() else { break };
        force(system, &cycle);
        stop(system, cycle.contributor.module_id, &cycle.activation);
    }
    if let Some(character) = system.ball(item).and_then(|ball| ball.char_id) {
        let state = &mut system.sovereignty.links;
        if !state.active.values().any(|link| link.contributor.character_id == character)
            && !state.segments.iter().any(|segment| segment.contributor.character_id == character) {
            state.watermarks.remove(&character);
        }
    }
}

pub(super) fn invalidate(system: &mut System, changes: &[SourceChange]) {
    for change in changes {
        if system.pilot_ball(change.character_id).is_some()
            || system.sovereignty.links.active.values().any(|link| link.contributor.character_id == change.character_id)
            || system.sovereignty.links.segments.iter().any(|segment| segment.contributor.character_id == change.character_id) {
            let watermark = system.sovereignty.links.watermarks.entry(change.character_id).or_insert(change.revision);
            *watermark = (*watermark).max(change.revision);
        }
    }
    replay::invalidate(system, changes);
    loop {
        let Some(cycle) = system.sovereignty.links.active.values().find(|link| changes.iter().any(|change| change.character_id == link.contributor.character_id && change.revision > link.contributor.source_revision)).cloned() else { break };
        force(system, &cycle);
        stop(system, cycle.contributor.module_id, &cycle.activation);
    }
}

pub(super) fn close(system: &mut System) {
    worker::close(system);
    install(system, None);
}

pub(in crate::space) fn tick(system: &mut System) {
    if system.sovereignty.links.current.is_none() { return; }
    loop {
        let Some(cycle) = system.sovereignty.links.active.values().find(|link| !valid(system, link)).cloned() else { break };
        force(system, &cycle);
        stop(system, cycle.contributor.module_id, &cycle.activation);
    }
    worker::tick(system);
}

#[cfg(test)] mod tests;
