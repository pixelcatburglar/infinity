//! Immutable checkpoint batches for actor-owned combat aggregates.
//! These server facts have no client protocol; conversion to events runs on the
//! checkpoint worker, without database access or live world state.

use std::fmt;
use std::sync::Arc;

use crate::lua::events::combat::{BinaryAmount, CombatInterval, CombatSlice};
use crate::lua::events::GameEvent;
use super::{CapturedFacts, CapturedRow, CombatFacts, Kind, Refusal, SlotState};
use super::scramble::{CapturedScramble, ScrambleSlot};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct CapturedIdentity {
    pub(crate) runtime_epoch: u64,
    pub(crate) system_id: i64,
    pub(crate) batch: u64,
    pub(crate) ordinal: i64,
    pub(crate) slice: u64,
}
impl fmt::Display for CapturedIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:x}:{}:{}:{}:{}", self.runtime_epoch, self.system_id, self.batch, self.ordinal, self.slice)
    }
}

pub(crate) struct CapturedEvent {
    pub(crate) identity: CapturedIdentity,
    pub(crate) ordinal: i64,
    pub(crate) event: GameEvent,
}

impl CombatFacts {
    pub(crate) fn capture(&mut self, at: i64) -> Result<Option<Arc<CapturedFacts>>, Refusal> {
        let mut count = 0usize;
        for slot in self.slots.iter().flatten() {
            if slot.reservations != 0 || slot.sum.occurrences() == 0 { continue; }
            let end = match slot.state {
                SlotState::Active => at,
                SlotState::Closed(end) => end,
                SlotState::Sealed { .. } => continue,
            };
            if end < slot.last { return Err(Refusal::Invalid); }
            count = count.checked_add(1).ok_or(Refusal::Overflow)?;
        }
        let row_count = count;
        for slot in self.scrambles.slots.iter().flatten() {
            if let ScrambleSlot::Segment(segment) = slot {
                if segment.end.is_some_and(|end| segment.count > 0 || end > segment.start) {
                    count = count.checked_add(1).ok_or(Refusal::Overflow)?;
                }
            }
        }
        if count == 0 { return Ok(None); }
        if self.batches.len() >= self.handle_capacity { return Err(Refusal::Full); }
        let next_batch = self.next_batch.checked_add(1).ok_or(Refusal::Overflow)?;
        let next_slice = self.next_slice.checked_add(1).ok_or(Refusal::Overflow)?;
        u32::try_from(count - 1).map_err(|_| Refusal::Overflow)?;
        let batch = self.next_batch;
        let mut rows = Vec::with_capacity(row_count);
        let mut scrambles = Vec::with_capacity(count - row_count);
        for slot in self.slots.iter_mut().flatten() {
            if slot.reservations != 0 || slot.sum.occurrences() == 0 { continue; }
            let active_until = match slot.state {
                SlotState::Active => at,
                SlotState::Closed(end) => end,
                SlotState::Sealed { .. } => continue,
            };
            let ordinal = rows.len() as u32;
            rows.push(CapturedRow { key: slot.key, sum: std::mem::take(&mut slot.sum), first: slot.first, last: slot.last, active_until, ordinal });
            slot.state = SlotState::Sealed { batch, active_until, ordinal };
        }
        for slot in &mut self.scrambles.slots {
            let eligible = matches!(slot, Some(ScrambleSlot::Segment(segment))
                if segment.end.is_some_and(|end| segment.count > 0 || end > segment.start));
            if !eligible { continue; }
            let Some(ScrambleSlot::Segment(segment)) = slot.replace(ScrambleSlot::Sealed(batch)) else { unreachable!() };
            let ordinal = (row_count + scrambles.len()) as u32;
            scrambles.push(CapturedScramble { segment, ordinal });
        }
        self.dirty_unsealed_keys -= count;
        let captured = Arc::new(CapturedFacts { runtime_epoch: self.runtime_epoch, system_id: self.system_id, batch, rows, scrambles });
        self.batches.insert(batch, Arc::clone(&captured));
        self.next_batch = next_batch;
        self.next_slice = next_slice;
        Ok(Some(captured))
    }

