use super::*;
use crate::lua::events::resources::{FactContext, FactOrigin};
use infinity_core::consts::FILETIME_DAY;

fn entity(id: i64, character: Option<i64>) -> CombatEntity {
    CombatEntity { item_id: id, type_id: 1, incarnation: [id as u8; 16], character_id: character,
        corporation_id: None, alliance_id: None, faction_id: None }
}
fn source(sequence: u64) -> CombatSource {
    let mut activation = [0; 16]; activation[15] = 1;
    CombatSource { actual_actor_character: Some(1), entity: entity(1, Some(1)), source_is_npc: false,
        controller_incarnation: Some([1; 16]), context: FactContext { corporation_id: None, alliance_id: None,
            faction_id: None, ship_id: Some(1), ship_type_id: Some(1), system_id: Some(1), station_id: None,
            origin: FactOrigin::Natural }, module_id: Some(2), effect_id: Some(3), weapon_type_id: None, activation, sequence }
}
fn ledger(capacity: usize) -> CombatFacts {
    let mut facts = CombatFacts::new(7, 1, capacity, 4);
    facts.register_source(source(0).activation, 1, 0).unwrap(); facts
}
fn accept(facts: &mut CombatFacts, seq: u64, previous: Option<ScrambleHandle>, at: i64) -> ScrambleHandle {
    facts.reserve_attempt(&source(seq)).unwrap();
    facts.accept_scramble(source(seq), entity(2, Some(2)), previous, at).unwrap()
}
fn segment(facts: &CombatFacts, handle: ScrambleHandle) -> &ScrambleSegment {
    let index = facts.scrambles.active[&handle];
    let Some(ScrambleSlot::Segment(segment)) = &facts.scrambles.slots[index] else { panic!("active segment") };
    segment
}

#[test]
fn refresh_merges_cycles_and_actual_stop_closes_interval() {
    let mut facts = ledger(4);
    let handle = accept(&mut facts, 0, None, 10);
    assert_eq!(accept(&mut facts, 1, Some(handle), 12), handle);
    assert_eq!(facts.key_count(), 2);
    assert_eq!(segment(&facts, handle).count, 2);
    facts.close_scramble(handle, 15).unwrap();
    assert_eq!(facts.key_count(), 1);
    let batch = facts.capture(15).unwrap().unwrap();
    let events = batch.events();
    let crate::lua::events::GameEvent::WarpScrambleApplied { slices, intervals, .. } = &events[0].event else { panic!("scramble") };
    assert_eq!(slices[0].successful_count, 2);
    assert_eq!((intervals[0].active_after, intervals[0].active_until), (10, 15));
}

#[test]
fn checkpoint_splits_interval_and_continuation_without_fake_cycle() {
    let mut facts = ledger(4);
    let handle = accept(&mut facts, 0, None, 10);
    assert!(facts.checkpoint_scrambles(20).unwrap().is_empty());
    let epoch = segment(&facts, handle).epoch;
    let first = facts.capture(20).unwrap().unwrap();
    assert_eq!(first.events().len(), 1);
    assert!(facts.is_dirty());
    assert_eq!(segment(&facts, handle).count, 0);
    assert_eq!(segment(&facts, handle).segment_sequence, 1);
    assert_eq!(segment(&facts, handle).slice, 1);
    assert_eq!(segment(&facts, handle).epoch, epoch);
    facts.close_scramble(handle, 30).unwrap();
    let second = facts.capture(30).unwrap().unwrap();
    let events = second.events();
    let crate::lua::events::GameEvent::WarpScrambleApplied { slices, intervals, .. } = &events[0].event else { panic!("scramble") };
    assert!(slices.is_empty());
    assert_eq!((intervals[0].active_after, intervals[0].active_until), (20, 30));
    assert!(facts.ack(&first)); assert!(facts.ack(&second)); assert!(!facts.is_dirty());
}

