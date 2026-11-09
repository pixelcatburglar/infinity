use super::*;
use crate::lua::events::combat::{CombatEntity, HpLayer};
use crate::lua::events::resources::{FactContext, FactOrigin};
use super::super::CombatSource;

fn entity(item_id: i64, character_id: Option<i64>) -> CombatEntity {
    CombatEntity { item_id, type_id: 1, incarnation: [1; 16], character_id,
        corporation_id: None, alliance_id: None, faction_id: None }
}
fn source(sequence: u64, actor: Option<i64>) -> CombatSource {
    let mut activation = [0; 16];
    activation[15] = 1;
    CombatSource { actual_actor_character: actor, entity: entity(10, actor),
        context: FactContext { corporation_id: None, alliance_id: None, faction_id: None, ship_id: Some(10),
            ship_type_id: Some(1), system_id: Some(1), station_id: None, origin: FactOrigin::Natural },
        module_id: Some(20), effect_id: Some(30), weapon_type_id: Some(40), source_is_npc: actor.is_none(),
        controller_incarnation: actor.map(|_| [1; 16]), activation, sequence }
}
fn ledger() -> CombatFacts {
    let mut facts = CombatFacts::new(7, 1, 32, 4);
    facts.register_source(source(0, Some(1)).activation, 1, 0).unwrap();
    facts
}
fn hit(facts: &mut CombatFacts, sequence: u64, at: i64) {
    let target = entity(11, Some(2));
    facts.reserve_attempt(&source(sequence, Some(1))).unwrap();
    let mut reservation = facts.reserve_damage(source(sequence, Some(1)), &target, at).unwrap();
    facts.commit_damage(&mut reservation, &target, [1.0, 0.0, 0.0], false).unwrap();
    facts.release(reservation);
}

#[test]
fn skips_reserved_and_zero_rows_then_captures_successes() {
    let mut facts = ledger();
    let target = entity(11, Some(2));
    facts.reserve_attempt(&source(0, Some(1))).unwrap();
    let mut reservation = facts.reserve_damage(source(0, Some(1)), &target, 10).unwrap();
    facts.commit_damage(&mut reservation, &target, [2.0, 0.0, 0.0], false).unwrap();
    assert!(facts.capture(10).unwrap().is_none());
    facts.release(reservation);
    let captured = facts.capture(10).unwrap().unwrap();
    assert_eq!(captured.rows.len(), 1);
    assert_eq!(captured.rows[0].sum.occurrences(), 1);
    assert_eq!(captured.rows[0].sum.to_amount().coefficient, "1");
    assert_eq!(captured.rows[0].sum.to_amount().exponent, 1);
}

#[test]
fn pending_reuses_arc_and_ack_preserves_new_writes() {
    let mut facts = ledger();
    hit(&mut facts, 0, 10);
    let first = facts.capture(10).unwrap().unwrap();
    assert!(Arc::ptr_eq(&first, &facts.pending_batches()[0]));
    hit(&mut facts, 1, 10);
    let second = facts.capture(10).unwrap().unwrap();
    hit(&mut facts, 2, 10);
    assert_eq!(facts.pending_batches().iter().map(|b| b.batch()).collect::<Vec<_>>(), vec![first.batch(), second.batch()]);
    assert!(facts.ack(&first));
    assert!(!facts.ack(&first));
    assert_eq!(facts.key_count(), 2);
    assert!(Arc::ptr_eq(&second, &facts.pending_batches()[0]));
    let third = facts.capture(10).unwrap().unwrap();
    assert_eq!(third.rows.len(), 1);
    assert!(facts.ack(&second));
    assert_eq!(facts.key_count(), 1);
}

