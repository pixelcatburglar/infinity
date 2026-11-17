//! Actor-only proofs for loaded slots: exact quantities, pending empty slots,
//! last-round depletion, incarnation identity, finite permits and the
//! loaded-only failure pause.
use super::*;
use crate::services::combat::groups_health::{Condition, ShipStats};
use crate::services::fit::{Fit, Key};

fn seeded(quantity: i64, pending: bool) -> System {
    crate::tests::init_config();
    let fit = Fit::for_space_type(crate::sde::types::tests::combat_fixture(), 1, vec![]);
    let stats = ShipStats::from_fit(&fit, Key::Ship);
    let mut system = System::default();
    system.install_condition(7, Condition { now: stats.max, stats, shield_at: crate::client::filetime_now() }, fit);
    let membership = system.combat_membership_generation(7).unwrap();
    assert!(system.publish_loaded_absolute(7, membership, vec![
        LoadedSeed { module: 11, flag: 27, type_id: Some(9), quantity, pending },
        LoadedSeed { module: 12, flag: 28, type_id: None, quantity: 0, pending: true },
    ]));
    assert!(!system.publish_loaded_absolute(7, membership.wrapping_add(1), vec![]));
    system
}

fn slot(system: &System, module: i64) -> &LoadedSlot { &system.actor_combat[&7].loaded[&module] }

#[test]
fn exact_consume_marks_dirty_and_last_round_writes_zero() {
    let mut system = seeded(2, false);
    let permit = system.hold_loaded_use(7, 11).expect("charged use admitted");
    assert_eq!(system.consume_loaded(permit), Some((9, 1)));
    assert!(slot(&system, 11).dirty && slot(&system, 11).quantity == 1);
    let permit = system.hold_loaded_use(7, 11).expect("last round admitted");
    assert_eq!(system.consume_loaded(permit), Some((9, 0)));
    let writes = system.capture_dirty_loaded(&[]);
    assert_eq!(writes.len(), 1);
    assert_eq!((writes[0].flag, writes[0].quantity), (27, 0));
}

#[test]
fn pending_empty_slot_blocks_charged_use_not_empty_proof() {
    let mut system = seeded(4, false);
    assert!(system.hold_loaded_use(7, 12).is_none(), "pending slot refuses charged use");
    assert!(!system.loaded_empty_proof(7, 12));
    assert!(system.loaded_empty_proof(7, 13), "a slot-free module stays empty");
    assert!(system.hold_loaded_use(7, 11).is_some());
}

#[test]
fn stale_permit_cannot_debit_a_reloaded_slot() {
    let mut system = seeded(4, false);
    let stale = system.hold_loaded_use(7, 11).expect("use admitted");
    let membership = system.combat_membership_generation(7).unwrap();
    assert!(system.publish_loaded_absolute(7, membership, vec![
        LoadedSeed { module: 11, flag: 27, type_id: Some(9), quantity: 4, pending: false }]));
    assert_eq!(system.consume_loaded(stale), None);
    system.release_loaded_use(stale);
    assert!(system.hold_loaded_use(7, 11).is_some());
}

#[test]
fn held_slots_skip_capture_until_publish_replaces() {
    let mut system = seeded(4, false);
    let permit = system.hold_loaded_use(7, 11).expect("use admitted");
    assert_eq!(system.consume_loaded(permit), Some((9, 3)));
    system.actor_combat.get_mut(&7).unwrap().loaded.get_mut(&11).unwrap().held = true;
    system.actor_combat.get_mut(&7).unwrap().loaded.get_mut(&12).unwrap().held = true;
    assert!(system.capture_dirty_loaded(&[]).is_empty(), "held slots are not snapshotted");
    assert_eq!(system.capture_held_loaded(&[7]).len(), 1);
    let membership = system.combat_membership_generation(7).unwrap();
    assert!(system.publish_loaded_absolute(7, membership, vec![
        LoadedSeed { module: 11, flag: 27, type_id: Some(9), quantity: 3, pending: false }]));
    assert!(!slot(&system, 11).held && !slot(&system, 11).dirty, "a seeded slot is replaced clean");
    assert!(slot(&system, 12).held, "an untouched hold is never released by a publish");
}

#[test]
fn failure_periods_pause_only_loaded_use() {
    let mut system = seeded(4, false);
    let permit = system.hold_loaded_use(7, 11).expect("use admitted");
    assert_eq!(system.consume_loaded(permit), Some((9, 3)));
    for _ in 0..3 { system.record_loaded_period(); }
    assert!(system.loaded_use_paused());
    assert!(system.hold_loaded_use(7, 11).is_none(), "paused system refuses new use");
    assert!(system.loaded_empty_proof(7, 13), "non-loaded answers keep working");
    let membership = system.combat_membership_generation(7).unwrap();
    assert!(system.publish_loaded_absolute(7, membership, vec![
        LoadedSeed { module: 11, flag: 27, type_id: Some(9), quantity: 4, pending: false }]));
    assert!(!system.loaded_use_paused(), "a fully clean state resumes loaded use");
    assert!(system.hold_loaded_use(7, 11).is_some());
    let mut clean = seeded(4, false);
    clean.record_loaded_period();
    assert!(!clean.loaded_use_paused(), "a clean system never counts a period");
}