#[test]
fn qualification_same_time_splits_count_and_epoch() {
    let mut facts = ledger(5);
    let handle = accept(&mut facts, 0, None, 10);
    let (receipt, ended) = facts.close_character_boundary(2, 10).unwrap();
    assert!(ended.is_empty());
    assert_eq!(segment(&facts, handle).epoch, receipt.epoch);
    accept(&mut facts, 1, Some(handle), 10);
    assert_eq!(segment(&facts, handle).count, 1);
    facts.close_scramble(handle, 11).unwrap();
    let batch = facts.capture(11).unwrap().unwrap();
    assert_eq!(batch.events().len(), 2);
}

#[test]
fn full_capacity_refuses_new_but_closes_active_at_checkpoint() {
    let mut too_small = ledger(1);
    too_small.reserve_attempt(&source(0)).unwrap();
    assert!(matches!(too_small.accept_scramble(source(0), entity(2, Some(2)), None, 10), Err(Refusal::Full)));
    too_small.finish_attempt(&source(0));
    let mut facts = ledger(2);
    let handle = accept(&mut facts, 0, None, 10);
    assert_eq!(facts.checkpoint_scrambles(20).unwrap(), vec![handle]);
    assert_eq!(facts.key_count(), 1);
    let batch = facts.capture(20).unwrap().unwrap();
    assert_eq!(batch.events().len(), 1);
    assert!(facts.ack(&batch)); assert_eq!(facts.key_count(), 0);
}

#[test]
fn observed_multiday_jump_has_one_actual_boundary_and_true_interval() {
    let mut facts = ledger(4);
    let start = FILETIME_DAY + 10;
    let handle = accept(&mut facts, 0, None, start);
    let observed = 4 * FILETIME_DAY + 20;
    assert!(facts.observe_utc(observed).unwrap().is_empty());
    let batch = facts.capture(observed).unwrap().unwrap();
    let events = batch.events();
    let crate::lua::events::GameEvent::WarpScrambleApplied { intervals, .. } = &events[0].event else { panic!("scramble") };
    assert_eq!((intervals[0].active_after, intervals[0].active_until), (start, observed));
    let intersection = (observed.min(2 * FILETIME_DAY) - start.max(FILETIME_DAY)).max(0);
    assert_eq!(intersection, FILETIME_DAY - 10);
    assert_eq!(segment(&facts, handle).segment_sequence, 1);
}

#[test]
fn full_qualification_boundary_ends_physical_without_losing_prefix() {
    let mut facts = ledger(2);
    let handle = accept(&mut facts, 0, None, 10);
    let (_, ended) = facts.close_character_boundary(2, 12).unwrap();
    assert_eq!(ended, vec![handle]);
    assert_eq!(facts.key_count(), 1);
    let batch = facts.capture(12).unwrap().unwrap();
    let events = batch.events();
    let crate::lua::events::GameEvent::WarpScrambleApplied { intervals, .. } = &events[0].event else { panic!("scramble") };
    assert_eq!(intervals[0].active_until, 12);
}

#[test]
fn cycle_count_overflow_refuses_before_any_mutation() {
    let mut facts = ledger(4);
    let handle = accept(&mut facts, 0, None, 10);
    let index = facts.scrambles.active[&handle];
    let Some(ScrambleSlot::Segment(row)) = facts.scrambles.slots[index].as_mut() else { panic!("active") };
    row.count = u64::MAX;
    facts.reserve_attempt(&source(1)).unwrap();
    assert!(matches!(facts.accept_scramble(source(1), entity(2, Some(2)), Some(handle), 11), Err(Refusal::Overflow)));
    assert_eq!(segment(&facts, handle).last, 10);
    facts.finish_attempt(&source(1));
}

