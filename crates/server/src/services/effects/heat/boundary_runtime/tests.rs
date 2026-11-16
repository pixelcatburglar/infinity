//! Actor-rack regressions for unresolved nominal boundaries.
//! Heat values come from the production client curve and the production
//! boundary application. Inputs are fixed dyadic rates so each elapsed
//! interval is exact in milliseconds.

use std::sync::{Arc, atomic::AtomicBool};

use crate::services::effects::lifecycle::cycle_deadline;

use super::super::rack::curve;
use super::*;

const CAPACITY: f64 = 100.0;
const RATE: f64 = 0.5;
const DAMAGE: f64 = 4.0;
const START: i64 = 1_000;
const DURATION_MS: f64 = 1_000.0;

fn pending(flag: i32, rate: f64) -> PendingStart {
    PendingStart { flag, rate, damage: DAMAGE, capacity: CAPACITY, dissipation: 0.0, attenuation: 1.0, generation_multiplier: 1.0 }
}

fn rack() -> super::super::Rack {
    super::super::Rack { last: START, ..super::super::Rack::default() }
}

fn reference(elapsed_ms: f64) -> f64 {
    curve(0.0, CAPACITY, RATE, 1.0, 0.0, elapsed_ms)
}

/// Model a finished connection: the end stays unready until its waiter exists.
fn connect(rack: &mut super::super::Rack, module: i64, generation: &Arc<AtomicBool>, at: i64, flag: i32, rate: f64) -> oneshot::Receiver<Option<Vec<super::super::SourceSnapshot>>> {
    install_start(rack, module, generation.clone(), at);
    assert!(arm_start(rack, module, generation, at, pending(flag, rate)));
    assert!(install_end(rack, module, generation.clone(), at, cycle_deadline(at, DURATION_MS)));
    let receiver = register_waiter(rack, module, generation.clone(), at, vec![module], false);
    mark_ends_ready(rack, &[module], generation, at);
    receiver
}

fn drive(rack: &mut super::super::Rack, time: i64) {
    super::super::apply_rack(rack, time, |_| false);
}

#[test]
fn delayed_connection_matches_immediate_nominal_heat() {
    let generation = Arc::new(AtomicBool::new(false));
    let mut immediate = rack();
    let mut immediate_receiver = connect(&mut immediate, 11, &generation, START, 27, RATE);
    for tick in 1..=14 { drive(&mut immediate, START + tick * 100); }
    let end = cycle_deadline(START, DURATION_MS);
    let expected = reference(DURATION_MS);
    assert_eq!(immediate.heat.to_bits(), expected.to_bits());
    assert_eq!(immediate.last, end);
    let captured = immediate_receiver.try_recv().expect("immediate end captured").expect("snapshot");
    assert_eq!(captured[0].heat.to_bits(), expected.to_bits());
    release_completed(&mut immediate, &[11], &generation, START);
    drive(&mut immediate, START + 1_400);
    assert_eq!(immediate.last, START + 1_400);

    let mut delayed = rack();
    install_start(&mut delayed, 11, generation.clone(), START);
    for tick in 1..=4 { drive(&mut delayed, START + tick * 100); }
    assert_eq!(delayed.last, START);
    assert_eq!(delayed.heat, 0.0);
    assert!(arm_start(&mut delayed, 11, &generation, START, pending(27, RATE)));
    assert!(install_end(&mut delayed, 11, generation.clone(), START, end));
    let mut delayed_receiver = register_waiter(&mut delayed, 11, generation.clone(), START, vec![11], false);
    mark_ends_ready(&mut delayed, &[11], &generation, START);
    for tick in 5..=14 { drive(&mut delayed, START + tick * 100); }
    assert_eq!(delayed.heat.to_bits(), expected.to_bits());
    assert_eq!(delayed.last, end);
    let captured = delayed_receiver.try_recv().expect("delayed end captured").expect("snapshot");
    assert_eq!(captured[0].heat.to_bits(), expected.to_bits());
    release_completed(&mut delayed, &[11], &generation, START);
    drive(&mut delayed, START + 1_400);
    assert_eq!(delayed.heat.to_bits(), immediate.heat.to_bits());
    assert_eq!(delayed.last, immediate.last);
}