#[test]
fn foreign_batch_with_same_number_cannot_ack() {
    let mut facts = ledger();
    let mut other = ledger();
    hit(&mut facts, 0, 10);
    hit(&mut other, 0, 10);
    let captured = facts.capture(10).unwrap().unwrap();
    let foreign = other.capture(10).unwrap().unwrap();
    assert_eq!(captured.batch(), foreign.batch());
    assert!(!facts.ack(&foreign));
    assert_eq!(facts.key_count(), 1);
    assert!(Arc::ptr_eq(&captured, &facts.pending_batches()[0]));
    assert!(facts.ack(&captured));
    assert!(!facts.ack(&captured));
}

#[test]
fn handle_and_identifier_limits_do_not_partially_seal() {
    let mut facts = ledger();
    for sequence in 0..4 {
        hit(&mut facts, sequence, 10);
        facts.capture(10).unwrap().unwrap();
    }
    // The fifth key is made before the fourth handle fills in normal callers;
    // install it directly here to exercise capture's independent preflight.
    let mut slot = facts.slots.iter().flatten().next().unwrap().key;
    slot.slice = facts.next_slice;
    let mut sum = super::super::ExactBinarySum::default();
    assert!(sum.try_add(1.0));
    let index = facts.slots.len();
    facts.slots.push(Some(super::super::Slot { key: slot, sum, first: 10, last: 10, state: SlotState::Active, reservations: 0 }));
    facts.index.insert(slot, index);
    facts.live_key_count += 1;
    facts.dirty_unsealed_keys += 1;
    assert!(matches!(facts.capture(10), Err(Refusal::Full)));
    assert!(matches!(facts.slots[index].as_ref().unwrap().state, SlotState::Active));
    assert_eq!(facts.slots[index].as_ref().unwrap().sum.occurrences(), 1);
    let first = Arc::clone(&facts.pending_batches()[0]);
    assert!(facts.ack(&first));
    facts.next_batch = u64::MAX;
    assert!(matches!(facts.capture(10), Err(Refusal::Overflow)));
    assert!(matches!(facts.slots[index].as_ref().unwrap().state, SlotState::Active));
    facts.next_batch = 4;
    facts.next_slice = u64::MAX;
    assert!(matches!(facts.capture(10), Err(Refusal::Overflow)));
    assert!(matches!(facts.slots[index].as_ref().unwrap().state, SlotState::Active));
}

#[test]
fn closed_boundary_at_same_time_preserves_end_and_epoch() {
    let mut facts = ledger();
    hit(&mut facts, 0, 10);
    facts.close_character_boundary(2, 10).unwrap();
    hit(&mut facts, 1, 10);
    let captured = facts.capture(20).unwrap().unwrap();
    assert_eq!(captured.rows.len(), 2);
    assert_eq!(captured.rows[0].active_until, 10);
    assert_eq!(captured.rows[1].active_until, 20);
    assert_ne!(captured.rows[0].key.epoch, captured.rows[1].key.epoch);
    let epoch = captured.rows[1].key.epoch;
    hit(&mut facts, 2, 20);
    assert_eq!(facts.capture(20).unwrap().unwrap().rows[0].key.epoch, epoch);
}

