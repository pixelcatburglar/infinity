//! Unresolved nominal start and end transitions for one actor-owned rack.
//! Each marker is one cycle boundary that has not yet been applied. Pending
//! start parameters and the current completion replies live only until those
//! boundaries resolve. A captured end stays as an unresolved completion gate
//! until its own cycle completes. There is no cycle history, replay, or extra timer.

use std::collections::HashMap;
use std::sync::{Arc, atomic::AtomicBool};

use tokio::sync::oneshot;

use super::boundary::{Boundary, BoundaryKind, BoundaryQueue};
use super::{Rack, SourceSnapshot};

pub(super) struct PendingStart {
    pub(super) flag: i32,
    pub(super) rate: f64,
    pub(super) damage: f64,
    pub(super) capacity: f64,
    pub(super) dissipation: f64,
    pub(super) attenuation: f64,
    pub(super) generation_multiplier: f64,
}

struct Waiter {
    expected: Vec<i64>,
    captured: Vec<SourceSnapshot>,
    sender: Option<oneshot::Sender<Option<Vec<SourceSnapshot>>>>,
}

type Identity = (i64, usize, i64);

/// One current activation's unresolved end. The timestamp remains a settlement
/// gate after the marker is captured, until that cycle completes.
struct HeldEnd {
    master: i64,
    at: i64,
}

pub(super) struct RackBoundary {
    queue: BoundaryQueue,
    pending: HashMap<Identity, PendingStart>,
    waiters: HashMap<Identity, Waiter>,
    held: HashMap<Identity, HeldEnd>,
    final_cycles: Vec<(i64, Arc<AtomicBool>)>,
}

impl RackBoundary {
    fn new() -> Self {
        Self {
            queue: BoundaryQueue::default(), pending: HashMap::new(), waiters: HashMap::new(),
            held: HashMap::new(), final_cycles: Vec::new(),
        }
    }

    pub(super) fn cycle_final(&self, cycle_start: i64, generation: &Arc<AtomicBool>) -> bool {
        self.final_cycles.iter().any(|(start, current)| *start == cycle_start && Arc::ptr_eq(current, generation))
    }
}

impl Default for RackBoundary {
    fn default() -> Self { Self::new() }
}

pub(super) fn rack_mut(rack: &mut Rack) -> &mut RackBoundary {
    rack.boundary.get_or_insert_with(RackBoundary::new)
}

pub(super) fn configured(rack: &Rack) -> bool {
    rack.capacity != 0.0 || rack.dissipation != 0.0 || rack.attenuation != 0.0 || rack.generation != 0.0
}

pub(super) fn limit(rack: &Rack, requested: i64) -> i64 {
    let Some(boundary) = rack.boundary.as_ref() else { return requested };
    let queued = boundary.queue.limit(requested);
    boundary.held.values().map(|held| held.at).filter(|at| *at != i64::MAX).fold(queued, i64::min)
}

fn identity(module: i64, generation: &Arc<AtomicBool>, cycle_start: i64) -> Identity {
    (module, Arc::as_ptr(generation) as usize, cycle_start)
}

fn pointer(generation: &Arc<AtomicBool>) -> usize { Arc::as_ptr(generation) as usize }


pub(super) fn install_start(rack: &mut Rack, module: i64, generation: Arc<AtomicBool>, at: i64) {
    let boundary = rack_mut(rack);
    boundary.pending.remove(&identity(module, &generation, at));
    boundary.queue.install(Boundary {
        at, module, generation, cycle_start: at, kind: BoundaryKind::Start, ready: false,
    });
}

pub(super) fn arm_start(rack: &mut Rack, module: i64, generation: &Arc<AtomicBool>, cycle_start: i64, pending: PendingStart) -> bool {
    let boundary = rack_mut(rack);
    if !boundary.queue.mark_ready(module, generation, cycle_start, BoundaryKind::Start) { return false; }
    boundary.pending.insert(identity(module, generation, cycle_start), pending);
    true
}