    pub(crate) fn pending_batches(&self) -> Vec<Arc<CapturedFacts>> {
        let mut batches: Vec<_> = self.batches.values().map(Arc::clone).collect();
        batches.sort_unstable_by_key(|batch| batch.batch);
        batches
    }

    pub(crate) fn ack(&mut self, captured: &Arc<CapturedFacts>) -> bool {
        let batch = captured.batch;
        if !self.batches.get(&batch).is_some_and(|stored| Arc::ptr_eq(stored, captured)) { return false; }
        self.batches.remove(&batch);
        for (index, entry) in self.slots.iter_mut().enumerate() {
            let Some(slot) = entry.as_ref() else { continue; };
            if !matches!(slot.state, SlotState::Sealed { batch: id, .. } if id == batch) { continue; }
            self.index.remove(&slot.key);
            *entry = None;
            self.live_key_count -= 1;
            self.free_slots.push(index);
        }
        for (index, slot) in self.scrambles.slots.iter_mut().enumerate() {
            if !matches!(slot, Some(ScrambleSlot::Sealed(id)) if *id == batch) { continue; }
            *slot = None;
            self.scrambles.free.push(index);
            self.live_key_count -= 1;
        }
        true
    }
}

impl CapturedFacts {
    #[cfg(test)]
    pub(crate) fn batch(&self) -> u64 { self.batch }

    /// Convert frozen rows only on the checkpoint worker, outside the actor.
    pub(crate) fn events(&self) -> Vec<CapturedEvent> {
        let mut events = Vec::with_capacity(self.rows.len() + self.scrambles.len());
        events.extend(self.rows.iter().map(|row| {
            let key = row.key;
            let ordinal = i64::from(row.ordinal);
            let identity = CapturedIdentity { runtime_epoch: self.runtime_epoch, system_id: self.system_id, batch: self.batch, ordinal, slice: key.slice };
            let slices = vec![CombatSlice { index: key.slice, qualification_epoch: key.epoch, boundary_at: key.boundary_at,
                active_until: row.active_until, first_occurred_at: row.first, last_occurred_at: row.last,
                successful_count: row.sum.occurrences(), amount: row.sum.to_amount() }];
            let source = key.source;
            let event = match key.kind {
                Kind::Damage => GameEvent::DamageApplied { char_id: source.actor, context: source.context, source: source.entity, target: key.target,
                    module_id: source.module_id, weapon_type_id: source.weapon_type_id, effect_id: source.effect_id, layer: key.layer, slices },
                Kind::RemoteRepair => GameEvent::RemoteRepairApplied { char_id: source.actor, context: source.context, source: source.entity, target: key.target,
                    module_id: source.module_id, effect_id: source.effect_id, layer: key.layer, slices },
            };
            CapturedEvent { identity, ordinal, event }
        }));
        events.extend(self.scrambles.iter().map(|row| {
            let segment = &row.segment;
            let ordinal = i64::from(row.ordinal);
            let identity = CapturedIdentity { runtime_epoch: self.runtime_epoch, system_id: self.system_id, batch: self.batch, ordinal, slice: segment.slice };
            let end = segment.end.expect("captured scramble is closed");
            let slices = if segment.count == 0 { Vec::new() } else {
                let shift = segment.count.trailing_zeros();
                vec![CombatSlice { index: segment.slice, qualification_epoch: segment.epoch, boundary_at: segment.boundary_at,
                    active_until: end, first_occurred_at: segment.first, last_occurred_at: segment.last,
                    successful_count: segment.count, amount: BinaryAmount { coefficient: (segment.count >> shift).to_string(), exponent: shift as i16 } }]
            };
            let intervals = if end > segment.start {
                vec![CombatInterval { activation: segment.activation, segment_sequence: segment.segment_sequence, active_after: segment.start, active_until: end }]
            } else { Vec::new() };
            let source = segment.source;
            let event = GameEvent::WarpScrambleApplied { char_id: source.actual_actor_character, context: source.context,
                source: source.entity, target: segment.target, module_id: source.module_id, effect_id: source.effect_id, slices, intervals };
            CapturedEvent { identity, ordinal, event }
        }));
        events
    }
}

#[cfg(test)] mod tests;
