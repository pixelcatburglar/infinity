use super::*;
use crate::lua::events::resources::FactOrigin;
use super::reservation::ReservedTarget;

fn entity(item_id: i64, character_id: Option<i64>) -> CombatEntity {
    CombatEntity { item_id, type_id: 1, incarnation: incarnation(7, item_id as u64), character_id,
        corporation_id: None, alliance_id: None, faction_id: None }
}
fn incarnation(runtime: u64, generation: u64) -> [u8; 16] {
    let mut result = [0; 16];
    result[..8].copy_from_slice(&runtime.to_be_bytes());
    result[8..].copy_from_slice(&generation.to_be_bytes());
    result
}
fn source(sequence: u64) -> CombatSource {
    CombatSource { actual_actor_character: Some(1), entity: entity(10, Some(1)),
        context: FactContext { corporation_id: None, alliance_id: None, faction_id: None,
            ship_id: Some(10), ship_type_id: Some(1), system_id: Some(1), station_id: None, origin: FactOrigin::Natural },
        module_id: Some(20), effect_id: Some(30), weapon_type_id: None, source_is_npc: false,
        controller_incarnation: Some(incarnation(7, 10)),
        activation: incarnation(7, 1), sequence }
}
fn ledger(capacity: usize) -> CombatFacts {
    let mut ledger = CombatFacts::new(7, 1, capacity, 4);
    ledger.register_source(source(0).activation, 1, 0).unwrap();
    ledger
}

#[test]
fn repeated_hits_merge_exactly_without_new_keys() {
    let mut facts = ledger(3);
    let target = entity(11, Some(2));
    for sequence in 0..100 {
        facts.reserve_attempt(&source(sequence)).unwrap();
        let mut reservation = facts.reserve_damage(source(sequence), &target, 10).unwrap();
        facts.commit_damage(&mut reservation, &target, [0.5, 0.0, 0.0], false).unwrap();
        facts.release(reservation);
    }
    assert_eq!(facts.key_count(), 1);
    let sum = &facts.slots.iter().flatten().next().unwrap().sum;
    assert_eq!(sum.occurrences(), 100);
    assert_eq!(sum.to_amount().coefficient, "25");
    assert_eq!(sum.to_amount().exponent, 1);
}

#[test]
fn damage_and_fanout_reserve_all_missing_keys_or_none() {
    let target = entity(11, Some(2));
    let other = entity(12, Some(3));
    let mut too_small = ledger(2);
    too_small.reserve_attempt(&source(0)).unwrap();
    assert!(matches!(too_small.reserve_damage(source(0), &target, 10), Err(Refusal::Full)));
    assert_eq!(too_small.key_count(), 0);
    let mut facts = ledger(5);
    facts.reserve_attempt(&source(0)).unwrap();
    let mut first = facts.reserve_damage(source(0), &target, 10).unwrap();
    facts.commit_damage(&mut first, &target, [1.0, 0.0, 0.0], false).unwrap();
    facts.release(first);
    facts.reserve_attempt(&source(1)).unwrap();
    let mut targets = [ReservedTarget::new(target, [1.0, 0.0, 0.0], ()), ReservedTarget::new(other, [1.0, 0.0, 0.0], ())];
    assert!(matches!(facts.reserve_damage_union(source(1), &mut targets, 10), Err(Refusal::Full)));
    assert_eq!(facts.key_count(), 1);
}

#[test]
fn fanout_preflight_refuses_invalid_victim_before_any_aggregate() {
    let mut facts = ledger(6);
    let first = entity(11, Some(2));
    let second = entity(12, Some(3));
    facts.reserve_attempt(&source(0)).unwrap();
    let mut targets = [ReservedTarget::new(first, [1.0, 0.0, 0.0], ()), ReservedTarget::new(second, [f64::NAN, 0.0, 0.0], ())];
    assert!(facts.reserve_damage_union(source(0), &mut targets, 10).is_err());
    assert!(facts.slots.iter().flatten().all(|slot| slot.sum.occurrences() == 0));
    assert_eq!(facts.key_count(), 0);
}
#[test]
fn duplicate_target_commit_cannot_partially_add_layers() {
    let mut facts = ledger(3);
    let target = entity(11, Some(2));
    facts.reserve_attempt(&source(0)).unwrap();
    let mut reservation = facts.reserve_damage(source(0), &target, 10).unwrap();
    facts.commit_damage(&mut reservation, &target, [1.0, 2.0, 0.0], false).unwrap();
    assert!(matches!(facts.commit_damage(&mut reservation, &target, [1.0, 2.0, 1.0], false), Err(Refusal::Replay)));
    facts.release(reservation);
    assert_eq!(facts.key_count(), 2);
    assert!(facts.slots.iter().flatten().all(|slot| slot.sum.occurrences() == 1));
}

