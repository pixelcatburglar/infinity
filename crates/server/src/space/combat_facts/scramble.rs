//! Actor-owned actual scramble intervals within the combat checkpoint budget.
//! The projection owner remains the physical authority; this stores no modifiers.
use std::collections::HashMap;
use crate::lua::events::combat::{CombatEntity, SettlementId, SettlementKey, SettlementPart};
use super::{CombatFacts, CombatSource, Refusal, SourceKey};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct ScrambleHandle(u64);

pub(super) struct ScrambleSegment {
    pub source: CombatSource, pub target: CombatEntity, pub activation: SettlementKey,
    pub segment_sequence: u64, pub slice: u64, pub epoch: u64, pub boundary_at: i64,
    pub start: i64, pub end: Option<i64>, pub first: i64, pub last: i64, pub count: u64,
}
pub(super) enum ScrambleSlot { Segment(ScrambleSegment), Sealed(u64) }
pub(super) struct CapturedScramble { pub segment: ScrambleSegment, pub ordinal: u32 }
#[derive(Default)]
pub(super) struct ScrambleStore {
    pub slots: Vec<Option<ScrambleSlot>>, pub free: Vec<usize>,
    pub active: HashMap<ScrambleHandle, usize>, pub next_handle: u64,
}
impl ScrambleStore {
    fn initialize(&mut self, capacity: usize) {
        if self.slots.capacity() == 0 {
            self.slots = Vec::with_capacity(capacity);
            self.free = Vec::with_capacity(capacity);
            self.active = HashMap::with_capacity(capacity);
        }
    }
    fn insert(&mut self, segment: ScrambleSegment) -> usize {
        let slot = Some(ScrambleSlot::Segment(segment));
        if let Some(index) = self.free.pop() { self.slots[index] = slot; index }
        else { self.slots.push(slot); self.slots.len() - 1 }
    }
    pub(super) fn subjects(&self) -> impl Iterator<Item = i64> + '_ {
        self.active.values().filter_map(|&index| match self.slots[index].as_ref() {
            Some(ScrambleSlot::Segment(segment)) => Some([segment.source.actual_actor_character, segment.target.character_id]),
            _ => None,
        }).flatten().flatten()
    }
}
impl CombatFacts {
    /// The native attempt was prepared at launch. Only a successful return may
    /// be followed by the corresponding physical projection upsert.
    pub fn accept_scramble(&mut self, source: CombatSource, target: CombatEntity, previous: Option<ScrambleHandle>, at: i64) -> Result<ScrambleHandle, Refusal> {
        if self.is_full() { return Err(Refusal::Full); }
        if self.boundaries.day_would_change(at).map_err(|_| Refusal::Invalid)? { return Err(Refusal::Busy); }
        self.boundaries.observe_day(at).map_err(|_| Refusal::Invalid)?;
        let generation = u64::from_be_bytes(source.activation[8..].try_into().expect("fixed activation"));
        if !self.replay.matches_subject(source.activation, source.actual_actor_character) { return Err(Refusal::Invalid); }
        if let Some(handle) = previous {
            let index = *self.scrambles.active.get(&handle).ok_or(Refusal::Invalid)?;
            let Some(ScrambleSlot::Segment(segment)) = self.scrambles.slots[index].as_ref() else { return Err(Refusal::Invalid); };
            if SourceKey::from(segment.source) != SourceKey::from(source) || segment.target != target || at < segment.last || segment.end.is_some() { return Err(Refusal::Invalid); }
            let count = segment.count.checked_add(1).ok_or(Refusal::Overflow)?;
            self.replay.claim(source.activation, generation, source.sequence)?;
            let Some(ScrambleSlot::Segment(segment)) = self.scrambles.slots[index].as_mut() else { unreachable!() };
            if segment.count == 0 { segment.first = at; }
            segment.last = at; segment.count = count;
            self.scope_cleanup_needed |= self.replay.finish(source.activation, generation, source.sequence, true);
            return Ok(handle);
        }
        if self.live_key_count.checked_add(2).is_none_or(|count| count > self.key_capacity) { return Err(Refusal::Full); }
        let next_handle = self.scrambles.next_handle.checked_add(1).ok_or(Refusal::Overflow)?;
        let boundary = self.boundaries.current(source.actual_actor_character, target.character_id);
        if at < boundary.at { return Err(Refusal::Invalid); }
        self.replay.claim(source.activation, generation, source.sequence)?;
        self.scrambles.initialize(self.key_capacity);
        let handle = ScrambleHandle(self.scrambles.next_handle);
        self.scrambles.next_handle = next_handle;
        let settlement = SettlementId { source_instance: source.activation, generation, sequence: source.sequence, target_id: target.item_id, target_incarnation: target.incarnation };
        let segment = ScrambleSegment { source, target, activation: SettlementKey { settlement, part: SettlementPart::Whole }, segment_sequence: 0,
            slice: self.next_slice, epoch: boundary.epoch, boundary_at: boundary.at, start: at, end: None, first: at, last: at, count: 1 };
        let index = self.scrambles.insert(segment);
        self.scrambles.active.insert(handle, index);
        self.live_key_count += 2; // Active segment plus its reserved continuation/endpoint.
        self.dirty_unsealed_keys += 1;
        self.scope_cleanup_needed |= self.replay.finish(source.activation, generation, source.sequence, true);
        Ok(handle)
    }
    pub fn scramble_matches(&self, handle: ScrambleHandle, source: CombatSource, target: CombatEntity) -> bool {
        self.scrambles.active.get(&handle).and_then(|&index| self.scrambles.slots[index].as_ref()).is_some_and(|slot| {
            matches!(slot, ScrambleSlot::Segment(segment) if segment.end.is_none()
                && SourceKey::from(segment.source) == SourceKey::from(source) && segment.target == target)
        })
    }
    pub fn scramble_identity(&self, handle: ScrambleHandle) -> Option<(CombatSource, CombatEntity)> {
        let index = *self.scrambles.active.get(&handle)?;
        match self.scrambles.slots[index].as_ref()? {
            ScrambleSlot::Segment(segment) if segment.end.is_none() => Some((segment.source, segment.target)),
            _ => None,
        }
    }