#[test]
fn reset_crossing_delayed_boundaries_preserves_waiters_and_next_cycle() {
    let mut epoch_rack = super::super::Rack::default();
    super::super::reset_rack(&mut epoch_rack, 0);
    super::super::reanchor(&mut epoch_rack, -1);
    assert_eq!(epoch_rack.anchor_at, 0);
    assert_eq!(super::super::floor_time(&epoch_rack, -1), 0);
    let generation = Arc::new(AtomicBool::new(false));
    let mut rack = rack();
    install_start(&mut rack, 11, generation.clone(), START);
    let mut later = connect(&mut rack, 22, &generation, START + 500, 28, RATE);
    drive(&mut rack, START + 2_000);
    assert!(later.try_recv().is_err());
    super::super::reset_rack(&mut rack, START + 2_000);
    assert!(arm_start(&mut rack, 11, &generation, START, pending(27, RATE)));
    assert!(install_end(&mut rack, 11, generation.clone(), START, START + 1_000));
    let mut older = register_waiter(&mut rack, 11, generation.clone(), START, vec![11], false);
    mark_ends_ready(&mut rack, &[11], &generation, START);
    drive(&mut rack, START + 2_000);
    let old = older.try_recv().unwrap().unwrap();
    assert_eq!(old[0].heat, 0.0);
    assert_eq!(old[0].sampled_at, START + 1_000);
    assert_eq!(rack.anchor_at, START + 2_000);
    assert!(later.try_recv().is_err());
    assert!(Arc::ptr_eq(&rack.sources[&11].generation, &generation));
    release_completed(&mut rack, &[11], &generation, START);
    drive(&mut rack, START + 2_000);
    assert_eq!(later.try_recv().unwrap().unwrap()[0].heat, 0.0);
    release_completed(&mut rack, &[22], &generation, START + 500);
    let mut next = connect(&mut rack, 11, &generation, START + 2_000, 27, RATE);
    drive(&mut rack, START + 3_000);
    assert!(next.try_recv().unwrap().unwrap()[0].heat > 0.0);
    release_completed(&mut rack, &[11], &generation, START + 2_000);
    let mut stopped = connect(&mut rack, 11, &generation, START + 3_000, 27, RATE);
    super::super::apply_rack(&mut rack, START + 4_000, |module| module == 11);
    assert!(stopped.try_recv().unwrap().is_some());
    assert!(rack.sources[&11].stopping && rack.sources[&11].suspended);
}

#[test]
fn refused_start_releases_the_rack() {
    let generation = Arc::new(AtomicBool::new(false));
    let mut rack = rack();
    install_start(&mut rack, 11, generation.clone(), START);
    drive(&mut rack, START + 400);
    assert_eq!(rack.last, START);
    assert!(!install_end(&mut rack, 11, generation, START, cycle_deadline(START, DURATION_MS)));
    drive(&mut rack, START + 400);
    assert_eq!(rack.last, START + 400);
    assert_eq!(rack.heat, 0.0);
    assert!(!blocking(&rack));
}

