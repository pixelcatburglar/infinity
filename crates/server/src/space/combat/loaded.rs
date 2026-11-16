//! Actor-resident loaded charge state: exact i64 quantities with dirty/held/
//! pending/unknown flags and finite-use permits. No await, SQL or World calls
//! run here; persistence rides the snapshot gate and business transactions
//! through the shared adapters in `services::combat::loaded`.
use super::ActorCombatRecord;
use crate::dao::items::charges::{self, LoadedSlotKey, LoadedWrite};
use crate::space::System;
use std::collections::HashMap;

/// Consecutive unflushed snapshot periods before new loaded use pauses.
fn failure_periods_limit() -> i64 { crate::config::game().space.loaded_snapshot_failure_periods }

/// A cold or business-published seed for one module's loaded slot.
#[derive(Clone, Copy)]
pub(crate) struct LoadedSeed {
    pub module: i64,
    pub flag: i32,
    pub type_id: Option<i32>,
    pub quantity: i64,
    pub pending: bool,
}
/// One admitted finite use: at most one charge payment, returned by every
/// outcome — success, refusal, cancel, stop or drop — and bound to the slot's
/// incarnation and type at admission.
#[derive(Clone, Copy)]
pub(crate) struct LoadedPermit {
    pub ship: i64,
    pub module: i64,
    pub flag: i32,
    pub incarnation: u64,
    pub type_id: i32,
}
/// One ship's loaded slot, keyed inside the record by module item id.
pub(crate) struct LoadedSlot {
    pub flag: i32,
    pub module: i64,
    pub type_id: Option<i32>,
    pub incarnation: u64,
    pub quantity: i64,
    pub dirty: bool,
    pub held: bool,
    pub unknown: bool,
    pub pending: bool,
    pub finite: u32,
}

impl LoadedSlot {
    /// The durable write this slot's current absolute requires, if dirty.
    pub(super) fn write(&self, ship: i64) -> LoadedWrite {
        LoadedWrite { ship, module: self.module, flag: self.flag,
            type_id: self.type_id.unwrap_or(0),
            incarnation: self.incarnation, quantity: self.quantity }
    }
    /// Work that install, removal or retirement must not overwrite: an unsaved or held value, an unknown value, or admitted finite use.
    fn outstanding(&self) -> bool { self.dirty || self.held || self.unknown || self.finite > 0 }
    fn fresh(seed: LoadedSeed, finite: u32) -> Self {
        Self { flag: seed.flag, module: seed.module, type_id: seed.type_id,
            incarnation: crate::space::targeting::next_generation(), quantity: seed.quantity,
            dirty: false, held: false, unknown: false, pending: seed.pending, finite }
    }
}

impl ActorCombatRecord {
    /// Unfinished loaded work: install, removal and retirement must not
    /// silently overwrite or drop a record that still has any.
    pub(crate) fn has_loaded_obligations(&self) -> bool {
        self.loaded.values().any(LoadedSlot::outstanding)
    }
}

/// Build the slot map a record seeds from its fit's loaded seeds.
pub(crate) fn seed_map(seeds: &[LoadedSeed]) -> HashMap<i64, LoadedSlot> {
    seeds.iter().map(|&seed| (seed.module, LoadedSlot::fresh(seed, 0))).collect()
}