pub(super) fn install_end(rack: &mut Rack, module: i64, generation: Arc<AtomicBool>, cycle_start: i64, at: i64) -> bool {
    let boundary = rack_mut(rack);
    let key = identity(module, &generation, cycle_start);
    if !boundary.pending.contains_key(&key) {
        boundary.queue.remove_cycle(module, &generation, cycle_start);
        return false;
    }
    boundary.queue.install(Boundary {
        at, module, generation, cycle_start, kind: BoundaryKind::End, ready: false,
    });
    true
}

/// Modules of this activation that actually own a heat marker or armed source.
pub(super) fn owned_modules(rack: &Rack, modules: &[i64], generation: &Arc<AtomicBool>, cycle_start: i64) -> Vec<i64> {
    let Some(boundary) = rack.boundary.as_ref() else { return Vec::new() };
    let mut owned: Vec<_> = modules.iter().copied().filter(|&module| {
        boundary.queue.has_cycle(module, generation, cycle_start)
            || boundary.pending.contains_key(&identity(module, generation, cycle_start))
            || rack.sources.get(&module).is_some_and(|source| Arc::ptr_eq(&source.generation, generation))
    }).collect();
    owned.sort_unstable();
    owned.dedup();
    owned
}

/// Register this cycle's waiter on its first heat-owned module. An empty set
/// resolves immediately. An unrelated activation keeps its own waiter.
pub(super) fn register_waiter(
    rack: &mut Rack, master: i64, generation: Arc<AtomicBool>, cycle_start: i64, expected: Vec<i64>, final_cycle: bool,
) -> oneshot::Receiver<Option<Vec<SourceSnapshot>>> {
    let (sender, receiver) = oneshot::channel();
    if expected.is_empty() {
        let _ = sender.send(Some(Vec::new()));
        return receiver;
    }
    let boundary = rack_mut(rack);
    if final_cycle { boundary.final_cycles.push((cycle_start, generation.clone())); }
    let key = identity(expected[0], &generation, cycle_start);
    if let Some(previous) = boundary.waiters.insert(key, Waiter { expected: expected.clone(), captured: Vec::new(), sender: Some(sender) }) {
        if let Some(sender) = previous.sender { let _ = sender.send(None); }
    }
    for module in expected {
        boundary.held.entry(identity(module, &generation, cycle_start)).or_insert(HeldEnd { master, at: i64::MAX });
    }
    receiver
}

pub(super) fn mark_ends_ready(rack: &mut Rack, modules: &[i64], generation: &Arc<AtomicBool>, cycle_start: i64) {
    let boundary = rack_mut(rack);
    for module in modules {
        boundary.queue.mark_ready(*module, generation, cycle_start, BoundaryKind::End);
    }
}

pub(super) fn pop_ready(rack: &mut Rack, up_to: i64) -> Option<(Boundary, Option<PendingStart>)> {
    let due = limit(rack, up_to);
    let marker = rack.boundary.as_mut()?.queue.pop_ready(due)?;
    if marker.kind != BoundaryKind::Start { return Some((marker, None)); }
    let Some(pending) = rack.boundary.as_mut()?.pending.remove(&identity(marker.module, &marker.generation, marker.cycle_start)) else {
        rack.boundary.as_mut()?.queue.install(Boundary {
            at: marker.at, module: marker.module, generation: marker.generation, cycle_start: marker.cycle_start,
            kind: BoundaryKind::Start, ready: false,
        });
        return None;
    };
    Some((marker, Some(pending)))
}

/// Master recorded for this captured cycle, else the source itself.
pub(super) fn capture_master(rack: &Rack, marker: &Boundary) -> i64 {
    let key = identity(marker.module, &marker.generation, marker.cycle_start);
    rack.boundary.as_ref().and_then(|boundary| boundary.held.get(&key).map(|held| held.master)).unwrap_or(marker.module)
}

