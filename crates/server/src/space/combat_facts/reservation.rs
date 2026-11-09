//! Reservation and atomic aggregate mutation for combat checkpoints.
//! The caller owns the selected AOE target buffer; no per-hit collection is allocated.

use super::*;

pub(super) const LAYERS: [HpLayer; 3] = [HpLayer::Shield, HpLayer::Armor, HpLayer::Hull];
const NONE: usize = usize::MAX;

/// Caller-owned target selection and scratch space for a multi-target application.
pub(crate) struct ReservedTarget<T = ()> {
    pub target: CombatEntity,
    pub amounts: [f64; 3],
    pub payload: T,
    slots: [usize; 3],
    applied: bool,
}
impl<T> ReservedTarget<T> {
    pub fn new(target: CombatEntity, amounts: [f64; 3], payload: T) -> Self {
        Self { target, amounts, payload, slots: [NONE; 3], applied: false }
    }
}
enum Targets<'a, T> { Single(ReservedTarget<T>), Union(&'a mut [ReservedTarget<T>]) }
pub(crate) struct Reservation<'a, T = ()> {
    source: CombatSource, at: i64, kind: Kind, targets: Targets<'a, T>,
}
impl<T> Reservation<'_, T> {
    pub fn payload_mut(&mut self, index: usize) -> Option<&mut T> {
        self.targets_mut().get_mut(index).map(|entry| &mut entry.payload)
    }
    fn targets(&self) -> &[ReservedTarget<T>] {
        match &self.targets { Targets::Single(target) => std::slice::from_ref(target), Targets::Union(targets) => targets }
    }
    fn targets_mut(&mut self) -> &mut [ReservedTarget<T>] {
        match &mut self.targets { Targets::Single(target) => std::slice::from_mut(target), Targets::Union(targets) => targets }
    }
}
impl CombatFacts {
    pub fn reserve_damage(&mut self, source: CombatSource, target: &CombatEntity, at: i64) -> Result<Reservation<'static>, Refusal> {
        let mut entry = ReservedTarget::new(*target, [0.0; 3], ());
        self.reserve_targets(source, std::slice::from_mut(&mut entry), at, Kind::Damage, &LAYERS, false)?;
        Ok(Reservation { source, at, kind: Kind::Damage, targets: Targets::Single(entry) })
    }
    pub fn reserve_damage_union<'a, T>(&mut self, source: CombatSource, targets: &'a mut [ReservedTarget<T>], at: i64) -> Result<Reservation<'a, T>, Refusal> {
        self.reserve_targets(source, targets, at, Kind::Damage, &LAYERS, true)?;
        Ok(Reservation { source, at, kind: Kind::Damage, targets: Targets::Union(targets) })
    }
    pub fn reserve_remote_repair(&mut self, source: CombatSource, target: &CombatEntity, layer: HpLayer, at: i64) -> Result<Reservation<'static>, Refusal> {
        let mut entry = ReservedTarget::new(*target, [0.0; 3], ());
        self.reserve_targets(source, std::slice::from_mut(&mut entry), at, Kind::RemoteRepair, std::slice::from_ref(&layer), false)?;
        Ok(Reservation { source, at, kind: Kind::RemoteRepair, targets: Targets::Single(entry) })
    }
    fn reserve_targets<T>(&mut self, source: CombatSource, targets: &mut [ReservedTarget<T>], at: i64, kind: Kind, layers: &[HpLayer], preflight: bool) -> Result<(), Refusal> {
        if targets.is_empty() { return Err(Refusal::Invalid); }
        if self.batches.len() >= self.handle_capacity { return Err(Refusal::Full); }
        if self.boundaries.day_would_change(at).map_err(|_| Refusal::Invalid)?
            && (!self.scrambles.active.is_empty() || self.slots.iter().flatten().any(|slot| slot.reservations != 0)) { return Err(Refusal::Busy); }
        let next_slice = self.next_slice.checked_add(1).ok_or(Refusal::Overflow)?;
        if let Some(day) = self.boundaries.observe_day(at).map_err(|_| Refusal::Invalid)? {
            for slot in self.slots.iter_mut().flatten() {
                if slot.state == SlotState::Active { slot.state = SlotState::Closed(day.close_at); }
            }
            self.next_slice = next_slice;
        }
        let source_key = SourceKey::from(source);
        let mut missing = 0usize;
        let mut previous = None;
        for target in targets.iter() {
            let identity = (target.target.item_id, target.target.incarnation);
            if previous.is_some_and(|prior| prior >= identity) { return Err(Refusal::Invalid); }
            previous = Some(identity);
            if preflight && target.amounts.iter().any(|amount| !amount.is_finite() || *amount < 0.0) { return Err(Refusal::Invalid); }
            let boundary = self.boundaries.current(source.actual_actor_character, target.target.character_id);
            if at < boundary.at { return Err(Refusal::Invalid); }
            for &layer in layers {
                let key = AggregateKey { kind, source: source_key, target: target.target, layer, slice: self.next_slice, epoch: boundary.epoch, boundary_at: boundary.at };
                if let Some(&index) = self.index.get(&key) {
                    let slot = self.slots[index].as_ref().expect("indexed slot");
                    if slot.state != SlotState::Active || slot.last > at || slot.reservations == usize::MAX || slot.sum.occurrences() == u64::MAX { return Err(Refusal::Busy); }
                    if preflight {
                        let mut sum = slot.sum.clone();
                        let value = target.amounts[layer_index(layer)];
                        if value > 0.0 && !sum.try_add(value) { return Err(Refusal::Overflow); }
                    }
                } else { missing = missing.checked_add(1).ok_or(Refusal::Overflow)?; }
            }
        }
        if self.live_key_count.checked_add(missing).is_none_or(|n| n > self.key_capacity) { return Err(Refusal::Full); }
        let generation = generation(source.activation);
        if !self.replay.matches_subject(source.activation, source.actual_actor_character) { return Err(Refusal::Invalid); }
        self.replay.claim(source.activation, generation, source.sequence)?;
        for target in targets {
            target.slots = [NONE; 3]; target.applied = false;
            let boundary = self.boundaries.current(source.actual_actor_character, target.target.character_id);
            for &layer in layers {
                let key = AggregateKey { kind, source: source_key, target: target.target, layer, slice: self.next_slice, epoch: boundary.epoch, boundary_at: boundary.at };
                let index = if let Some(&index) = self.index.get(&key) {
                    self.slots[index].as_mut().expect("indexed slot").reservations += 1;
                    index
                } else {
                    let slot = Slot { key, sum: ExactBinarySum::default(), first: at, last: at, state: SlotState::Active, reservations: 1 };
                    let index = if let Some(index) = self.free_slots.pop() { self.slots[index] = Some(slot); index } else { self.slots.push(Some(slot)); self.slots.len() - 1 };
                    self.index.insert(key, index); self.live_key_count += 1; index
                };
                target.slots[layer_index(layer)] = index;
            }
        }
        Ok(())
    }
    pub fn commit_damage<T>(&mut self, reservation: &mut Reservation<'_, T>, target: &CombatEntity, amounts: [f64; 3], lethal: bool) -> Result<Option<PendingDamage>, Refusal> {
        if reservation.kind != Kind::Damage { return Err(Refusal::Invalid); }
        if let Targets::Union(_) = &reservation.targets {
            let entry = reservation.targets().iter().find(|entry| entry.target == *target).ok_or(Refusal::Invalid)?;
            if entry.amounts.map(f64::to_bits) != amounts.map(f64::to_bits) { return Err(Refusal::Invalid); }
        }
        self.commit(reservation, target, amounts, lethal)
    }
    pub fn commit_remote_repair<T>(&mut self, reservation: &mut Reservation<'_, T>, target: &CombatEntity, layer: HpLayer, amount: f64) -> Result<(), Refusal> {
        if reservation.kind != Kind::RemoteRepair { return Err(Refusal::Invalid); }
        let mut amounts = [0.0; 3]; amounts[layer_index(layer)] = amount;
        self.commit(reservation, target, amounts, false).map(|_| ())
    }
    fn commit<T>(&mut self, reservation: &mut Reservation<'_, T>, target: &CombatEntity, amounts: [f64; 3], lethal: bool) -> Result<Option<PendingDamage>, Refusal> {
        if amounts.iter().any(|value| !value.is_finite() || *value < 0.0) { return Err(Refusal::Invalid); }
        let entry = reservation.targets().iter().find(|entry| entry.target == *target).ok_or(Refusal::Invalid)?;
        if entry.applied { return Err(Refusal::Replay); }
        let indices = entry.slots;
        let mut sums: [ExactBinarySum; 3] = std::array::from_fn(|_| ExactBinarySum::default());
        for layer in 0..3 {
            if indices[layer] == NONE { if amounts[layer] > 0.0 { return Err(Refusal::Invalid); } continue; }
            let slot = self.slots[indices[layer]].as_ref().ok_or(Refusal::Invalid)?;
            if slot.last > reservation.at { return Err(Refusal::Invalid); }
            if !lethal { sums[layer] = slot.sum.clone(); }
            if amounts[layer] > 0.0 && !sums[layer].try_add(amounts[layer]) { return Err(Refusal::Overflow); }
        }
        let any = amounts.iter().any(|amount| *amount > 0.0);
        for layer in 0..3 {
            if indices[layer] == NONE || lethal || amounts[layer] == 0.0 { continue; }
            let slot = self.slots[indices[layer]].as_mut().expect("reserved slot");
            if slot.sum.occurrences() == 0 { slot.first = reservation.at; self.dirty_unsealed_keys += 1; }
            slot.last = reservation.at; slot.sum = sums[layer].clone();
        }
        reservation.targets_mut().iter_mut().find(|entry| entry.target == *target).expect("selected target").applied = true;
        if lethal && any {
            let source = reservation.source;
            let index = indices.into_iter().find(|&index| index != NONE).ok_or(Refusal::Invalid)?;
            let key = self.slots[index].as_ref().ok_or(Refusal::Invalid)?.key;
            return Ok(Some(PendingDamage { settlement: SettlementId { source_instance: source.activation, generation: generation(source.activation), sequence: source.sequence, target_id: target.item_id, target_incarnation: target.incarnation }, source, target: *target, occurred_at: reservation.at, amounts: sums, slice: key.slice, qualification_epoch: key.epoch, boundary_at: key.boundary_at }));
        }
        Ok(None)
    }
    pub fn release<T>(&mut self, reservation: Reservation<'_, T>) {
        for entry in reservation.targets() {
            for &index in &entry.slots {
                if index == NONE { continue; }
                let slot = self.slots[index].as_mut().expect("reserved slot");
                slot.reservations -= 1;
                if slot.reservations == 0 && slot.sum.occurrences() == 0 {
                    self.index.remove(&slot.key); self.slots[index] = None;
                    self.free_slots.push(index); self.live_key_count -= 1;
                }
            }
        }
        self.scope_cleanup_needed |= self.replay.finish(reservation.source.activation, generation(reservation.source.activation), reservation.source.sequence, true);
    }
}
fn layer_index(layer: HpLayer) -> usize { match layer { HpLayer::Shield => 0, HpLayer::Armor => 1, HpLayer::Hull => 2 } }
fn generation(activation: [u8; 16]) -> u64 { u64::from_be_bytes(activation[8..].try_into().expect("fixed activation")) }