#[test]
fn frozen_events_keep_subject_options_hull_and_identity() {
    let mut facts = ledger();
    let target = entity(11, None);
    facts.reserve_attempt(&source(0, None)).unwrap();
    let mut damage = facts.reserve_damage(source(0, None), &target, 10).unwrap();
    facts.commit_damage(&mut damage, &target, [0.0, 0.0, 3.0], false).unwrap();
    facts.release(damage);
    let mut repair_source = source(1, Some(1));
    repair_source.module_id = None;
    repair_source.effect_id = None;
    repair_source.activation[7] = 1;
    facts.register_source(repair_source.activation, 1, 1).unwrap();
    facts.reserve_attempt(&repair_source).unwrap();
    let mut repair = facts.reserve_remote_repair(repair_source, &target, HpLayer::Hull, 10).unwrap();
    facts.commit_remote_repair(&mut repair, &target, HpLayer::Hull, 2.0).unwrap();
    facts.release(repair);
    let captured = facts.capture(10).unwrap().unwrap();
    let events = captured.events();
    assert_eq!(events.len(), 2);
    for event in &events {
        assert_eq!(event.ordinal, event.identity.ordinal);
        assert_eq!(event.identity.to_string(), format!("7:1:0:{}:0", event.ordinal));
        match &event.event {
            GameEvent::DamageApplied { char_id, module_id, effect_id, layer, target: actual, slices, .. } => {
                assert_eq!(*char_id, None); assert_eq!(*module_id, Some(20)); assert_eq!(*effect_id, Some(30));
                assert_eq!(*layer, HpLayer::Hull); assert_eq!(*actual, target); assert_eq!(slices[0].successful_count, 1);
            }
            GameEvent::RemoteRepairApplied { char_id, module_id, effect_id, layer, slices, .. } => {
                assert_eq!(*char_id, Some(1)); assert_eq!(*module_id, None); assert_eq!(*effect_id, None);
                assert_eq!(*layer, HpLayer::Hull); assert_eq!(slices[0].successful_count, 1);
            }
            _ => panic!("unexpected event"),
        }
    }
    assert_eq!(events.iter().map(|e| e.identity.to_string()).collect::<Vec<_>>(), captured.events().iter().map(|e| e.identity.to_string()).collect::<Vec<_>>());
}

#[test]
fn dirty_counter_tracks_only_nonempty_unsealed_slots() {
    fn check(facts: &CombatFacts) {
        let scan = facts.slots.iter().flatten().filter(|slot|
            slot.sum.occurrences() != 0 && !matches!(slot.state, SlotState::Sealed { .. })).count();
        assert_eq!(facts.dirty_unsealed_keys, scan);
        assert_eq!(facts.is_dirty(), scan != 0);
    }
    let mut facts = ledger();
    let target = entity(11, Some(2));
    check(&facts);
    facts.reserve_attempt(&source(0, Some(1))).unwrap();
    let mut empty = facts.reserve_damage(source(0, Some(1)), &target, 10).unwrap();
    check(&facts);
    facts.commit_damage(&mut empty, &target, [0.0; 3], false).unwrap();
    facts.release(empty);
    check(&facts);
    hit(&mut facts, 1, 10);
    check(&facts);
    let batch = facts.capture(10).unwrap().unwrap();
    check(&facts);
    assert!(Arc::ptr_eq(&batch, &facts.pending_batches()[0]));
    hit(&mut facts, 2, 10);
    check(&facts);
    assert!(facts.ack(&batch));
    assert!(!facts.ack(&batch));
    check(&facts);
    let second = facts.capture(10).unwrap().unwrap();
    check(&facts);
    assert!(facts.ack(&second));
    check(&facts);
}

#[test]
fn scramble_capture_retains_pending_and_continuation_without_inflating_cycles() {
    let mut facts = ledger();
    let at = infinity_core::consts::FILETIME_DAY + 10;
    facts.boundaries.observe_day(at).unwrap();
    let target = entity(11, Some(2));
    facts.reserve_attempt(&source(0, Some(1))).unwrap();
    let handle = facts.accept_scramble(source(0, Some(1)), target, None, at).unwrap();
    assert!(facts.capture(at).unwrap().is_none());
    assert!(facts.is_dirty());
    assert!(facts.checkpoint_scrambles(at + 5).unwrap().is_empty());
    let first = facts.capture(at + 5).unwrap().unwrap();
    assert!(facts.is_dirty());
    assert_eq!(facts.key_count(), 3);
    let events = first.events();
    assert_eq!(events.len(), 1);
    match &events[0].event {
        GameEvent::WarpScrambleApplied { slices, intervals, .. } => {
            assert_eq!(slices[0].successful_count, 1);
            assert_eq!(intervals[0].active_after, at);
            assert_eq!(intervals[0].active_until, at + 5);
        }
        _ => panic!("expected scramble"),
    }
    assert!(facts.checkpoint_scrambles(at + 10).unwrap().is_empty());
    let second = facts.capture(at + 10).unwrap().unwrap();
    let duration = second.events();
    match &duration[0].event {
        GameEvent::WarpScrambleApplied { slices, intervals, .. } => {
            assert!(slices.is_empty());
            assert_eq!(intervals[0].segment_sequence, 1);
            assert_eq!((intervals[0].active_after, intervals[0].active_until), (at + 5, at + 10));
        }
        _ => panic!("expected scramble"),
    }
    let payload = crate::progress::codec::encode(&duration[0].event).unwrap();
    assert_eq!(crate::progress::codec::decode(crate::progress::codec::SCHEMA_VERSION, "WarpScrambleApplied", &payload).unwrap(), duration[0].event);
    assert!(Arc::ptr_eq(&first, &facts.pending_batches()[0]));
    assert!(facts.ack(&first));
    assert!(facts.is_dirty());
    assert!(facts.ack(&second));
    assert_eq!(facts.key_count(), 2);
    facts.close_scramble(handle, at + 10).unwrap();
    assert!(!facts.is_dirty());
    assert_eq!(facts.key_count(), 0);
}