pub(super) fn capture(rack: &mut Rack, marker: &Boundary, snapshot: SourceSnapshot) {
    let Some(boundary) = rack.boundary.as_mut() else { return };
    let key = identity(marker.module, &marker.generation, marker.cycle_start);
    let Some(held) = boundary.held.get_mut(&key) else { return };
    held.at = held.at.min(marker.at);
    let reply = boundary.waiters.iter().find(|(key, waiter)| {
        key.1 == pointer(&marker.generation) && key.2 == marker.cycle_start && waiter.expected.contains(&marker.module)
    }).map(|(key, _)| *key);
    let Some(reply) = reply else { return };
    let Some(waiter) = boundary.waiters.get_mut(&reply) else { return };
    if waiter.captured.iter().any(|item| item.module == marker.module) { return; }
    waiter.captured.push(snapshot);
    if waiter.captured.len() != waiter.expected.len() { return; }
    waiter.captured.sort_unstable_by_key(|item| item.module);
    if let Some(sender) = waiter.sender.take() { let _ = sender.send(Some(std::mem::take(&mut waiter.captured))); }
    let cycle_start = marker.cycle_start;
    let generation = marker.generation.clone();
    boundary.final_cycles.retain(|(start, current)| !(*start == cycle_start && Arc::ptr_eq(current, &generation)));
}

/// Drop this cycle's completion gates and waiter. The connected source stays.
pub(super) fn release_completed(rack: &mut Rack, modules: &[i64], generation: &Arc<AtomicBool>, cycle_start: i64) {
    let Some(boundary) = rack.boundary.as_mut() else { return };
    let pointer = pointer(generation);
    for module in modules { boundary.held.remove(&identity(*module, generation, cycle_start)); }
    let affected: Vec<_> = boundary.waiters.keys().copied().filter(|key| key.1 == pointer && key.2 == cycle_start
        && boundary.waiters.get(key).is_some_and(|waiter| waiter.expected.iter().any(|module| modules.contains(module)))).collect();
    for key in affected {
        let Some(waiter) = boundary.waiters.remove(&key) else { continue };
        if waiter.sender.is_some() && waiter.captured.len() != waiter.expected.len() {
            if let Some(sender) = waiter.sender { let _ = sender.send(None); }
        }
    }
    boundary.final_cycles.retain(|(start, current)| !(*start == cycle_start && Arc::ptr_eq(current, generation)));
}

fn cancel_affected(boundary: &mut RackBoundary, module: i64, generation: Option<&Arc<AtomicBool>>) {
    boundary.held.retain(|key, _| !(key.0 == module && generation.is_none_or(|current| key.1 == pointer(current))));
    let affected: Vec<_> = boundary.waiters.keys().copied().filter(|key| {
        generation.is_none_or(|current| key.1 == pointer(current))
            && boundary.waiters.get(key).is_some_and(|waiter| waiter.expected.contains(&module))
    }).collect();
    for key in affected {
        let Some(mut waiter) = boundary.waiters.remove(&key) else { continue };
        waiter.expected.retain(|id| *id != module);
        waiter.captured.retain(|item| item.module != module);
        if waiter.expected.is_empty() {
            if let Some(sender) = waiter.sender { let _ = sender.send(None); }
            continue;
        }
        if waiter.captured.len() == waiter.expected.len() {
            waiter.captured.sort_unstable_by_key(|item| item.module);
            if let Some(sender) = waiter.sender.take() { let _ = sender.send(Some(std::mem::take(&mut waiter.captured))); }
        }
        boundary.waiters.insert(key, waiter);
    }
}
pub(super) fn release_generation(rack: &mut Rack, module: i64, generation: &Arc<AtomicBool>) -> usize {
    let Some(boundary) = rack.boundary.as_mut() else { return 0 };
    let pointer = pointer(generation);
    boundary.pending.retain(|key, _| !(key.0 == module && key.1 == pointer));
    let removed = boundary.queue.remove_generation(module, generation);
    cancel_affected(boundary, module, Some(generation));
    boundary.final_cycles.retain(|(start, current)| !(Arc::ptr_eq(current, generation)
        && !boundary.waiters.keys().any(|key| key.1 == pointer && key.2 == *start)));
    removed
}

pub(super) fn release_module(rack: &mut Rack, module: i64) -> usize {
    let Some(boundary) = rack.boundary.as_mut() else { return 0 };
    boundary.pending.retain(|key, _| key.0 != module);
    let removed = boundary.queue.remove_module(module);
    cancel_affected(boundary, module, None);
    removed
}

pub(super) fn blocking(rack: &Rack) -> bool {
    limit(rack, i64::MAX) != i64::MAX
}

#[cfg(all(test, feature = "tests-effects"))]
mod tests;