#[test]
fn interleaved_starts_resolve_by_timestamp_then_module() {
    let older = Arc::new(AtomicBool::new(false));
    let newer = Arc::new(AtomicBool::new(false));
    let mut rack = rack();
    install_start(&mut rack, 11, older.clone(), START);
    install_start(&mut rack, 22, newer.clone(), START + 500);
    assert!(arm_start(&mut rack, 22, &newer, START + 500, pending(28, RATE)));
    assert!(install_end(&mut rack, 22, newer.clone(), START + 500, cycle_deadline(START + 500, DURATION_MS)));
    let mut newer_receiver = register_waiter(&mut rack, 22, newer.clone(), START + 500, vec![22], false);
    mark_ends_ready(&mut rack, &[22], &newer, START + 500);
    drive(&mut rack, START + 700);
    assert_eq!(rack.last, START);
    assert!(newer_receiver.try_recv().is_err());
    assert!(arm_start(&mut rack, 11, &older, START, pending(27, RATE)));
    assert!(install_end(&mut rack, 11, older.clone(), START, cycle_deadline(START, DURATION_MS)));
    let mut older_receiver = register_waiter(&mut rack, 11, older.clone(), START, vec![11], false);
    mark_ends_ready(&mut rack, &[11], &older, START);
    drive(&mut rack, START + 1_600);
    let end = cycle_deadline(START, DURATION_MS);
    let midpoint = reference(500.0);
    let joined = curve(midpoint, CAPACITY, RATE * 2.0, 1.0, 0.0, 500.0);
    assert_eq!(rack.heat.to_bits(), joined.to_bits());
    assert_eq!(rack.last, end);
    let older_heat = older_receiver.try_recv().expect("oldest end captured").expect("snapshot")[0].heat;
    assert_eq!(older_heat.to_bits(), joined.to_bits());
    assert!(newer_receiver.try_recv().is_err(), "newer end stays pending behind the older held end");
    release_completed(&mut rack, &[11], &older, START);
    drive(&mut rack, START + 1_600);
    let finished = curve(midpoint, CAPACITY, RATE * 2.0, 1.0, 0.0, DURATION_MS);
    let newer_end = cycle_deadline(START + 500, DURATION_MS);
    assert_eq!(rack.heat.to_bits(), finished.to_bits());
    assert_eq!(rack.last, newer_end);
    let newer_heat = newer_receiver.try_recv().expect("newer end captured after release").expect("snapshot")[0].heat;
    assert_eq!(newer_heat.to_bits(), finished.to_bits());
    release_completed(&mut rack, &[22], &newer, START + 500);
    drive(&mut rack, START + 1_600);
    assert_eq!(rack.last, START + 1_600);
    assert_eq!(rack.heat.to_bits(), curve(midpoint, CAPACITY, RATE * 2.0, 1.0, 0.0, DURATION_MS + 100.0).to_bits());
}

#[test]
fn end_capture_waits_and_cleanup_wakes_the_receiver() {
    let older = Arc::new(AtomicBool::new(false));
    let current = Arc::new(AtomicBool::new(false));
    let mut rack = rack();
    install_start(&mut rack, 11, older.clone(), START);
    let mut receiver = connect(&mut rack, 22, &current, START + 500, 28, RATE);
    drive(&mut rack, START + 2_000);
    assert!(receiver.try_recv().is_err());
    assert_eq!(rack.last, START);
    assert_eq!(release_generation(&mut rack, 11, &older), 1);
    drive(&mut rack, START + 2_000);
    let snapshots = receiver.try_recv().expect("released marker wakes the receiver").expect("captured end");
    assert_eq!(snapshots[0].module, 22);
    assert_eq!(snapshots[0].heat_damage, DAMAGE);
    let end = cycle_deadline(START + 500, DURATION_MS);
    assert_eq!(rack.last, end);
    release_completed(&mut rack, &[22], &current, START + 500);
    drive(&mut rack, START + 2_000);
    assert_eq!(rack.last, START + 2_000);
}

#[test]
fn repeat_handoff_holds_heat_until_the_next_start() {
    let generation = Arc::new(AtomicBool::new(false));
    let mut rack = rack();
    let mut first = connect(&mut rack, 11, &generation, START, 27, RATE);
    let end = cycle_deadline(START, DURATION_MS);
    drive(&mut rack, end + 300);
    let held = rack.heat;
    assert_eq!(rack.last, end);
    assert_eq!(held.to_bits(), reference(DURATION_MS).to_bits());
    assert_eq!(first.try_recv().expect("first end captured").expect("snapshot")[0].heat.to_bits(), held.to_bits());
    release_completed(&mut rack, &[11], &generation, START);
    install_start(&mut rack, 11, generation.clone(), end);
    drive(&mut rack, end + 300);
    assert_eq!(rack.last, end);
    assert_eq!(rack.heat.to_bits(), held.to_bits());
    assert!(arm_start(&mut rack, 11, &generation, end, pending(27, RATE)));
    assert!(install_end(&mut rack, 11, generation.clone(), end, cycle_deadline(end, DURATION_MS)));
    let mut second = register_waiter(&mut rack, 11, generation.clone(), end, vec![11], false);
    mark_ends_ready(&mut rack, &[11], &generation, end);
    drive(&mut rack, end + 300);
    assert_eq!(rack.heat.to_bits(), curve(held, CAPACITY, RATE, 1.0, 0.0, 300.0).to_bits());
    assert_eq!(rack.last, end + 300);
    assert!(second.try_recv().is_err(), "the repeat end is still in the future");
}

