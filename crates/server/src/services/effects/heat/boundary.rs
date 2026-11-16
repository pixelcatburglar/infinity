//! Ordering of unresolved nominal heat cycle boundaries owned by the system actor.
//! The rack curve mirrors `dogma/attributes/heatAttribute.py`; the client has no
//! corresponding module damage routine. This queue owns no database or static
//! data, source parameters, timers, history, or replay. Callers must retire markers
//! when their cycle or activation ends and bound the markers they install.

use std::collections::BTreeMap;
use std::sync::{Arc, atomic::AtomicBool};

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) enum BoundaryKind {
    End,
    Start,
}

#[derive(Clone, Debug)]
pub(super) struct Boundary {
    pub(super) at: i64,
    pub(super) module: i64,
    pub(super) generation: Arc<AtomicBool>,
    pub(super) cycle_start: i64,
    pub(super) kind: BoundaryKind,
    pub(super) ready: bool,
}

type Key = (i64, i64, BoundaryKind, i64, usize);

#[derive(Default)]
pub(super) struct BoundaryQueue {
    markers: BTreeMap<Key, Boundary>,
}

impl BoundaryQueue {
    /// Reinstalling an identity leaves its original timestamp and readiness intact.
    pub(super) fn install(&mut self, boundary: Boundary) {
        if self.markers.values().any(|marker| {
            marker.module == boundary.module
                && Arc::ptr_eq(&marker.generation, &boundary.generation)
                && marker.cycle_start == boundary.cycle_start
                && marker.kind == boundary.kind
        }) {
            return;
        }
        let key = (boundary.at, boundary.module, boundary.kind, boundary.cycle_start,
            Arc::as_ptr(&boundary.generation) as usize);
        self.markers.insert(key, boundary);
    }

    pub(super) fn mark_ready(&mut self, module: i64, generation: &Arc<AtomicBool>, cycle_start: i64, kind: BoundaryKind) -> bool {
        if let Some(marker) = self.markers.values_mut().find(|marker| {
            marker.module == module && Arc::ptr_eq(&marker.generation, generation)
                && marker.cycle_start == cycle_start && marker.kind == kind
        }) {
            marker.ready = true;
            true
        } else {
            false
        }
    }

    /// Oldest marker that is ready and due at or before `up_to`. An earlier marker
    /// that is not yet due, or not yet ready, blocks every later marker.
    pub(super) fn pop_ready(&mut self, up_to: i64) -> Option<Boundary> {
        let (key, marker) = self.markers.first_key_value()?;
        if key.0 > up_to || !marker.ready { return None; }
        self.markers.pop_first().map(|(_, marker)| marker)
    }

    pub(super) fn limit(&self, requested: i64) -> i64 {
        self.markers.first_key_value().map_or(requested, |(key, _)| requested.min(key.0))
    }

    pub(super) fn has_cycle(&self, module: i64, generation: &Arc<AtomicBool>, cycle_start: i64) -> bool {
        self.markers.values().any(|marker| marker.module == module
            && Arc::ptr_eq(&marker.generation, generation) && marker.cycle_start == cycle_start)
    }

    pub(super) fn remove_cycle(&mut self, module: i64, generation: &Arc<AtomicBool>, cycle_start: i64) -> usize {
        let before = self.markers.len();
        self.markers.retain(|_, marker| !(marker.module == module
            && Arc::ptr_eq(&marker.generation, generation) && marker.cycle_start == cycle_start));
        before - self.markers.len()
    }

    pub(super) fn remove_generation(&mut self, module: i64, generation: &Arc<AtomicBool>) -> usize {
        let before = self.markers.len();
        self.markers.retain(|_, marker| !(marker.module == module && Arc::ptr_eq(&marker.generation, generation)));
        before - self.markers.len()
    }

    pub(super) fn remove_module(&mut self, module: i64) -> usize {
        let before = self.markers.len();
        self.markers.retain(|_, marker| marker.module != module);
        before - self.markers.len()
    }
}

#[cfg(all(test, feature = "tests-effects"))]
mod tests;
