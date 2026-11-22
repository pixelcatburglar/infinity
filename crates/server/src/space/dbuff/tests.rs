use std::sync::Arc;

use super::*;
use crate::sde::dbuff::DbuffAggregate;
use crate::services::fit::runtime::RuntimeProducerNamespace;
use crate::space::tests::destiny_reference::ship;
use crate::space::System;

fn source(id: i64) -> RuntimeSourceKey {
    RuntimeSourceKey { producer: RuntimeProducerNamespace::new(9), source_id: id, incarnation: 1 }
}

fn input(receiver: RuntimeReceiverKey, collection_id: i32, source: RuntimeSourceKey, value: f64, expires_at_ms: i64) -> TimedRuntimeInput {
    TimedRuntimeInput { receiver, collection_id, source, value, expires_at_ms }
}

#[test]
fn maximum_skyline_keeps_stronger_earlier_and_weaker_later_fallback() {
    let mut state = RuntimeTimedState::default();
    let (receiver, source) = (RuntimeReceiverKey::Ship(1), source(7));
    state.apply_timed(input(receiver, 5, source, 10.0, 1_000), DbuffAggregate::Maximum, 0).unwrap();
    state.apply_timed(input(receiver, 5, source, 4.0, 2_000), DbuffAggregate::Maximum, 0).unwrap();
    assert_eq!(state.source_snapshot(receiver, 5, source, 999).unwrap().value, 10.0);
    let changes = state.expire_before(receiver, 1_001);
    assert_eq!(changes.len(), 1);
    assert!(changes[0].changed && changes[0].next_expiry_ms == Some(2_000));
    assert_eq!(state.source_snapshot(receiver, 5, source, 1_001).unwrap().value, 4.0);
}

#[test]
fn minimum_skyline_keeps_lower_earlier_and_later_fallback() {
    let mut state = RuntimeTimedState::default();
    let (receiver, source) = (RuntimeReceiverKey::Ship(1), source(7));
    state.apply_timed(input(receiver, 5, source, 2.0, 1_000), DbuffAggregate::Minimum, 0).unwrap();
    state.apply_timed(input(receiver, 5, source, 8.0, 2_000), DbuffAggregate::Minimum, 0).unwrap();
    assert_eq!(state.source_snapshot(receiver, 5, source, 0).unwrap().value, 2.0);
    let changes = state.expire_before(receiver, 1_001);
    assert!(changes[0].changed);
    assert_eq!(state.source_snapshot(receiver, 5, source, 1_001).unwrap().value, 8.0);
}

#[test]
fn same_source_repeat_does_not_replace_stronger_or_later_value() {
    let mut state = RuntimeTimedState::default();
    let (receiver, source) = (RuntimeReceiverKey::Ship(1), source(7));
    state.apply_timed(input(receiver, 5, source, 5.0, 1_000), DbuffAggregate::Maximum, 0).unwrap();
    state.apply_timed(input(receiver, 5, source, 9.0, 1_000), DbuffAggregate::Maximum, 0).unwrap();
    state.apply_timed(input(receiver, 5, source, 1.0, 1_000), DbuffAggregate::Maximum, 0).unwrap();
    assert_eq!(state.source_snapshot(receiver, 5, source, 0).unwrap().value, 9.0);
    state.apply_timed(input(receiver, 5, source, 3.0, 2_000), DbuffAggregate::Maximum, 0).unwrap();
    assert_eq!(state.source_snapshot(receiver, 5, source, 0).unwrap().value, 9.0);
    state.expire_before(receiver, 1_001);
    assert_eq!(state.source_snapshot(receiver, 5, source, 1_001).unwrap().value, 3.0);
    assert_eq!(state.next_expiry(receiver), Some(2_000));
}

