//! Transient overload membership and actor-owned ship rack heat.
//! The rack curve mirrors `dogma/attributes/heatAttribute.py` `CalculateHeat`.
//! The server evaluates from the last actual input change, so the result does
//! not depend on read or tick cadence. Overload effects and heat attributes
//! are evaluated from Fit and SDE data.
//! Runtime heat and priming are discarded on ship departure and restart.
//! Module damage, burnout, RPC validation and repair are handled elsewhere.
//! Effective overload transitions also update the actor's cached fitted defenses.

pub(crate) mod rack;
pub(crate) mod damage;
pub(crate) mod rpc;
mod boundary;
mod boundary_runtime;

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, atomic::AtomicBool};
use tokio::sync::oneshot;
use crate::client::{filetime_from_unix_ms, filetime_now};
use infinity_core::consts::{dogma::ATTR_HEAT_LOW, dogma::ATTR_HEAT_MED, dogma::ATTR_HEAT_HI, FLAG_FITTING_FIRST, FLAG_MED_SLOT_FIRST, FLAG_HI_SLOT_FIRST, FLAG_FITTING_LAST};
use crate::services::combat::{self, Ctx};
use crate::services::combat::targeting_charges::bank;
use crate::services::effects::lifecycle::{cycle_valid, Cycle, Module};
use crate::services::effects::notify;
use crate::services::fit::{Fit, Key};
use crate::services::CallError;
use crate::space::{System, World};

/// SDE heat, capacity, dissipation and attenuation attributes, low to high rack.
const HEAT: [i32; 3] = [ATTR_HEAT_LOW, ATTR_HEAT_MED, ATTR_HEAT_HI];
const CAPACITY: [i32; 3] = [1200, 1199, 1178];
const DISSIPATION: [i32; 3] = [1198, 1196, 1179];
const ATTENUATION: [i32; 3] = [1262, 1261, 1259];
/// SDE module heatAbsorbtionRateModifier.
const ABSORPTION: i32 = 1180;
/// SDE ship heatGenerationMultiplier.
const GENERATION: i32 = 1224;
/// SDE module heatDamage, already modified by Fit.
const HEAT_DAMAGE: i32 = 1211;

#[derive(Clone)]
struct Entry {
    owner: i64,
    ship: i64,
    effect: i32,
    effective: bool,
    desired: bool,
    cx: Ctx,
    source_generation: Option<Arc<AtomicBool>>,
}

#[derive(Default)]
struct State { modules: HashMap<i64, Entry> }
fn state() -> MutexGuard<'static, State> {
    static STATE: OnceLock<Mutex<State>> = OnceLock::new();
    STATE.get_or_init(Default::default).lock().unwrap()
}

/// Effective overload membership for a single Fit snapshot.
pub(crate) fn overloaded_modules() -> HashSet<i64> {
    state().modules.iter().filter_map(|(&id, entry)| entry.effective.then_some(id)).collect()
}

fn rack_index(flag: i32) -> Option<usize> {
    match flag {
        FLAG_FITTING_FIRST..FLAG_MED_SLOT_FIRST => Some(0),
        FLAG_MED_SLOT_FIRST..FLAG_HI_SLOT_FIRST => Some(1),
        FLAG_HI_SLOT_FIRST..=FLAG_FITTING_LAST => Some(2),
        _ => None,
    }
}

#[derive(Clone)]
struct Source {
    flag: i32,
    rate: f64,
    damage: f64,
    suspended: bool,
    stopping: bool,
    generation: Arc<AtomicBool>,
}

#[derive(Default)]
struct Rack {
    heat: f64,
    capacity: f64,
    incoming: f64,
    generation: f64,
    dissipation: f64,
    attenuation: f64,
    /// Last time the visible heat was evaluated. Reads and ticks move this; the curve anchor does not.
    last: i64,
    /// Heat and time of the current curve phase. It resets only when an input actually changes.
    anchor_heat: f64,
    anchor_at: i64,
    sources: HashMap<i64, Source>,
    boundary: Option<boundary_runtime::RackBoundary>,
}

#[derive(Default)]
struct Ship { owner: i64, racks: [Rack; 3] }

/// Actor-owned rack state; never accessed outside the system actor.
#[derive(Default)]
pub(crate) struct HeatState { ships: HashMap<i64, Ship> }