#[test]
fn tick_before_end_does_not_capture_future_heat() {
    let generation = Arc::new(AtomicBool::new(false));
    let mut rack = rack();
    install_start(&mut rack, 11, generation.clone(), START);
    assert!(arm_start(&mut rack, 11, &generation, START, pending(27, RATE)));
    let end = cycle_deadline(START, DURATION_MS);
    assert!(install_end(&mut rack, 11, generation.clone(), START, end));
    drive(&mut rack, 0);
    assert_eq!(rack.last, START);
    assert_eq!(rack.heat, 0.0);
    drive(&mut rack, end - 1);
    assert_eq!(rack.heat.to_bits(), reference((end - 1 - START) as f64).to_bits());
    assert_eq!(rack.last, end - 1);
    assert!(rack.sources.contains_key(&11));
    let mut receiver = register_waiter(&mut rack, 11, generation.clone(), START, vec![11], false);
    mark_ends_ready(&mut rack, &[11], &generation, START);
    drive(&mut rack, end - 1);
    assert!(receiver.try_recv().is_err());
    assert_eq!(rack.last, end - 1);
    drive(&mut rack, end + 400);
    let snapshots = receiver.try_recv().expect("due end resolves once").expect("captured");
    assert_eq!(snapshots.len(), 1);
    assert_eq!(snapshots[0].heat.to_bits(), reference(DURATION_MS).to_bits());
    assert_eq!(rack.last, end);
    drive(&mut rack, end + 400);
    assert_eq!(rack.last, end);
    assert!(matches!(receiver.try_recv(), Err(oneshot::error::TryRecvError::Closed)));
}

#[test]
fn unheated_cycle_finishes_empty_without_a_barrier() {
    let generation = Arc::new(AtomicBool::new(false));
    let mut rack = rack();
    let mut receiver = register_waiter(&mut rack, 11, generation, START, Vec::new(), false);
    let snapshots = receiver.try_recv().expect("empty cycle resolves now").expect("empty snapshots");
    assert!(snapshots.is_empty());
    assert!(matches!(receiver.try_recv(), Err(oneshot::error::TryRecvError::Closed)));
    drive(&mut rack, START + 500);
    assert_eq!(rack.last, START + 500);
    assert!(!blocking(&rack));
}

#[test]
fn independent_same_rack_ends_do_not_cancel() {
    let first = Arc::new(AtomicBool::new(false));
    let second = Arc::new(AtomicBool::new(false));
    let mut rack = rack();
    let mut first_receiver = connect(&mut rack, 11, &first, START, 27, RATE);
    let mut second_receiver = connect(&mut rack, 22, &second, START + 200, 28, RATE);
    let first_end = cycle_deadline(START, DURATION_MS);
    let second_end = cycle_deadline(START + 200, DURATION_MS);
    drive(&mut rack, second_end + 500);
    let first_snapshots = first_receiver.try_recv().expect("first receiver").expect("first capture");
    assert!(second_receiver.try_recv().is_err(), "second end stays pending behind the older held end");
    assert_eq!(first_snapshots[0].module, 11);
    assert_eq!(rack.last, first_end);
    release_completed(&mut rack, &[11], &first, START);
    drive(&mut rack, second_end + 500);
    let second_snapshots = second_receiver.try_recv().expect("second receiver after older release").expect("second capture");
    assert_eq!(second_snapshots[0].module, 22);
    assert_ne!(first_snapshots[0].heat.to_bits(), second_snapshots[0].heat.to_bits());
    assert_eq!(rack.last, second_end);
    assert!(matches!(first_receiver.try_recv(), Err(oneshot::error::TryRecvError::Closed)));
    assert!(matches!(second_receiver.try_recv(), Err(oneshot::error::TryRecvError::Closed)));
    release_completed(&mut rack, &[22], &second, START + 200);
    drive(&mut rack, second_end + 500);
    assert_eq!(rack.last, second_end + 500);
}