#[test]
fn equal_strength_prefers_later_expiry() {
    let mut state = RuntimeTimedState::default();
    let (receiver, source) = (RuntimeReceiverKey::Ship(1), source(7));
    state.apply_timed(input(receiver, 5, source, 7.0, 1_000), DbuffAggregate::Maximum, 0).unwrap();
    let change = state.apply_timed(input(receiver, 5, source, 7.0, 2_000), DbuffAggregate::Maximum, 0).unwrap();
    assert!(!change.changed && change.next_expiry_ms == Some(1_000));
    state.expire_before(receiver, 1_001);
    assert_eq!(state.source_snapshot(receiver, 5, source, 1_001).unwrap().value, 7.0);
    assert_eq!(state.next_expiry(receiver), Some(2_000));
}

#[test]
fn strict_boundary_expiry_is_half_open() {
    let mut state = RuntimeTimedState::default();
    let (receiver, source) = (RuntimeReceiverKey::Ship(1), source(7));
    state.apply_timed(input(receiver, 5, source, 6.0, 1_000), DbuffAggregate::Maximum, 0).unwrap();
    assert!(state.expire_before(receiver, 1_000).is_empty());
    assert!(state.source_snapshot(receiver, 5, source, 1_000).is_some());
    let changes = state.expire_before(receiver, 1_001);
    assert_eq!(changes.len(), 1);
    assert!(changes[0].changed && changes[0].next_expiry_ms.is_none());
    assert!(state.source_snapshot(receiver, 5, source, 1_001).is_none());
    assert_eq!(state.next_expiry(receiver), None);
}

#[test]
fn two_sources_remain_independent() {
    let mut state = RuntimeTimedState::default();
    let (receiver, a, b) = (RuntimeReceiverKey::Ship(1), source(7), source(8));
    state.apply_timed(input(receiver, 5, a, 10.0, 1_000), DbuffAggregate::Maximum, 0).unwrap();
    state.apply_timed(input(receiver, 5, b, 2.0, 2_000), DbuffAggregate::Maximum, 0).unwrap();
    let changes = state.expire_before(receiver, 1_001);
    assert!(changes.len() == 1 && changes[0].changed);
    assert!(state.source_snapshot(receiver, 5, a, 1_001).is_none());
    assert_eq!(state.source_snapshot(receiver, 5, b, 1_001).unwrap().value, 2.0);
    assert_eq!(state.next_expiry(receiver), Some(2_000));
}

#[test]
fn remove_receiver_clears_all_collections() {
    let mut state = RuntimeTimedState::default();
    let (first, second, a, b) = (RuntimeReceiverKey::Ship(1), RuntimeReceiverKey::Ship(2), source(7), source(8));
    for (receiver, collection_id, key) in [(first, 5, a), (first, 6, a), (first, 5, b), (second, 5, a)] {
        state.apply_timed(input(receiver, collection_id, key, 1.0, 1_000), DbuffAggregate::Maximum, 0).unwrap();
    }
    let changes = state.remove_receiver(first);
    assert!(changes.len() == 3 && changes.iter().all(|c| c.changed && c.next_expiry_ms.is_none()));
    assert!(state.source_snapshot(first, 5, a, 0).is_none()
        && state.source_snapshot(first, 6, a, 0).is_none()
        && state.source_snapshot(first, 5, b, 0).is_none());
    assert!(state.source_snapshot(second, 5, a, 0).is_some());
    assert_eq!(state.next_expiry(first), None);
    assert_eq!(state.next_expiry(second), Some(1_000));
}

#[test]
fn nonfinite_value_and_past_deadline_are_refused_without_live_state() {
    let mut state = RuntimeTimedState::default();
    let (receiver, source) = (RuntimeReceiverKey::Ship(1), source(7));
    for value in [f64::NAN, f64::INFINITY] {
        assert!(state.apply_timed(input(receiver, 5, source, value, 1_000), DbuffAggregate::Maximum, 0).is_err());
    }
    let change = state.apply_timed(input(receiver, 5, source, 5.0, 500), DbuffAggregate::Maximum, 1_000).unwrap();
    assert!(!change.changed && change.next_expiry_ms.is_none());
    assert!(state.source_snapshot(receiver, 5, source, 1_000).is_none());
    assert_eq!(state.next_expiry(receiver), None);
    assert!(state.receivers.is_empty());
}