/// Client heat-state fields, with last calculation time in FILETIME.
#[derive(Clone, Debug)]
pub(crate) struct RackSnapshot {
    pub heat_id: i32,
    pub heat: f64,
    pub capacity: f64,
    pub incoming: f64,
    pub generation: f64,
    pub dissipation: f64,
    pub last_calc_time: i64,
}

/// Captured at a cycle boundary before any pending stop is applied.
#[derive(Clone)]
pub(crate) struct SourceSnapshot {
    pub module: i64,
    pub flag: i32,
    pub heat: f64,
    pub master: i64,
    pub capacity: f64,
    pub attenuation: f64,
    pub heat_damage: f64,
    pub generation: Arc<AtomicBool>,
}

fn evaluated(rack: &Rack, time: i64) -> f64 {
    if time <= rack.anchor_at { return rack.anchor_heat; }
    rack::curve(rack.anchor_heat, rack.capacity, rack.incoming, rack.generation, rack.dissipation, (time - rack.anchor_at) as f64)
}

fn settle(rack: &mut Rack, time: i64) -> Option<(f64, f64)> {
    if time <= rack.last { return None; }
    let old = rack.heat;
    rack.heat = evaluated(rack, time);
    rack.last = time;
    (old != rack.heat).then_some((old, rack.heat))
}

/// Settle under the old inputs, then start the next curve phase when the change is not still in the future.
fn reanchor(rack: &mut Rack, time: i64) {
    if time < rack.anchor_at { return; }
    settle(rack, time);
    rack.anchor_heat = evaluated(rack, time);
    rack.anchor_at = time;
}

fn apply_source(rack: &mut Rack, marker: &boundary::Boundary, pending: boundary_runtime::PendingStart) -> (bool, bool) {
    settle(rack, marker.at);
    let old = rack.sources.get(&marker.module);
    let incoming = rack.incoming + pending.rate - old.filter(|source| !source.suspended).map_or(0.0, |source| source.rate);
    let inputs = (rack.capacity, rack.dissipation, rack.generation, rack.incoming);
    let next = (pending.capacity, pending.dissipation, pending.generation_multiplier, incoming);
    if inputs != next { reanchor(rack, marker.at); }
    rack.capacity = pending.capacity;
    rack.dissipation = pending.dissipation;
    rack.attenuation = pending.attenuation;
    rack.generation = pending.generation_multiplier;
    let old = rack.sources.insert(marker.module, Source {
        flag: pending.flag, rate: pending.rate, damage: pending.damage,
        generation: marker.generation.clone(), suspended: false, stopping: false,
    });
    rack.incoming = incoming;
    let replaced = old.as_ref().is_some_and(|source| !Arc::ptr_eq(&source.generation, &marker.generation));
    (old.is_none() || replaced, replaced)
}

fn apply_source_end(rack: &mut Rack, marker: &boundary::Boundary, final_cycle: bool, pending_stop: bool) -> bool {
    let master = boundary_runtime::capture_master(rack, marker);
    let Some(source) = rack.sources.get(&marker.module).filter(|source| Arc::ptr_eq(&source.generation, &marker.generation)) else { return false };
    let snapshot = SourceSnapshot {
        master, module: marker.module, flag: source.flag, heat: evaluated(rack, marker.at), capacity: rack.capacity,
        attenuation: rack.attenuation, heat_damage: source.damage, generation: source.generation.clone(),
    };
    let stopping = source.stopping || final_cycle || pending_stop;
    let drop_incoming = stopping && !source.suspended;
    if drop_incoming { reanchor(rack, marker.at); }
    let Some(source) = rack.sources.get_mut(&marker.module) else { return false };
    source.stopping = stopping;
    if drop_incoming {
        rack.incoming -= source.rate;
        source.suspended = true;
        if rack.sources.values().all(|source| source.suspended) { rack.incoming = 0.0; }
    }
    boundary_runtime::capture(rack, marker, snapshot);
    true
}