impl System {
    fn loaded_slot(&self, ship: i64, module: i64) -> Option<&LoadedSlot> {
        self.actor_combat.get(&ship)?.loaded.get(&module)
    }
    /// Any unfinished loaded obligation in this system.
    pub(crate) fn loaded_outstanding(&self) -> bool {
        self.actor_combat.values().any(ActorCombatRecord::has_loaded_obligations)
    }
    /// New loaded use is refused while quiescing or after consecutive failed
    /// snapshot periods; chargeless work, cargo and movement are unaffected.
    pub(crate) fn loaded_use_paused(&self) -> bool {
        self.loaded_quiescing || self.loaded_failed_periods >= failure_periods_limit()
    }
    /// Count one due period before the gate is tried; busy, in-flight, held,
    /// unknown and unfinished uses all count as unfinished.
    pub(crate) fn record_loaded_period(&mut self) {
        if self.loaded_outstanding() {
            self.loaded_failed_periods = self.loaded_failed_periods.saturating_add(1);
        }
    }
    /// A fully clean state resets the failure streak; one ship's business
    /// success alone does not clear another's outstanding obligation.
    pub(crate) fn refresh_loaded_counter(&mut self) {
        if !self.loaded_outstanding() { self.loaded_failed_periods = 0; }
    }
    /// Exact effective view of one slot; blocked slots answer as unavailable.
    /// Returns (incarnation, type, quantity, pending).
    pub(crate) fn loaded_effective(&self, ship: i64, module: i64) -> Option<(u64, i32, i64, bool)> {
        let slot = self.loaded_slot(ship, module)?;
        let type_id = slot.type_id?;
        (!slot.held && !slot.unknown).then_some((slot.incarnation, type_id, slot.quantity, slot.pending))
    }
    /// An uncharged proof: no quantity, and no pending/held/unknown race.
    pub(crate) fn loaded_empty_proof(&self, ship: i64, module: i64) -> bool {
        self.loaded_slot(ship, module)
            .is_none_or(|slot| slot.quantity == 0 && !slot.pending && !slot.held && !slot.unknown)
    }
    /// Admit one finite use bound to the slot's current incarnation.
    pub(crate) fn hold_loaded_use(&mut self, ship: i64, module: i64) -> Option<LoadedPermit> {
        if self.loaded_use_paused() { return None; }
        let slot = self.loaded_slot(ship, module)?;
        let type_id = slot.type_id?;
        if slot.quantity <= 0 || slot.held || slot.unknown || slot.pending { return None; }
        let permit = LoadedPermit { ship, module, flag: slot.flag, incarnation: slot.incarnation, type_id };
        self.actor_combat.get_mut(&ship)?.loaded.get_mut(&module)?.finite += 1;
        Some(permit)
    }
    /// Release an admitted use without paying: refusal, cancel, stop or drop.
    pub(crate) fn release_loaded_use(&mut self, permit: LoadedPermit) {
        if let Some(slot) = self.actor_combat.get_mut(&permit.ship).and_then(|r| r.loaded.get_mut(&permit.module)) {
            slot.finite = slot.finite.saturating_sub(1);
        }
        self.refresh_loaded_counter();
    }
    /// Pay exactly one charge under a live permit; a stale incarnation, an
    /// emptied magazine or a held/unknown slot refuses instead of debiting.
    pub(crate) fn consume_loaded(&mut self, permit: LoadedPermit) -> Option<(i32, i64)> {
        let slot = self.actor_combat.get_mut(&permit.ship)?.loaded.get_mut(&permit.module)?;
        if slot.incarnation != permit.incarnation || slot.type_id != Some(permit.type_id)
            || slot.quantity <= 0 || slot.held || slot.unknown { return None; }
        slot.finite = slot.finite.saturating_sub(1);
        slot.quantity -= 1;
        slot.dirty = true;
        Some((permit.type_id, slot.quantity))
    }

    /// Dirty absolute rows; `held` selects the business-held mode used by the
    /// holder itself, an empty `ships` covers every resident record.
    fn capture_loaded(&self, ships: &[i64], held: bool) -> Vec<LoadedWrite> {
        self.actor_combat.iter()
            .filter(|(ship, _)| ships.is_empty() || ships.contains(ship))
            .flat_map(|(&ship, record)| record.loaded.values()
                .filter(move |slot| slot.dirty && slot.held == held)
                .map(move |slot| slot.write(ship)))
            .collect()
    }
    pub(crate) fn capture_dirty_loaded(&self, ships: &[i64]) -> Vec<LoadedWrite> { self.capture_loaded(ships, false) }
    pub(crate) fn capture_held_loaded(&self, ships: &[i64]) -> Vec<LoadedWrite> { self.capture_loaded(ships, true) }
    /// Clear dirty only on rows whose identity and quantity still equal what
    /// the durable batch actually matched; a value that moved stays dirty.
    pub(crate) fn clear_saved_loaded(&mut self, saved: &[LoadedWrite]) {
        for write in saved {
            if let Some(slot) = self.actor_combat.get_mut(&write.ship).and_then(|r| r.loaded.get_mut(&write.module)) {
                if slot.incarnation == write.incarnation && slot.type_id == Some(write.type_id)
                    && slot.quantity == write.quantity { slot.dirty = false; }
            }
        }
        self.refresh_loaded_counter();
    }
    /// Keys of slots invalidated and still awaiting a successful fresh reseed.
    pub(crate) fn unknown_loaded_keys(&self) -> Vec<LoadedSlotKey> {
        self.actor_combat.iter().flat_map(|(&ship, record)| record.loaded.values()
            .filter(|slot| slot.unknown)
            .map(move |slot| LoadedSlotKey { ship, module: slot.module, flag: slot.flag })).collect()
    }