#[test]
fn lethal_is_owned_and_rollback_keeps_prior_aggregate() {
    let mut facts = ledger(3);
    let target = entity(11, Some(2));
    facts.reserve_attempt(&source(0)).unwrap();
    let mut first = facts.reserve_damage(source(0), &target, 10).unwrap();
    facts.commit_damage(&mut first, &target, [2.0, 0.0, 0.0], false).unwrap();
    facts.release(first);
    facts.reserve_attempt(&source(1)).unwrap();
    let mut lethal = facts.reserve_damage(source(1), &target, 10).unwrap();
    let pending = facts.commit_damage(&mut lethal, &target, [3.0, 1.0, 0.0], true).unwrap().unwrap();
    assert_eq!(pending.amounts[0].to_amount().coefficient, "3");
    facts.release(lethal);
    let ordinary = facts.slots.iter().flatten().next().unwrap();
    assert_eq!(ordinary.sum.occurrences(), 1);
    assert_eq!(ordinary.sum.to_amount().coefficient, "1");
    assert_eq!(ordinary.sum.to_amount().exponent, 1);
}

#[test]
fn character_boundary_at_same_filetime_splits_epochs() {
    let mut facts = ledger(6);
    let target = entity(11, Some(2));
    facts.reserve_attempt(&source(0)).unwrap();
    let mut first = facts.reserve_damage(source(0), &target, 10).unwrap();
    facts.commit_damage(&mut first, &target, [1.0, 0.0, 0.0], false).unwrap();
    facts.release(first);
    let (boundary, ended) = facts.close_character_boundary(2, 10).unwrap();
    assert!(ended.is_empty());
    facts.reserve_attempt(&source(1)).unwrap();
    let mut second = facts.reserve_damage(source(1), &target, 10).unwrap();
    facts.commit_damage(&mut second, &target, [1.0, 0.0, 0.0], false).unwrap();
    facts.release(second);
    let rows: Vec<_> = facts.slots.iter().flatten().collect();
    assert_eq!(rows.len(), 2);
    assert_ne!(rows[0].key.epoch, rows[1].key.epoch);
    assert_eq!(boundary.at, 10);
}

#[test]
fn utc_gap_closes_once_at_actual_observation() {
    let mut boundaries = Boundaries::new();
    assert!(boundaries.observe_day(10).unwrap().is_none());
    let change = boundaries.observe_day(3 * infinity_core::consts::FILETIME_DAY + 1).unwrap().unwrap();
    assert_eq!(change.close_at, 3 * infinity_core::consts::FILETIME_DAY + 1);
    assert_eq!(change.current.at, change.close_at);
    assert!(boundaries.observe_day(10).is_err());
}

#[test]
fn same_key_reservations_keep_slot_until_both_release() {
    let mut facts = ledger(3);
    let target = entity(11, Some(2));
    facts.reserve_attempt(&source(0)).unwrap();
    facts.reserve_attempt(&source(1)).unwrap();
    let mut first = facts.reserve_damage(source(0), &target, 10).unwrap();
    let mut second = facts.reserve_damage(source(1), &target, 10).unwrap();
    facts.commit_damage(&mut first, &target, [1.0, 0.0, 0.0], false).unwrap();
    facts.release(first);
    assert_eq!(facts.key_count(), 3);
    facts.commit_damage(&mut second, &target, [2.0, 0.0, 0.0], false).unwrap();
    facts.release(second);
    assert_eq!(facts.key_count(), 1);
    assert_eq!(facts.slots.iter().flatten().next().unwrap().sum.to_amount().coefficient, "3");
}

#[test]
fn remote_hull_repair_is_accepted() {
    let mut facts = ledger(1);
    let target = entity(11, Some(2));
    facts.reserve_attempt(&source(0)).unwrap();
    let mut reservation = facts.reserve_remote_repair(source(0), &target, HpLayer::Hull, 10).unwrap();
    facts.commit_remote_repair(&mut reservation, &target, HpLayer::Hull, 1.0).unwrap();
    facts.release(reservation);
    assert_eq!(facts.key_count(), 1);
}

