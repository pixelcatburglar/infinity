//! Actor-owned bounded aggregates for accepted combat effects.
//! The client supplies no ledger protocol. Facts remain in memory until the
//! existing system checkpoint journals them; no timer, database, or Lua runs here.

pub(crate) mod boundaries;
mod capture;
pub(crate) use capture::CapturedEvent;
mod lethal;
mod replay;
pub(crate) mod reservation;
pub(crate) mod scramble;

use std::collections::HashMap;
use std::sync::Arc;
use crate::lua::events::combat::{CombatEntity, HpLayer, SettlementId};
use crate::lua::events::combat::amount::ExactBinarySum;
use crate::lua::events::resources::FactContext;
use boundaries::Boundaries;
use replay::Replay;
use scramble::{CapturedScramble, ScrambleStore};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct CombatSource {
    pub actual_actor_character: Option<i64>, pub entity: CombatEntity, pub context: FactContext,
    pub source_is_npc: bool,
    pub controller_incarnation: Option<[u8; 16]>,
    pub module_id: Option<i64>, pub effect_id: Option<i64>, pub weapon_type_id: Option<i32>,
    pub activation: [u8; 16], pub sequence: u64,
}
impl CombatSource {
    pub(crate) fn controller_entity(&self) -> Option<CombatEntity> {
        Some(CombatEntity {
            item_id: self.context.ship_id?, type_id: self.context.ship_type_id?,
            incarnation: self.controller_incarnation?, character_id: Some(self.actual_actor_character?),
            corporation_id: self.context.corporation_id, alliance_id: self.context.alliance_id,
            faction_id: self.context.faction_id,
        })
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct SourceKey {
    actor: Option<i64>, entity: CombatEntity, context: FactContext,
    source_is_npc: bool,
    controller_incarnation: Option<[u8; 16]>,
    module_id: Option<i64>, effect_id: Option<i64>, weapon_type_id: Option<i32>, activation: [u8; 16],
}
impl From<CombatSource> for SourceKey {
    fn from(source: CombatSource) -> Self {
        Self { actor: source.actual_actor_character, entity: source.entity, context: source.context, source_is_npc: source.source_is_npc, controller_incarnation: source.controller_incarnation,
            module_id: source.module_id, effect_id: source.effect_id,
            weapon_type_id: source.weapon_type_id, activation: source.activation }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Kind { Damage, RemoteRepair }
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct AggregateKey {
    kind: Kind, source: SourceKey, target: CombatEntity, layer: HpLayer,
    slice: u64, epoch: u64, boundary_at: i64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SlotState { Active, Closed(i64), Sealed { batch: u64, active_until: i64, ordinal: u32 } }
struct Slot {
    key: AggregateKey, sum: ExactBinarySum, first: i64, last: i64,
    state: SlotState, reservations: usize,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Refusal { Full, Busy, Replay, Invalid, Overflow }

#[derive(Clone, Debug)]
pub(crate) struct PendingDamage {
    pub settlement: SettlementId, pub source: CombatSource, pub target: CombatEntity,
    pub occurred_at: i64, pub amounts: [ExactBinarySum; 3],
    pub slice: u64, pub qualification_epoch: u64, pub boundary_at: i64,
}
struct CapturedRow {
    key: AggregateKey, sum: ExactBinarySum, first: i64, last: i64, active_until: i64, ordinal: u32,
}
pub(super) struct CapturedFacts {
    runtime_epoch: u64, system_id: i64, batch: u64, rows: Vec<CapturedRow>, scrambles: Vec<CapturedScramble>,
}
pub(super) struct CombatFacts {
    runtime_epoch: u64, system_id: i64, key_capacity: usize, handle_capacity: usize,
    slots: Vec<Option<Slot>>, index: HashMap<AggregateKey, usize>, free_slots: Vec<usize>, live_key_count: usize, dirty_unsealed_keys: usize,
    next_slice: u64, next_batch: u64, boundaries: Boundaries, replay: Replay,
    batches: HashMap<u64, Arc<CapturedFacts>>, scope_cleanup_needed: bool, scrambles: ScrambleStore,
}
impl CombatFacts {
    pub fn new(runtime_epoch: u64, system_id: i64, key_capacity: usize, handle_capacity: usize) -> Self {
        Self { runtime_epoch, system_id, key_capacity, handle_capacity,
            slots: Vec::with_capacity(key_capacity), index: HashMap::with_capacity(key_capacity),
            free_slots: Vec::with_capacity(key_capacity), live_key_count: 0, dirty_unsealed_keys: 0,
            next_slice: 0, next_batch: 0, boundaries: Boundaries::new(),
            replay: Replay::new(key_capacity), batches: HashMap::with_capacity(handle_capacity), scope_cleanup_needed: false,
            scrambles: ScrambleStore::default() }
    }
    pub fn runtime_epoch(&self) -> u64 { self.runtime_epoch }
    pub fn is_dirty(&self) -> bool { self.dirty_unsealed_keys != 0 }
    pub fn is_full(&self) -> bool { self.live_key_count >= self.key_capacity || self.batches.len() >= self.handle_capacity }
    #[cfg(test)]
    pub fn key_count(&self) -> usize { self.live_key_count }
    pub fn batch_count(&self) -> usize { self.batches.len() }
    pub fn register_source(&mut self, activation: [u8; 16], generation: u64, first_sequence: u64) -> Result<(), Refusal> {
        if u64::from_be_bytes(activation[8..].try_into().expect("fixed activation")) != generation { return Err(Refusal::Invalid); }
        self.replay.register(activation, generation, first_sequence)
    }
    pub fn retire_source(&mut self, activation: [u8; 16], generation: u64) -> Result<(), Refusal> {
        self.replay.retire(activation, generation)?;
        self.scope_cleanup_needed = true;
        Ok(())
    }
    pub(crate) fn reserve_attempt(&mut self, source: &CombatSource) -> Result<(), Refusal> {
        if source.actual_actor_character.is_some() && source.controller_entity().is_none() { return Err(Refusal::Invalid); }
        let generation = u64::from_be_bytes(source.activation[8..].try_into().expect("fixed activation"));
        self.replay.bind_subject(source.activation, generation, source.actual_actor_character)?;
        self.replay.reserve(source.activation, generation, source.sequence)
    }
    pub(crate) fn finish_attempt(&mut self, source: &CombatSource) {
        let generation = u64::from_be_bytes(source.activation[8..].try_into().expect("fixed activation"));
        self.scope_cleanup_needed |= self.replay.finish(source.activation, generation, source.sequence, true);
    }
    pub fn close_character_boundary(&mut self, character: i64, at: i64) -> Result<(boundaries::Boundary, Vec<scramble::ScrambleHandle>), Refusal> {
        let affected = |slot: &Slot| slot.key.source.actor == Some(character) || slot.key.target.character_id == Some(character);
        if self.slots.iter().flatten().any(|slot| affected(slot) && slot.reservations != 0) { return Err(Refusal::Busy); }
        if self.slots.iter().flatten().any(|slot| affected(slot) && slot.state == SlotState::Active && slot.last > at) { return Err(Refusal::Invalid); }
        self.validate_scramble_split(at, Some(character))?;
        let receipt = self.boundaries.close_character(character, at)?;
        for slot in self.slots.iter_mut().flatten() {
            if affected(slot) && slot.state == SlotState::Active { slot.state = SlotState::Closed(at); }
        }
        let ended = self.split_scrambles(at, at, Some(character))?;
        Ok((receipt, ended))
    }
    /// Observe UTC on the existing actor tick, never with a separate timer.
    pub fn observe_utc(&mut self, at: i64) -> Result<Vec<scramble::ScrambleHandle>, Refusal> {
        if !self.boundaries.day_would_change(at).map_err(|_| Refusal::Invalid)? {
            self.boundaries.observe_day(at).map_err(|_| Refusal::Invalid)?;
            return Ok(Vec::new());
        }
        if self.slots.iter().flatten().any(|slot| slot.reservations != 0) { return Err(Refusal::Busy); }
        if self.slots.iter().flatten().any(|slot| slot.state == SlotState::Active && slot.last > at) { return Err(Refusal::Invalid); }
        self.validate_scramble_split(at, None)?;
        let next_slice = self.next_slice.checked_add(1).ok_or(Refusal::Overflow)?;
        self.boundaries.observe_day(at).map_err(|_| Refusal::Invalid)?;
        for slot in self.slots.iter_mut().flatten() {
            if slot.state == SlotState::Active { slot.state = SlotState::Closed(at); }
        }
        self.next_slice = next_slice;
        self.split_scrambles(at, at, None)
    }
    /// Copy a scope for transfer; old in-flight sources retain their receipt.
    pub fn export_character_scope(&self, character: i64) -> Option<boundaries::Boundary> {
        self.boundaries.export_scope(character)
    }
    pub fn mark_character_scopes_dirty(&mut self) { self.scope_cleanup_needed = true; }
    pub fn scope_cleanup_needed(&self) -> bool { self.scope_cleanup_needed }
    /// Called at a rare membership boundary or checkpoint, not on each hit.
    pub fn prune_character_scopes(&mut self, live_pilots: impl IntoIterator<Item = i64>) -> usize {
        let mut keep: std::collections::HashSet<i64> = live_pilots.into_iter().collect();
        keep.extend(self.replay.subjects());
        keep.extend(self.scrambles.subjects());
        let removed = self.boundaries.prune(&keep);
        self.scope_cleanup_needed = false;
        removed
    }
    /// The destination must import before admitting the transferred character.
    pub fn import_character_scope(&mut self, character: i64, receipt: boundaries::Boundary) -> Result<boundaries::Boundary, Refusal> {
        if self.slots.iter().flatten().any(|slot|
            (slot.key.source.actor == Some(character) || slot.key.target.character_id == Some(character))
                && (slot.reservations != 0 || slot.state == SlotState::Active)) { return Err(Refusal::Busy); }
        self.boundaries.import_scope(character, receipt)
    }
}
#[cfg(test)] mod tests;