fn apply_rack(rack: &mut Rack, time: i64, pending_stop: impl Fn(i64) -> bool) -> Vec<(i64, bool)> {
    let mut connections = Vec::new();
    loop {
        let Some((marker, pending)) = boundary_runtime::pop_ready(rack, time) else { break };
        let at = marker.at;
        let final_cycle = rack.boundary.as_ref().is_some_and(|boundary| boundary.cycle_final(marker.cycle_start, &marker.generation));
        settle(rack, at);
        match marker.kind {
            boundary::BoundaryKind::Start => {
                if let Some(pending) = pending {
                    let (added, replaced) = apply_source(rack, &marker, pending);
                    if replaced { connections.push((marker.module, false)); }
                    if added { connections.push((marker.module, true)); }
                    let mut membership = state();
                    if let Some(entry) = membership.modules.get_mut(&marker.module) {
                        entry.source_generation = Some(marker.generation.clone());
                    }
                }
            }
            boundary::BoundaryKind::End => { apply_source_end(rack, &marker, final_cycle, pending_stop(marker.module)); }
        }
    }
    settle(rack, boundary_runtime::limit(rack, time));
    connections
}

fn advance(s: &mut System, ship: i64, index: usize, time: i64) {
    let Some(ship_state) = s.heat.ships.get_mut(&ship) else { return };
    let before = ship_state.racks[index].heat;
    let owner = ship_state.owner;
    let connections = apply_rack(&mut ship_state.racks[index], time, |module| state().modules.get(&module).is_some_and(|entry| entry.effective && !entry.desired));
    let settled = ship_state.racks[index].last;
    let after = ship_state.racks[index].heat;
    if after != before {
        let (name, args) = notify::heat_attribute(owner, ship, HEAT[index], before, after, filetime_from_unix_ms(settled));
        s.notify_pilot(owner, name, args);
    }
    for (module, start) in connections {
        let (name, args) = notify::heat_connection(HEAT[index], module, start);
        s.notify_pilot(owner, name, args);
    }
}

/// Called once per actor housekeeping tick, after cycle timers.
pub(crate) fn tick(s: &mut System) {
    let time = s.now_ms();
    let ships: Vec<_> = s.heat.ships.keys().copied().collect();
    for ship in ships { for index in 0..HEAT.len() { advance(s, ship, index, time); } }
}

pub(crate) fn active(heat: &HeatState) -> bool {
    heat.ships.values().any(|ship| ship.racks.iter().any(|rack| {
        rack.heat != 0.0 || rack.incoming != 0.0 || boundary_runtime::blocking(rack)
    }))
}

/// Discard all transient state when a ship leaves its actor.
pub(crate) fn remove_ship(s: &mut System, ship: i64) {
    s.heat.ships.remove(&ship);
    state().modules.retain(|_, entry| entry.ship != ship);
}

fn effect(cx: &Ctx, module: i64, effect: i32, start: bool) {
    notify::ship_effect(cx, module, i64::from(effect), None, None, start, filetime_now(), -1.0, -1);
}

/// Request validated modules atomically; running bank members defer to cycle boundaries.
pub(super) async fn request(cx: &Ctx, modules: Vec<(Module, i32)>, start: bool) -> Result<(), CallError> {
    let masters: Vec<_> = {
        let st = combat::state();
        st.active.iter().filter(|(_, active)| active.char_id == cx.char_id && !active.forced_inactive)
            .map(|(&id, active)| (id, active.stop.clone())).collect()
    };
    let mut running = HashMap::new();
    for (master, generation) in &masters {
        for module in bank(&cx.db, cx.ship_id, *master).await? { running.insert(module, (*master, generation.clone())); }
    }
    let cx2 = cx.clone();
    cx.world.with(cx.system_id, move |s| {
        if !s.contains(cx2.ship_id) { return Err(CallError::Internal(anyhow::anyhow!("ship left system during overload request"))); }
        {
            let st = combat::state();
            let current: Vec<_> = st.active.iter().filter(|(_, active)| active.char_id == cx2.char_id && !active.forced_inactive).collect();
            if current.len() != masters.len() || current.iter().any(|(id, active)| !masters.iter().any(|(master, generation)| id == &master && Arc::ptr_eq(&active.stop, generation))) {
                return Err(CallError::Internal(anyhow::anyhow!("activation changed during overload request")));
            }
        }
        let mut changes = Vec::new();
        {
            let mut st = state();
            for (module, effect_id) in modules {
                let entry = st.modules.entry(module.id).or_insert_with(|| Entry { owner: cx2.char_id, ship: cx2.ship_id, effect: effect_id, effective: false, desired: false, cx: cx2.clone(), source_generation: None });
                entry.desired = start;
                if !running.contains_key(&module.id) && entry.effective != start {
                    entry.effective = start;
                    changes.push((module.id, entry.effect, start));
                }
            }
            st.modules.retain(|_, entry| entry.effective || entry.desired);
        }
        for (id, effect_id, enabled) in changes {
            s.fitted_module_overloaded(cx2.ship_id, id, enabled);
            effect(&cx2, id, effect_id, enabled);
        }
        Ok(())
    }).await.ok_or_else(|| CallError::Internal(anyhow::anyhow!("system unavailable during overload request")))?
}