#[test]
fn singleton_union_rejects_invalid_amount_before_key_reservation() {
    let mut facts = ledger(3);
    let target = entity(11, Some(2));
    facts.reserve_attempt(&source(0)).unwrap();
    let mut targets = [ReservedTarget::new(target, [f64::INFINITY, 0.0, 0.0], ())];
    assert!(matches!(facts.reserve_damage_union(source(0), &mut targets, 10), Err(Refusal::Invalid)));
    assert_eq!(facts.key_count(), 0);
}

#[test]
fn fanout_duplicate_and_unsorted_targets_refuse_without_reservation() {
    let mut facts = ledger(6);
    let first = entity(11, Some(2));
    let second = entity(12, Some(3));
    facts.reserve_attempt(&source(0)).unwrap();
    let mut equal = [ReservedTarget::new(first, [1.0, 0.0, 0.0], ()), ReservedTarget::new(first, [1.0, 0.0, 0.0], ())];
    assert!(matches!(facts.reserve_damage_union(source(0), &mut equal, 10), Err(Refusal::Invalid)));
    let mut divergent = [ReservedTarget::new(first, [1.0, 0.0, 0.0], ()), ReservedTarget::new(first, [2.0, 0.0, 0.0], ())];
    assert!(matches!(facts.reserve_damage_union(source(0), &mut divergent, 10), Err(Refusal::Invalid)));
    let mut unsorted = [ReservedTarget::new(second, [1.0, 0.0, 0.0], ()), ReservedTarget::new(first, [1.0, 0.0, 0.0], ())];
    assert!(matches!(facts.reserve_damage_union(source(0), &mut unsorted, 10), Err(Refusal::Invalid)));
    assert_eq!(facts.key_count(), 0);
    facts.finish_attempt(&source(0));
}

#[test]
fn combined_scope_uses_latest_time_even_when_older_receipt_has_newer_epoch() {
    let mut boundaries = Boundaries::new();
    boundaries.observe_day(1).unwrap();
    let first = boundaries.close_character(1, 10).unwrap();
    let imported = boundaries.import_scope(2, boundaries::Boundary { epoch: 0, at: 2 }).unwrap();
    assert!(imported.epoch > first.epoch);
    let combined = boundaries.current(Some(1), Some(2));
    assert_eq!(combined.epoch, imported.epoch);
    assert_eq!(combined.at, 10);
    assert_eq!(boundaries.current(Some(2), Some(1)), combined);
    let same_time = boundaries.close_character(2, 10).unwrap();
    assert_eq!(boundaries.current(Some(1), Some(2)), same_time);
}

#[test]
fn departed_scope_is_pruned_at_checkpoint_without_live_source() {
    let mut facts = ledger(3);
    facts.close_character_boundary(2, 10).unwrap();
    facts.mark_character_scopes_dirty();
    assert!(facts.scope_cleanup_needed());
    assert_eq!(facts.prune_character_scopes(std::iter::empty()), 1);
    assert!(facts.export_character_scope(2).is_none());
    assert!(!facts.scope_cleanup_needed());
}

#[test]
fn transfer_copies_scope_until_retired_source_finishes() {
    let mut origin = ledger(3);
    let (old, _) = origin.close_character_boundary(1, 10).unwrap();
    origin.reserve_attempt(&source(0)).unwrap();
    origin.retire_source(source(0).activation, 1).unwrap();
    let copied = origin.export_character_scope(1).unwrap();
    assert_eq!(copied, old);
    let mut destination = ledger(3);
    destination.close_character_boundary(1, 20).unwrap();
    let imported = destination.import_character_scope(1, copied).unwrap();
    assert_eq!(imported.at, 20);
    assert_eq!(origin.prune_character_scopes(std::iter::empty()), 0);
    assert_eq!(origin.export_character_scope(1), Some(old));
    origin.finish_attempt(&source(0));
    assert!(origin.scope_cleanup_needed());
    assert_eq!(origin.prune_character_scopes(std::iter::empty()), 1);
    assert!(origin.export_character_scope(1).is_none());
}