    /// An unknown outcome invalidates the held absolute until a real reseed.
    pub(crate) fn invalidate_held_loaded(&mut self, slots: &[LoadedSlotKey]) {
        for key in slots {
            if let Some(slot) = self.actor_combat.get_mut(&key.ship).and_then(|r| r.loaded.get_mut(&key.module)) {
                if slot.flag == key.flag { slot.held = true; slot.unknown = true; }
            }
        }
    }
    /// A known commit publishes the business's absolute values and pending
    /// flags under the caller's gate and re-incarnates every touched slot so
    /// earlier permits cannot debit the new state. Untouched holds are the
    /// receipt owner's to release, never this call's.

    pub(crate) fn publish_loaded_absolute(&mut self, ship: i64, membership: u64, seeds: Vec<LoadedSeed>) -> bool {
        let Some(record) = self.actor_combat.get_mut(&ship) else { return false };
        if record.membership_generation != membership { return false; }
        for seed in seeds {
            record.fit.set_loaded_seed(seed);
            let finite = record.loaded.get(&seed.module).map_or(0, |slot| slot.finite);
            record.loaded.insert(seed.module, LoadedSlot::fresh(seed, finite));
        }
        self.refresh_loaded_counter();
        true
    }
    /// Reseed held/unknown slots from a fresh durable read; a `None` entry
    /// drops the slot and its fit seed when the module no longer exists.
    pub(crate) fn reseed_loaded(&mut self, ship: i64, entries: Vec<(LoadedSlotKey, Option<LoadedSeed>)>) -> bool {
        let Some(record) = self.actor_combat.get_mut(&ship) else { return false; };
        for (key, seed) in entries {
            match seed {
                Some(seed) => {
                    record.fit.set_loaded_seed(seed);
                    record.loaded.insert(key.module, LoadedSlot::fresh(seed, 0));
                }
                None => {
                    record.fit.remove_loaded_seed(key.module);
                    record.loaded.remove(&key.module);
                }
            }
        }
        self.refresh_loaded_counter();
        true
    }
    /// Effective charge rows for readers; `None` when the ship is not resident.
    pub(crate) fn loaded_rows(&self, ship: i64) -> Option<Vec<charges::Charge>> {
        Some(self.actor_combat.get(&ship)?.loaded.values()
            .filter(|slot| slot.type_id.is_some() && (slot.quantity > 0 || slot.unknown))
            .map(|slot| charges::Charge { flag_id: slot.flag, type_id: slot.type_id.unwrap_or(0),
                quantity: if slot.unknown { 0 } else { slot.quantity } }).collect())
    }

    /// Refuse every new loaded use ahead of the shutdown drain.
    pub(crate) fn begin_loaded_quiesce(&mut self) { self.loaded_quiescing = true; }
    /// True while admitted finite uses have not settled.
    fn loaded_busy(&self) -> bool {
        self.actor_combat.values().any(|record| record.loaded.values().any(|slot| slot.finite > 0))
    }
    /// Signal `done` once no admitted finite use remains, rescheduling itself
    /// on the actor's own timer until admitted work finishes. Business-held
    /// slots are owned by transactions holding the gate, which the flush
    /// serializes behind, so they are not drained here.
    pub(crate) fn drain_loaded(&mut self, done: tokio::sync::mpsc::UnboundedSender<()>) {
        if self.loaded_busy() {
            self.schedule(100, move |s| s.drain_loaded(done));
        } else {
            let _ = done.send(());
        }
    }
}

#[cfg(all(test, feature = "tests-space"))] mod tests;