/// Apply queued starts and hold the rack at this cycle's nominal start.
pub(super) fn begin_cycle(s: &mut System, cycle: &Cycle) {
    if !cycle_valid(s, cycle) { return; }
    let mut changes = Vec::new();
    let mut starts = Vec::new();
    {
        let mut membership = state();
        for module in &cycle.modules {
            if let Some(entry) = membership.modules.get_mut(&module.id).filter(|entry| entry.ship == cycle.cx.ship_id && entry.owner == cycle.cx.char_id) {
                if entry.desired && !entry.effective {
                    entry.effective = true;
                    changes.push((module.id, entry.effect));
                }
                if entry.effective {
                    if let Some(index) = rack_index(module.flag) {
                        entry.source_generation = Some(cycle.stop.clone());
                        starts.push((module.id, index));
                    }
                }
            }
        }
    }
    for (id, effect_id) in changes {
        s.fitted_module_overloaded(cycle.cx.ship_id, id, true);
        effect(&cycle.cx, id, effect_id, true);
    }
    if starts.is_empty() { return; }
    let ship = cycle.cx.ship_id;
    let state = s.heat.ships.entry(ship).or_insert_with(|| Ship { owner: cycle.cx.char_id, racks: std::array::from_fn(|_| Rack::default()) });
    for (module, index) in starts {
        boundary_runtime::install_start(&mut state.racks[index], module, cycle.stop.clone(), cycle.nominal_start);
    }
}

/// Queue evaluated source parameters; the actor applies them at nominal start.
pub(super) async fn connect_cycle(cx: &Ctx, fit: &Fit, cycle: &Cycle) -> Result<(), CallError> {
    let configs: Vec<_> = (0..HEAT.len()).map(|index| (
        fit.value(Key::Ship, CAPACITY[index]), fit.value(Key::Ship, DISSIPATION[index]),
        fit.value(Key::Ship, ATTENUATION[index]), fit.value(Key::Ship, GENERATION),
    )).collect();
    let modules: Vec<_> = cycle.modules.iter().filter_map(|module| rack_index(module.flag).map(|index| (
        module.id, module.flag, index, fit.value(Key::Module(module.id), ABSORPTION), fit.value(Key::Module(module.id), HEAT_DAMAGE),
    ))).collect();
    let ship = cx.ship_id;
    let owner = cx.char_id;
    let master = cycle.modules[0].id;
    let generation = cycle.stop.clone();
    let nominal = cycle.nominal_start;
    let duration = cycle.duration_ms;
    cx.world.with(cx.system_id, move |s| {
        let valid = s.contains(ship) && combat::state().active.get(&master).is_some_and(|active| Arc::ptr_eq(&active.stop, &generation));
        if !valid { return Err(CallError::Internal(anyhow::anyhow!("stale heat cycle"))); }
        let membership = state();
        let effective: Vec<_> = modules.into_iter().filter(|(module, _, _, _, _)| {
            membership.modules.get(module).is_some_and(|entry| entry.effective && entry.ship == ship && entry.owner == owner)
        }).collect();
        drop(membership);
        let heat = s.heat.ships.entry(ship).or_insert_with(|| Ship { owner, racks: std::array::from_fn(|_| Rack::default()) });
        for (module, flag, index, rate, damage) in effective {
            let (capacity, dissipation, attenuation, generation_multiplier) = configs[index];
            let armed = boundary_runtime::arm_start(&mut heat.racks[index], module, &generation, nominal, boundary_runtime::PendingStart {
                flag, rate, damage, capacity, dissipation, attenuation, generation_multiplier,
            });
            if !armed {
                boundary_runtime::install_start(&mut heat.racks[index], module, generation.clone(), nominal);
                boundary_runtime::arm_start(&mut heat.racks[index], module, &generation, nominal, boundary_runtime::PendingStart {
                    flag, rate, damage, capacity, dissipation, attenuation, generation_multiplier,
                });
            }
            let end = crate::services::effects::lifecycle::cycle_deadline(nominal, duration);
            if !boundary_runtime::install_end(&mut heat.racks[index], module, generation.clone(), nominal, end) { continue; }
        }
        for index in 0..HEAT.len() { advance(s, ship, index, s.now_ms()); }
        Ok(())
    }).await.ok_or_else(|| CallError::Internal(anyhow::anyhow!("system unavailable during heat connection")))?
}