#[test]
fn full_pool_refuses_refresh_but_reserved_end_is_recorded() {
    let mut facts = ledger(2);
    let handle = accept(&mut facts, 0, None, 10);
    facts.reserve_attempt(&source(1)).unwrap();
    assert!(matches!(facts.accept_scramble(source(1), entity(2, Some(2)), Some(handle), 12), Err(Refusal::Full)));
    assert_eq!(segment(&facts, handle).count, 1);
    facts.finish_attempt(&source(1));
    facts.close_scramble(handle, 12).unwrap();
    let batch = facts.capture(12).unwrap().unwrap();
    let events = batch.events();
    let crate::lua::events::GameEvent::WarpScrambleApplied { slices, intervals, .. } = &events[0].event else { panic!("scramble") };
    assert_eq!(slices[0].successful_count, 1);
    assert_eq!(intervals[0].active_until, 12);
}

#[test]
fn affiliation_rebind_changes_only_continuation_metadata() {
    let mut facts = ledger(5);
    let handle = accept(&mut facts, 0, None, 10);
    let (_, ended) = facts.close_character_boundary(2, 11).unwrap();
    assert!(ended.is_empty());
    let (old_source, mut target) = facts.scramble_identity(handle).unwrap();
    target.corporation_id = Some(8);
    facts.rebind_scramble(handle, old_source, target, 11).unwrap();
    assert_eq!(facts.scramble_identity(handle).unwrap().1.corporation_id, Some(8));
    assert_eq!(segment(&facts, handle).count, 0);
    facts.close_scramble(handle, 12).unwrap();
    let batch = facts.capture(12).unwrap().unwrap();
    let events = batch.events();
    let targets: Vec<_> = events.iter().map(|event| match &event.event {
        crate::lua::events::GameEvent::WarpScrambleApplied { target, .. } => target.corporation_id,
        _ => panic!("scramble"),
    }).collect();
    assert_eq!(targets, vec![None, Some(8)]);
}

#[test]
fn worker_normalizes_max_cycle_count_without_float_rounding() {
    let mut facts = ledger(4);
    let handle = accept(&mut facts, 0, None, 10);
    let index = facts.scrambles.active[&handle];
    let Some(ScrambleSlot::Segment(row)) = facts.scrambles.slots[index].as_mut() else { panic!("active") };
    row.count = u64::MAX;
    facts.close_scramble(handle, 11).unwrap();
    let batch = facts.capture(11).unwrap().unwrap();
    let events = batch.events();
    let crate::lua::events::GameEvent::WarpScrambleApplied { slices, .. } = &events[0].event else { panic!("scramble") };
    assert_eq!(slices[0].successful_count, u64::MAX);
    assert_eq!(slices[0].amount.coefficient, u64::MAX.to_string());
    assert_eq!(slices[0].amount.exponent, 0);
}

#[test]
fn active_projection_keeps_scopes_until_actual_close() {
    let mut facts = ledger(4);
    facts.close_character_boundary(1, 10).unwrap();
    facts.close_character_boundary(2, 10).unwrap();
    let handle = accept(&mut facts, 0, None, 10);
    facts.retire_source(source(0).activation, 1).unwrap();
    assert_eq!(facts.prune_character_scopes(std::iter::empty()), 0);
    assert!(facts.export_character_scope(1).is_some());
    assert!(facts.export_character_scope(2).is_some());
    facts.close_scramble(handle, 11).unwrap();
    assert!(facts.scope_cleanup_needed());
    assert_eq!(facts.prune_character_scopes(std::iter::empty()), 2);
}

#[test]
fn zero_cycle_zero_duration_continuation_releases_key() {
    let mut facts = ledger(4);
    let handle = accept(&mut facts, 0, None, 10);
    assert!(facts.checkpoint_scrambles(20).unwrap().is_empty());
    facts.close_scramble(handle, 20).unwrap();
    assert_eq!(facts.key_count(), 1);
    let batch = facts.capture(20).unwrap().unwrap();
    assert_eq!(batch.events().len(), 1);
    assert!(facts.ack(&batch));
    assert_eq!(facts.key_count(), 0);
    assert!(!facts.is_dirty());
}