#[test]
fn scramble_and_hp_capture_preflight_is_atomic_and_ordinals_are_unique() {
    let mut facts = ledger();
    let at = infinity_core::consts::FILETIME_DAY + 10;
    facts.boundaries.observe_day(at).unwrap();
    hit(&mut facts, 0, at);
    let target = entity(11, Some(2));
    facts.reserve_attempt(&source(1, Some(1))).unwrap();
    let handle = facts.accept_scramble(source(1, Some(1)), target, None, at).unwrap();
    facts.close_scramble(handle, at + 1).unwrap();
    facts.handle_capacity = 0;
    assert!(matches!(facts.capture(at + 1), Err(Refusal::Full)));
    assert!(facts.is_dirty());
    assert_eq!(facts.batch_count(), 0);
    assert_eq!(facts.key_count(), 2);
    facts.handle_capacity = 4;
    let captured = facts.capture(at + 1).unwrap().unwrap();
    assert!(!facts.is_dirty());
    let events = captured.events();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].ordinal, 0);
    assert_eq!(events[1].ordinal, 1);
    assert!(matches!(events[1].event, GameEvent::WarpScrambleApplied { .. }));
    assert!(facts.ack(&captured));
    assert_eq!(facts.key_count(), 0);
}

#[test]
fn scramble_refresh_counts_real_cycles_in_their_segment_only() {
    let mut facts = ledger();
    let at = infinity_core::consts::FILETIME_DAY + 10;
    facts.boundaries.observe_day(at).unwrap();
    let target = entity(11, Some(2));
    facts.reserve_attempt(&source(0, Some(1))).unwrap();
    let handle = facts.accept_scramble(source(0, Some(1)), target, None, at).unwrap();
    facts.checkpoint_scrambles(at + 1).unwrap();
    let first = facts.capture(at + 1).unwrap().unwrap();
    facts.reserve_attempt(&source(1, Some(1))).unwrap();
    facts.accept_scramble(source(1, Some(1)), target, Some(handle), at + 2).unwrap();
    facts.reserve_attempt(&source(2, Some(1))).unwrap();
    facts.accept_scramble(source(2, Some(1)), target, Some(handle), at + 3).unwrap();
    facts.close_scramble(handle, at + 4).unwrap();
    let second = facts.capture(at + 4).unwrap().unwrap();
    match (&first.events()[0].event, &second.events()[0].event) {
        (GameEvent::WarpScrambleApplied { slices: before, intervals: old, .. },
         GameEvent::WarpScrambleApplied { slices: after, intervals: new, .. }) => {
            assert_eq!(before[0].successful_count, 1);
            assert_eq!(after[0].successful_count, 2);
            assert_eq!(after[0].amount.coefficient, "1");
            assert_eq!(after[0].amount.exponent, 1);
            assert_eq!(old[0].active_until, new[0].active_after);
        }
        _ => panic!("expected scramble"),
    }
}