/// Queue this activation's end capture. The receiver resolves after older markers.
pub(super) fn finish_cycle(
    s: &mut System, cycle: &Cycle, _end: i64, final_cycle: bool,
) -> Option<oneshot::Receiver<Option<Vec<SourceSnapshot>>>> {
    if !cycle_valid(s, cycle) { return None; }
    let ship = cycle.cx.ship_id;
    let index = rack_index(cycle.modules[0].flag)?;
    let ids: Vec<_> = cycle.modules.iter().map(|module| module.id).collect();
    let state = s.heat.ships.entry(ship).or_insert_with(|| Ship { owner: cycle.cx.char_id, racks: std::array::from_fn(|_| Rack::default()) });
    let expected = boundary_runtime::owned_modules(&state.racks[index], &ids, &cycle.stop, cycle.nominal_start);
    let receiver = boundary_runtime::register_waiter(
        &mut state.racks[index], cycle.modules[0].id, cycle.stop.clone(), cycle.nominal_start, expected.clone(), final_cycle,
    );
    boundary_runtime::mark_ends_ready(&mut state.racks[index], &expected, &cycle.stop, cycle.nominal_start);
    advance(s, ship, index, s.now_ms());
    Some(receiver)
}

/// Recheck ship, owner and source activation after acquiring the damage lock.
pub(crate) async fn generation_valid(world: &World, system: i64, ship: i64, owner: i64, source: &SourceSnapshot) -> bool {
    let master = source.master;
    let module = source.module;
    let generation = source.generation.clone();
    world.with(system, move |s| {
        s.contains(ship)
            && combat::state().active.get(&master).is_some_and(|active| active.char_id == owner && Arc::ptr_eq(&active.stop, &generation))
            && s.heat.ships.get(&ship).is_some_and(|state| state.owner == owner && state.racks.iter().any(|rack|
                rack.sources.get(&module).is_some_and(|source| Arc::ptr_eq(&source.generation, &generation))))
    }).await.unwrap_or(false)
}

fn suspend_stopping(rack: &mut Rack, ids: &[i64], generation: Option<&Arc<AtomicBool>>, at: i64) {
    let dropping: Vec<_> = ids.iter().copied().filter(|&id| {
        rack.sources.get(&id).is_some_and(|source| !source.suspended && generation.is_none_or(|current| Arc::ptr_eq(current, &source.generation)))
    }).collect();
    if dropping.is_empty() { return; }
    reanchor(rack, at);
    for id in dropping {
        let Some(source) = rack.sources.get_mut(&id) else { continue };
        rack.incoming -= source.rate;
        source.suspended = true;
        source.stopping = true;
    }
    if rack.sources.values().all(|source| source.suspended) { rack.incoming = 0.0; }
}

