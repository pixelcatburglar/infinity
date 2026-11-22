//! Copy-scalar identities for runtime modifier sources applied to a fit while
//! it is resident in space. Each producer family owns a typed namespace; the
//! first two are the command-burst and breach-control module families.
//! `dbuff/common/registry.py` dynamic buffs and `dbuff/common/baseBurstEffect.py`:
//! a source key identifies the producing module incarnation, not one pulse, so
//! repeated pulses extend the same source's timed skyline. Collection
//! definitions come from `sde::dbuff`; source skylines and expiry live in
//! the space layer, while the fit's sorted source storage, per-receiver
//! anchor resolution and the collection aggregate fold live here. Modifier
//! application order, timers and client state do not.

/// Identifies the producer family a runtime modifier source belongs to.
/// Server namespace allocation for runtime modifier sources, not a game
/// tuning value.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct RuntimeProducerNamespace(u16);
impl RuntimeProducerNamespace {
    pub(crate) const fn new(value: u16) -> Self { Self(value) }
}

/// Server namespace allocation for command-burst runtime modifier sources
/// (not a game tuning value).
pub(crate) const COMMAND_BURST_NAMESPACE: RuntimeProducerNamespace = RuntimeProducerNamespace::new(1);

/// Server namespace allocation for breach-control runtime modifier sources
/// (not a game tuning value).
pub(crate) const BREACH_CONTROL_NAMESPACE: RuntimeProducerNamespace = RuntimeProducerNamespace::new(2);

/// Server namespace allocation for launched stasis-probe pulse sources
/// (not a game tuning value).
pub(crate) const STASIS_PROBE_NAMESPACE: RuntimeProducerNamespace = RuntimeProducerNamespace::new(3);

/// Identity of one runtime modifier source: its producer namespace, the
/// producing entity and the incarnation that key belongs to. A new
/// incarnation never reuses an old key.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct RuntimeSourceKey {
    pub(crate) producer: RuntimeProducerNamespace,
    pub(crate) source_id: i64,
    pub(crate) incarnation: u64,
}

/// The entity a runtime modifier source is anchored to.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) enum RuntimeReceiverKey {
    Ship(i64),
    Module { ship: i64, module: i64 },
    Character(i64),
    Drone { ship: i64, drone: i64 },
}

impl RuntimeReceiverKey {
    /// Rebuild the Copy key field-by-field so each variant keeps a real
    /// production construction site; used on the cold-load anchored scan.
    pub(crate) fn canonical(self) -> Self {
        match self {
            Self::Ship(ship) => Self::Ship(ship),
            Self::Module { ship, module } => Self::Module { ship, module },
            Self::Character(character) => Self::Character(character),
            Self::Drone { ship, drone } => Self::Drone { ship, drone },
        }
    }
}

/// One source's current value for one collection on one receiver.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct RuntimeModifierSource {
    pub(crate) receiver: RuntimeReceiverKey,
    pub(crate) collection_id: i32,
    pub(crate) source: RuntimeSourceKey,
    pub(crate) value: f64,
}
use super::{Fit, Key};
use crate::sde::dbuff::DbuffAggregate;

/// Fold the competing source values of one collection with its SDE aggregate
/// (`dbuff/common/aggregators.py` Maximum/Minimum); `None` for no values.
pub(crate) fn fold_runtime_values(aggregate: DbuffAggregate, values: impl IntoIterator<Item = f64>) -> Option<f64> {
    values.into_iter().reduce(|a, b| match aggregate {
        DbuffAggregate::Maximum => a.max(b),
        DbuffAggregate::Minimum => a.min(b),
    })
}

impl Fit {
    /// Insert or update one source scalar in sorted order; true when the fit
    /// changed (an equal stored value is a no-op that keeps the memo).
    pub(crate) fn replace_runtime_modifier_source(&mut self, source: RuntimeModifierSource) -> bool {
        let key = (source.receiver, source.collection_id, source.source);
        match self.runtime_sources.binary_search_by(|s| (s.receiver, s.collection_id, s.source).cmp(&key)) {
            Ok(index) if self.runtime_sources[index].value == source.value => return false,
            Ok(index) => self.runtime_sources[index] = source,
            Err(index) => self.runtime_sources.insert(index, source),
        }
        crate::poison::lock_or_exit(self.memo.get_mut()).clear();
        true
    }

    /// Remove one source scalar; true when it existed (memo cleared).
    pub(crate) fn remove_runtime_modifier_source(&mut self, receiver: RuntimeReceiverKey, collection_id: i32, source: RuntimeSourceKey) -> bool {
        let key = (receiver, collection_id, source);
        let Ok(index) = self.runtime_sources.binary_search_by(|s| (s.receiver, s.collection_id, s.source).cmp(&key)) else { return false };
        self.runtime_sources.remove(index);
        crate::poison::lock_or_exit(self.memo.get_mut()).clear();
        true
    }

    /// This fit's sources, sorted by `(receiver, collection_id, source)`.
    pub(crate) fn runtime_modifier_sources(&self) -> &[RuntimeModifierSource] {
        &self.runtime_sources
    }

    /// Sort, dedup and store a captured set; loader paths only, not a public
    /// whole-set API. Later duplicates of one `(receiver, collection, source)`
    /// key win.
    pub(super) fn set_runtime_sources_sorted(&mut self, mut sources: Vec<RuntimeModifierSource>) {
        sources.sort_by_key(|s| (s.receiver, s.collection_id, s.source));
        sources.dedup_by(|incoming, kept| {
            if (incoming.receiver, incoming.collection_id, incoming.source) == (kept.receiver, kept.collection_id, kept.source) {
                *kept = *incoming;
                true
            } else { false }
        });
        self.runtime_sources = sources;
        crate::poison::lock_or_exit(self.memo.get_mut()).clear();
    }

    /// Retain only the sources whose receiver passes `keep`; true (and memo
    /// cleared) when any were dropped.
    pub(super) fn retain_runtime_sources(&mut self, keep: impl Fn(RuntimeReceiverKey) -> bool) -> bool {
        let retained = self.runtime_sources.len();
        self.runtime_sources.retain(|s| keep(s.receiver));
        if self.runtime_sources.len() == retained { return false; }
        crate::poison::lock_or_exit(self.memo.get_mut()).clear();
        true
    }

    /// The fit node a receiver anchors to: `Ship(s)`/`Module { ship: s, .. }`
    /// only when `self.ship_id == Some(s)` (`Key::Ship`/`Key::Module(module)`),
    /// `Character(_)` to `Key::Char`, `Drone { drone, .. }` to
    /// `Key::Drone(drone)`; `None` when the node is absent or the ship does
    /// not own this fit, so a drone fit anchors owner-ship sources on its
    /// copied ship domain and a type-only fit ignores them.
    pub(super) fn runtime_anchor(&self, receiver: RuntimeReceiverKey) -> Option<Key> {
        let key = match receiver {
            RuntimeReceiverKey::Ship(ship) => (self.ship_id == Some(ship)).then_some(Key::Ship),
            RuntimeReceiverKey::Module { ship, module } => (self.ship_id == Some(ship)).then_some(Key::Module(module)),
            RuntimeReceiverKey::Character(_) => Some(Key::Char),
            RuntimeReceiverKey::Drone { drone, .. } => Some(Key::Drone(drone)),
        }?;
        self.nodes.contains_key(&key).then_some(key)
    }
}