    /// Rebind only the just-opened zero-cycle continuation after an affiliation
    /// boundary; physical incarnations and the native activation must not change.
    pub fn rebind_scramble(&mut self, handle: ScrambleHandle, source: CombatSource, target: CombatEntity, at: i64) -> Result<(), Refusal> {
        let index = *self.scrambles.active.get(&handle).ok_or(Refusal::Invalid)?;
        let Some(ScrambleSlot::Segment(segment)) = self.scrambles.slots[index].as_mut() else { return Err(Refusal::Invalid); };
        let old = segment.source;
        if segment.end.is_some() || segment.start != at || segment.count != 0
            || old.activation != source.activation || old.actual_actor_character != source.actual_actor_character
            || old.controller_incarnation != source.controller_incarnation || old.source_is_npc != source.source_is_npc
            || old.entity.item_id != source.entity.item_id || old.entity.incarnation != source.entity.incarnation
            || old.entity.type_id != source.entity.type_id || old.module_id != source.module_id
            || old.effect_id != source.effect_id || old.weapon_type_id != source.weapon_type_id
            || segment.target.item_id != target.item_id || segment.target.incarnation != target.incarnation
            || segment.target.type_id != target.type_id || segment.target.character_id != target.character_id {
            return Err(Refusal::Invalid);
        }
        let boundary = self.boundaries.current(source.actual_actor_character, target.character_id);
        segment.source = source; segment.target = target;
        segment.epoch = boundary.epoch; segment.boundary_at = boundary.at;
        Ok(())
    }