fn disconnect(s: &mut System, ship: i64, owner: i64, ids: &[i64], generation: Option<&Arc<AtomicBool>>) {
    for index in 0..HEAT.len() {
        let now = s.now_ms();
        if let Some(state) = s.heat.ships.get_mut(&ship) {
            for id in ids {
                match generation {
                    Some(generation) => { boundary_runtime::release_generation(&mut state.racks[index], *id, generation); }
                    None => { boundary_runtime::release_module(&mut state.racks[index], *id); }
                }
            }
            suspend_stopping(&mut state.racks[index], ids, generation, now);
        }
        advance(s, ship, index, now);
        let Some(state) = s.heat.ships.get_mut(&ship) else { continue };
        let rack = &mut state.racks[index];
        let mut removed = Vec::new();
        for id in ids {
            if rack.sources.get(id).is_some_and(|source| generation.is_none_or(|current| Arc::ptr_eq(current, &source.generation))) {
                rack.sources.remove(id);
                removed.push(*id);
            }
        }
        for id in removed {
            let (name, args) = notify::heat_connection(HEAT[index], id, false);
            s.notify_pilot(owner, name, args);
        }
    }
}

/// Release this cycle's held end. A continuing cycle keeps its source so the
/// next start, installed in the same actor closure, sees uninterrupted heat.
/// A stopped source drops incoming at the settled nominal end before cooling.
pub(super) fn complete_cycle(s: &mut System, cycle: &Cycle, final_cycle: bool) {
    let ship = cycle.cx.ship_id;
    let owner = cycle.cx.char_id;
    let ids: Vec<_> = cycle.modules.iter().map(|module| module.id).collect();
    if !s.contains(ship) || !combat::state().active.get(&ids[0]).is_some_and(|active| Arc::ptr_eq(&active.stop, &cycle.stop)) { return; }
    let boundary_stops: HashSet<_> = s.heat.ships.get(&ship).into_iter().flat_map(|state| state.racks.iter())
        .flat_map(|rack| rack.sources.iter())
        .filter_map(|(&id, source)| (source.stopping && Arc::ptr_eq(&source.generation, &cycle.stop)).then_some(id))
        .collect();
    let mut stopped = Vec::new();
    {
        let mut membership = state();
        for id in &ids {
            if let Some(entry) = membership.modules.get_mut(id).filter(|entry| entry.ship == ship && entry.owner == owner) {
                if entry.effective && (boundary_stops.contains(id) || !entry.desired) {
                    entry.effective = false;
                    stopped.push((*id, entry.effect));
                }
            }
        }
        membership.modules.retain(|_, entry| entry.effective || entry.desired);
    }
    let remove: Vec<_> = if final_cycle { ids.clone() } else { stopped.iter().map(|(id, _)| *id).collect() };
    let now = s.now_ms();
    if let Some(state) = s.heat.ships.get_mut(&ship) {
        for rack in &mut state.racks {
            if !remove.is_empty() { suspend_stopping(rack, &remove, Some(&cycle.stop), now); }
            boundary_runtime::release_completed(rack, &ids, &cycle.stop, cycle.nominal_start);
        }
    }
    if !remove.is_empty() { disconnect(s, ship, owner, &remove, Some(&cycle.stop)); }
    for (id, effect_id) in stopped {
        s.fitted_module_overloaded(ship, id, false);
        effect(&cycle.cx, id, effect_id, false);
    }
}

/// Retire sources and reconcile pending requests when activation exits early.
pub(super) fn stop_cycle(s: &mut System, cycle: &Cycle) {
    let ids: Vec<_> = cycle.modules.iter().map(|m| m.id).collect();
    disconnect(s, cycle.cx.ship_id, cycle.cx.char_id, &ids, Some(&cycle.stop));
    if !cycle_valid(s, cycle) { return; }
    let mut changes = Vec::new();
    {
        let mut st = state();
        for id in &ids {
            if let Some(entry) = st.modules.get_mut(id).filter(|e| e.ship == cycle.cx.ship_id && e.owner == cycle.cx.char_id) {
                if entry.effective != entry.desired { entry.effective = entry.desired; changes.push((*id, entry.effect, entry.effective)); }
            }
        }
        st.modules.retain(|_, entry| entry.effective || entry.desired);
    }
    for (id, effect_id, start) in changes {
        s.fitted_module_overloaded(cycle.cx.ship_id, id, start);
        effect(&cycle.cx, id, effect_id, start);
    }
}

/// Detached overload entries awaiting the fenced fitting publication.
pub(crate) struct FittingHeatCleanup { entries: Vec<(i64, i64, Entry)> }