#[test]
fn continuing_completion_does_not_cool_before_the_next_start() {
    let generation = Arc::new(AtomicBool::new(false));
    let mut rack = rack();
    let mut receiver = connect(&mut rack, 11, &generation, START, 27, RATE);
    let end = cycle_deadline(START, DURATION_MS);
    let now = end + 500;
    drive(&mut rack, now);
    let held = receiver.try_recv().expect("end captured").expect("snapshot")[0].heat;
    assert_eq!(rack.last, end);
    assert_eq!(held.to_bits(), reference(DURATION_MS).to_bits());
    release_completed(&mut rack, &[11], &generation, START);
    install_start(&mut rack, 11, generation.clone(), end);
    assert!(rack.sources.contains_key(&11));
    assert_eq!(rack.incoming, RATE);
    drive(&mut rack, now);
    assert_eq!(rack.last, end);
    assert_eq!(rack.heat.to_bits(), held.to_bits());
    assert!(arm_start(&mut rack, 11, &generation, end, pending(27, RATE)));
    drive(&mut rack, now);
    assert_eq!(rack.last, now);
    assert_eq!(rack.heat.to_bits(), reference(DURATION_MS + 500.0).to_bits());
}

#[test]
fn final_completion_cools_from_the_nominal_end() {
    let generation = Arc::new(AtomicBool::new(false));
    let mut rack = rack();
    install_start(&mut rack, 11, generation.clone(), START);
    assert!(arm_start(&mut rack, 11, &generation, START, pending(27, RATE)));
    let end = cycle_deadline(START, DURATION_MS);
    assert!(install_end(&mut rack, 11, generation.clone(), START, end));
    let mut receiver = register_waiter(&mut rack, 11, generation.clone(), START, vec![11], true);
    mark_ends_ready(&mut rack, &[11], &generation, START);
    let now = end + 500;
    drive(&mut rack, now);
    let held = receiver.try_recv().expect("final end captured").expect("snapshot")[0].heat;
    assert_eq!(rack.last, end);
    assert!(rack.sources[&11].suspended);
    assert_eq!(rack.incoming, 0.0);
    release_completed(&mut rack, &[11], &generation, START);
    drive(&mut rack, now);
    assert_eq!(rack.last, now);
    assert_eq!(rack.incoming, 0.0);
    assert_eq!(rack.heat.to_bits(), curve(held, CAPACITY, 0.0, 1.0, 0.0, 500.0).to_bits());
}

fn cool_pending(flag: i32) -> PendingStart {
    PendingStart { flag, rate: 0.0, damage: DAMAGE, capacity: CAPACITY, dissipation: 0.01, attenuation: 1.0, generation_multiplier: 1.0 }
}

fn arm_only(rack: &mut super::super::Rack, module: i64, generation: &Arc<AtomicBool>, at: i64, pending: PendingStart) {
    install_start(rack, module, generation.clone(), at);
    assert!(arm_start(rack, module, generation, at, pending));
}

#[test]
fn generation_cadence_matches_one_direct_evaluation() {
    let generation = Arc::new(AtomicBool::new(false));
    let mut stepped = rack();
    arm_only(&mut stepped, 11, &generation, START, pending(27, RATE));
    for tick in 1..=14 { drive(&mut stepped, START + tick * 100); }
    let mut once = rack();
    arm_only(&mut once, 11, &generation, START, pending(27, RATE));
    drive(&mut once, START + 1_400);
    let direct = curve(0.0, CAPACITY, RATE, 1.0, 0.0, 1_400.0);
    assert_eq!(stepped.heat.to_bits(), direct.to_bits());
    assert_eq!(once.heat.to_bits(), direct.to_bits());
    assert_eq!(stepped.last, once.last);
}