#[test]
fn repeated_membership_churn_recycles_scope_receipts() {
    let mut facts = ledger(3);
    for character in 1..5000 {
        facts.close_character_boundary(character, 10).unwrap();
        facts.mark_character_scopes_dirty();
        assert_eq!(facts.prune_character_scopes(std::iter::empty()), 1);
        assert!(facts.export_character_scope(character).is_none());
    }
}

#[test]
fn source_scope_rejects_subject_rebinding_and_impact_spoof() {
    let mut facts = ledger(3);
    let original = source(0);
    facts.reserve_attempt(&original).unwrap();
    let mut other = source(1);
    other.actual_actor_character = Some(2);
    assert_eq!(facts.reserve_attempt(&other), Err(Refusal::Invalid));
    other.sequence = 0;
    assert!(matches!(facts.reserve_damage(other, &entity(11, Some(2)), 10), Err(Refusal::Invalid)));
    facts.finish_attempt(&original);
}

#[test]
fn configured_ledger_storage_layout_and_capacity() {
    use std::mem::size_of;
    use super::scramble::{ScrambleSegment, ScrambleSlot};
    let policy = crate::config::game::world::CombatConfig::default();
    let mut facts = CombatFacts::new(7, 1, policy.progress_key_capacity, policy.progress_pending_batches);
    let target = entity(11, Some(2));
    facts.register_source(source(0).activation, 1, 0).unwrap();
    facts.reserve_attempt(&source(0)).unwrap();
    let mut damage = facts.reserve_damage(source(0), &target, 10).unwrap();
    facts.commit_damage(&mut damage, &target, [1.0, 0.0, 0.0], false).unwrap();
    facts.release(damage);
    facts.reserve_attempt(&source(1)).unwrap();
    let handle = facts.accept_scramble(source(1), target, None, 10).unwrap();
    let keys = facts.key_count();
    println!("type bytes: Slot={} OptionSlot={} SourceKey={} CombatSource={} ExactBinarySum={} ScrambleSegment={} OptionScrambleSlot={}",
        size_of::<Slot>(), size_of::<Option<Slot>>(), size_of::<SourceKey>(), size_of::<CombatSource>(),
        size_of::<ExactBinarySum>(), size_of::<ScrambleSegment>(), size_of::<Option<ScrambleSlot>>());
    println!("vector reserved: hp_slots={}x{}={} hp_free={}x{}={} scramble_slots={}x{}={} scramble_free={}x{}={}",
        facts.slots.capacity(), size_of::<Option<Slot>>(), facts.slots.capacity() * size_of::<Option<Slot>>(),
        facts.free_slots.capacity(), size_of::<usize>(), facts.free_slots.capacity() * size_of::<usize>(),
        facts.scrambles.slots.capacity(), size_of::<Option<ScrambleSlot>>(), facts.scrambles.slots.capacity() * size_of::<Option<ScrambleSlot>>(),
        facts.scrambles.free.capacity(), size_of::<usize>(), facts.scrambles.free.capacity() * size_of::<usize>());
    println!("hash capacities only (allocator and bucket overhead excluded): hp_index={} scramble_active={} batches={}",
        facts.index.capacity(), facts.scrambles.active.capacity(), facts.batches.capacity());
    facts.close_scramble(handle, 10).unwrap();
    let after_close = facts.key_count();
    assert_eq!(keys, after_close + 1);
    let batch = facts.capture(10).unwrap().unwrap();
    assert_eq!(facts.key_count(), after_close);
    assert!(facts.ack(&batch));
    assert_eq!(facts.key_count(), 0);
}

#[test]
fn scope_errors_distinguish_clock_regression_and_epoch_overflow() {
    let mut facts = ledger(3);
    facts.close_character_boundary(1, 10).unwrap();
    assert!(matches!(facts.close_character_boundary(1, 9), Err(Refusal::Invalid)));
    let exhausted = boundaries::Boundary { epoch: u64::MAX, at: 10 };
    assert!(matches!(facts.import_character_scope(2, exhausted), Err(Refusal::Overflow)));
    assert!(facts.export_character_scope(2).is_none());
    facts.import_character_scope(2, boundaries::Boundary { epoch: u64::MAX - 1, at: 10 }).unwrap();
    assert!(matches!(facts.close_character_boundary(2, 10), Err(Refusal::Overflow)));
}