fn detach_modules(ship: i64, modules: &[i64]) -> Vec<(i64, Entry)> {
    let mut st = state();
    modules.iter().filter_map(|id| {
        if st.modules.get(id).is_some_and(|entry| entry.ship == ship) {
            st.modules.remove(id).map(|entry| (*id, entry))
        } else { None }
    }).collect()
}

/// Detach committed membership, leaving same-system actor cleanup to publication.
pub(crate) fn detach_for_fitting(ship: i64, system: i64, modules: &[i64]) -> FittingHeatCleanup {
    let entries = detach_modules(ship, modules);
    let mut cleanup = FittingHeatCleanup { entries: Vec::new() };
    for (id, entry) in entries {
        if entry.cx.system_id == system {
            cleanup.entries.push((ship, id, entry));
        } else {
            let world = entry.cx.world.clone();
            world.post(entry.cx.system_id, move |s| clear_committed(s, ship, id, entry));
        }
    }
    cleanup
}

/// Consume detached entries after the actor's membership fence and old-value snapshot.
pub(crate) fn apply_fitting_cleanup(s: &mut System, cleanup: FittingHeatCleanup) {
    for (ship, id, entry) in cleanup.entries {
        clear_committed_inner(s, ship, id, entry, true);
    }
}

/// Invalidate overload synchronously after commit, then remove matching actor sources.
pub(crate) fn clear_modules(ship: i64, modules: &[i64]) {
    let entries = detach_modules(ship, modules);
    for (id, entry) in entries {
        let world = entry.cx.world.clone();
        world.post(entry.cx.system_id, move |s| clear_committed(s, ship, id, entry));
    }
}

fn clear_committed(s: &mut System, ship: i64, id: i64, entry: Entry) {
    clear_committed_inner(s, ship, id, entry, false);
}

fn clear_committed_inner(s: &mut System, ship: i64, id: i64, entry: Entry, fitting: bool) {
    if let Some(generation) = &entry.source_generation {
        disconnect(s, ship, entry.owner, &[id], Some(generation));
    }
    if entry.effective {
        if !state().modules.get(&id).is_some_and(|current| current.ship == ship && current.effective) {
            if fitting {
                s.clear_fitting_overload(ship, id);
            } else {
                s.fitted_module_overloaded(ship, id, false);
            }
        }
        let (name, args) = notify::overload_effect(entry.owner, ship, id, entry.effect, false);
        s.notify_pilot(entry.owner, name, args);
    }
}


/// Clear every overload belonging to a departing character.
pub(crate) fn clear_character(owner: i64) {
    let mut ships: HashMap<i64, Vec<i64>> = HashMap::new();
    {
        let st = state();
        for (&id, entry) in &st.modules {
            if entry.owner == owner { ships.entry(entry.ship).or_default().push(id); }
        }
    }
    for (ship, modules) in ships { clear_modules(ship, &modules); }
}

/// Bootstrap an unheated ship from evaluated Fit attributes without creating sources.
pub(crate) fn empty_ship_state(fit: &Fit, at: i64) -> Vec<RackSnapshot> {
    (0..HEAT.len()).map(|i| RackSnapshot {
        heat_id: HEAT[i], heat: 0.0, capacity: fit.value(Key::Ship, CAPACITY[i]), incoming: 0.0,
        generation: fit.value(Key::Ship, GENERATION), dissipation: fit.value(Key::Ship, DISSIPATION[i]), last_calc_time: at,
    }).collect()
}

/// Actor snapshot for client bootstrap and heat-attribute reporting.
pub(crate) async fn ship_state(world: &World, system: i64, ship: i64) -> Option<Vec<RackSnapshot>> {
    world.with(system, move |s| {
        let now = s.now_ms();
        for i in 0..HEAT.len() { advance(s, ship, i, now); }
        let state = s.heat.ships.get(&ship)?;
        if state.racks.iter().all(|rack| !boundary_runtime::configured(rack)) { return None; }
        Some(state.racks.iter().enumerate().map(|(i, rack)| RackSnapshot {
            heat_id: HEAT[i], heat: rack.heat, capacity: rack.capacity, incoming: rack.incoming,
            generation: rack.generation, dissipation: rack.dissipation, last_calc_time: filetime_from_unix_ms(rack.last),
        }).collect())
    }).await.flatten()
}

#[cfg(test)] mod tests;