#[test]
fn receiver_cleanup_removes_ship_module_character_and_drone_keys() {
    let mut state = RuntimeTimedState::default();
    let source = source(7);
    let anchored = [
        RuntimeReceiverKey::Ship(9),
        RuntimeReceiverKey::Module { ship: 9, module: 1 },
        RuntimeReceiverKey::Character(70),
        RuntimeReceiverKey::Drone { ship: 9, drone: 5 },
        RuntimeReceiverKey::Drone { ship: 2, drone: 9 },
    ];
    for receiver in anchored.into_iter().chain([RuntimeReceiverKey::Ship(2)]) {
        state.apply_timed(input(receiver, 5, source, 1.0, 1_000), DbuffAggregate::Maximum, 0).unwrap();
        state.arm(receiver, 1_000);
    }
    state.remove_for_ball(9, Some(70));
    assert!(anchored.iter().all(|r| state.next_expiry(*r).is_none() && state.armed(*r).is_none()));
    assert_eq!(state.next_expiry(RuntimeReceiverKey::Ship(2)), Some(1_000));
    assert!(state.armed(RuntimeReceiverKey::Ship(2)).is_some());
}

#[test]
fn timer_generation_changes_on_rearm_and_receiver_removal() {
    let mut state = RuntimeTimedState::default();
    let receiver = RuntimeReceiverKey::Ship(1);
    state.apply_timed(input(receiver, 5, source(7), 1.0, 1_000), DbuffAggregate::Maximum, 0).unwrap();
    let first = state.arm(receiver, 1_000);
    assert_eq!(state.armed(receiver), Some((first, 1_000)));
    assert!(state.arm(receiver, 1_000) > first);
    state.remove_receiver(receiver);
    assert_eq!(state.armed(receiver), None);
    state.apply_timed(input(receiver, 5, source(7), 1.0, 1_000), DbuffAggregate::Maximum, 0).unwrap();
    assert!(state.arm(receiver, 1_000) > first);
}

#[test]
fn strict_timer_uses_expiry_plus_one() {
    let mut system = System::new(1, Arc::default(), 5_000);
    system.insert(ship(1, [0.0; 3], [0.0; 3]));
    let (receiver, source) = (RuntimeReceiverKey::Ship(1), source(7));
    system.runtime_inputs.apply_timed(input(receiver, 5, source, 2.0, 5_000), DbuffAggregate::Maximum, 5_000).unwrap();
    system.arm_runtime_timer(receiver);
    let (generation, deadline) = system.runtime_inputs.armed(receiver).unwrap();
    assert_eq!(deadline, 5_000);
    let token = system.runtime_receiver_token(receiver).unwrap();
    system.expire_runtime_timer(receiver, token, generation, deadline);
    assert!(system.runtime_source_snapshot(receiver, 5, source).is_some());
    system.ms = 5_001;
    system.expire_runtime_timer(receiver, token, generation, deadline);
    assert!(system.runtime_source_snapshot(receiver, 5, source).is_none());
    assert_eq!(system.runtime_inputs.armed(receiver), None);
}

#[test]
fn same_id_replacement_has_new_membership_fence() {
    let mut system = System::new(1, Arc::default(), 0);
    let (receiver, source) = (RuntimeReceiverKey::Ship(9), source(7));
    system.insert(ship(9, [0.0; 3], [0.0; 3]));
    let first = system.runtime_receiver_token(receiver).unwrap();
    assert!(system.ball_token_current(first));
    assert!(system.take_for_scene_exit(9).is_some());
    system.insert(ship(9, [0.0; 3], [0.0; 3]));
    let second = system.runtime_receiver_token(receiver).unwrap();
    assert!(system.ball_token_current(second) && !system.ball_token_current(first));
    system.runtime_inputs.apply_timed(input(receiver, 5, source, 2.0, 5_000), DbuffAggregate::Maximum, 0).unwrap();
    system.arm_runtime_timer(receiver);
    system.expire_runtime_timer(receiver, first, 0, 5_000);
    assert!(system.runtime_source_snapshot(receiver, 5, source).is_some());
}