    /// Uses the segment's existing key and continuation reservation; it never
    /// needs capacity and does not invent a future endpoint.
    pub fn close_scramble(&mut self, handle: ScrambleHandle, at: i64) -> Result<(), Refusal> {
        let index = *self.scrambles.active.get(&handle).ok_or(Refusal::Invalid)?;
        let Some(ScrambleSlot::Segment(segment)) = self.scrambles.slots[index].as_ref() else { return Err(Refusal::Invalid); };
        if at < segment.last || at < segment.start { return Err(Refusal::Invalid); }
        self.scrambles.active.remove(&handle);
        self.scope_cleanup_needed = true;
        self.live_key_count -= 1; // release continuation reservation
        let Some(ScrambleSlot::Segment(segment)) = self.scrambles.slots[index].as_mut() else { unreachable!() };
        segment.end = Some(at);
        if segment.count == 0 && at == segment.start {
            self.scrambles.slots[index] = None; self.scrambles.free.push(index);
            self.live_key_count -= 1; self.dirty_unsealed_keys -= 1;
        }
        Ok(())
    }

    /// Close checkpoint prefixes before capture. Capacity exhaustion ends the
    /// physical projection at this actual checkpoint rather than dropping data.
    pub fn checkpoint_scrambles(&mut self, at: i64) -> Result<Vec<ScrambleHandle>, Refusal> {
        self.split_scrambles(at, at, None)
    }

    pub(super) fn validate_scramble_split(&self, at: i64, character: Option<i64>) -> Result<(), Refusal> {
        for &index in self.scrambles.active.values() {
            let Some(ScrambleSlot::Segment(segment)) = self.scrambles.slots[index].as_ref() else { return Err(Refusal::Invalid); };
            if character.is_some_and(|id| segment.source.actual_actor_character != Some(id) && segment.target.character_id != Some(id)) { continue; }
            if at < segment.last || at < segment.start { return Err(Refusal::Invalid); }
        }
        Ok(())
    }

    pub(super) fn split_scrambles(&mut self, boundary_at: i64, observed_at: i64, character: Option<i64>) -> Result<Vec<ScrambleHandle>, Refusal> {
        self.validate_scramble_split(boundary_at, character)?;
        if observed_at < boundary_at { return Err(Refusal::Invalid); }
        let handles: Vec<_> = self.scrambles.active.keys().copied().collect();
        let mut ended = Vec::new();
        for handle in handles {
            let index = self.scrambles.active[&handle];
            let Some(ScrambleSlot::Segment(segment)) = self.scrambles.slots[index].as_mut() else { unreachable!() };
            if character.is_some_and(|id| segment.source.actual_actor_character != Some(id) && segment.target.character_id != Some(id)) { continue; }
            if segment.count == 0 && segment.start == boundary_at {
                let boundary = self.boundaries.current(segment.source.actual_actor_character, segment.target.character_id);
                segment.epoch = boundary.epoch; segment.boundary_at = boundary.at;
                continue;
            }
            segment.end = Some(boundary_at);
            let source = segment.source; let target = segment.target; let activation = segment.activation;
            let (Some(next_segment), Some(next_slice)) = (segment.segment_sequence.checked_add(1), segment.slice.checked_add(1)) else {
                self.scrambles.active.remove(&handle); self.live_key_count -= 1; ended.push(handle); continue;
            };
            let boundary = self.boundaries.current(source.actual_actor_character, target.character_id);
            let next = ScrambleSegment { source, target, activation, segment_sequence: next_segment, slice: next_slice,
                epoch: boundary.epoch, boundary_at: boundary.at, start: boundary_at, end: None, first: boundary_at, last: boundary_at, count: 0 };
            if self.live_key_count < self.key_capacity {
                let next_index = self.scrambles.insert(next);
                self.scrambles.active.insert(handle, next_index);
                self.live_key_count += 1; // replace the consumed continuation reservation
                self.dirty_unsealed_keys += 1;
            } else if boundary_at == observed_at {
                self.scrambles.active.remove(&handle);
                self.live_key_count -= 1; // unused continuation reservation
                ended.push(handle);
            } else {
                let mut next = next;
                next.end = Some(observed_at);
                self.scrambles.insert(next); // consume reserved key; actual final duration remains journalable
                self.scrambles.active.remove(&handle);
                self.dirty_unsealed_keys += 1;
                ended.push(handle);
            }
        }
        if !ended.is_empty() { self.scope_cleanup_needed = true; }
        Ok(ended)
    }
}
#[cfg(test)] mod tests;