#[test]
fn cooling_cadence_matches_one_direct_evaluation() {
    let generation = Arc::new(AtomicBool::new(false));
    let mut heated = rack();
    arm_only(&mut heated, 11, &generation, START, cool_pending(27));
    for tick in 1..=4 { drive(&mut heated, START + tick * 100); }
    let mut once = rack();
    arm_only(&mut once, 11, &generation, START, cool_pending(27));
    drive(&mut once, START + 400);
    assert_eq!(heated.heat, 0.0);
    assert_eq!(once.heat, 0.0);
    assert_eq!(heated.last, once.last);
    arm_only(&mut heated, 22, &generation, START + 400, pending(28, 0.01));
    for tick in 5..=8 { drive(&mut heated, START + tick * 100); }
    arm_only(&mut once, 22, &generation, START + 400, pending(28, 0.01));
    drive(&mut once, START + 800);
    let direct = curve(0.0, CAPACITY, 0.01, 1.0, 0.01, 400.0);
    assert_eq!(heated.heat.to_bits(), direct.to_bits());
    assert_eq!(once.heat.to_bits(), direct.to_bits());
    assert!(direct > 0.0);
}

#[test]
fn input_change_reanchors_and_later_cadence_stays_exact() {
    let first = Arc::new(AtomicBool::new(false));
    let second = Arc::new(AtomicBool::new(false));
    let mut stepped = rack();
    arm_only(&mut stepped, 11, &first, START, pending(27, RATE));
    let join = START + 500;
    arm_only(&mut stepped, 22, &second, join, pending(28, RATE));
    drive(&mut stepped, join);
    let at_join = curve(0.0, CAPACITY, RATE, 1.0, 0.0, 500.0);
    assert_eq!(stepped.anchor_at, join);
    assert_eq!(stepped.anchor_heat.to_bits(), at_join.to_bits());
    for tick in 1..=5 { drive(&mut stepped, join + tick * 100); }
    let mut once = rack();
    arm_only(&mut once, 11, &first, START, pending(27, RATE));
    arm_only(&mut once, 22, &second, join, pending(28, RATE));
    drive(&mut once, join + 500);
    let direct = curve(at_join, CAPACITY, RATE * 2.0, 1.0, 0.0, 500.0);
    assert_eq!(stepped.heat.to_bits(), direct.to_bits());
    assert_eq!(once.heat.to_bits(), direct.to_bits());
}

#[test]
fn unchanged_source_refresh_keeps_the_current_anchor() {
    let generation = Arc::new(AtomicBool::new(false));
    let mut rack = rack();
    arm_only(&mut rack, 11, &generation, START, pending(27, RATE));
    drive(&mut rack, START + 400);
    assert_eq!(rack.anchor_at, START);
    assert_eq!(rack.anchor_heat, 0.0);
    arm_only(&mut rack, 11, &generation, START + 400, pending(27, RATE));
    assert_eq!(rack.anchor_at, START);
    assert_eq!(rack.anchor_heat, 0.0);
    drive(&mut rack, START + 900);
    assert_eq!(rack.heat.to_bits(), curve(0.0, CAPACITY, RATE, 1.0, 0.0, 900.0).to_bits());
}

#[test]
fn cooling_half_below_half_and_zero_stay_exact() {
    assert_eq!(curve(0.5, CAPACITY, 0.0, 1.0, 0.0, 1_000.0), 0.5);
    let below = f64::from_bits(0.5_f64.to_bits() - 1);
    assert_eq!(curve(below, CAPACITY, 0.0, 1.0, 0.0, 1_000.0), 0.0);
    let generation = Arc::new(AtomicBool::new(false));
    let mut rack = rack();

    arm_only(&mut rack, 11, &generation, START, cool_pending(27));
    drive(&mut rack, START + 1_000);
    assert_eq!(rack.heat, 0.0);
    drive(&mut rack, START + 2_000);
    assert_eq!(rack.heat, 0.0);
}

