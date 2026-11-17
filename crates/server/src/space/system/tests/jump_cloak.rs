use super::*;

fn uncloak_fx(stamp: i64, ship: i64) -> Value {
    event(stamp, "OnSpecialFX", vec![
        Value::Int(ship), Value::None, Value::None, Value::None, Value::None,
        Value::str("effects.Uncloak"), Value::Bool(false), Value::Int(1), Value::Int(0),
        Value::Int(SPECIAL_FX_UNSPECIFIED_DURATION), Value::Int(0), Value::None, Value::Int(0), Value::None,
    ])
}

#[test]
fn explicit_drop_batches_exact_wire_and_suppresses_duplicates() {
    let registry = Arc::new(Pilots::default());
    let (tx, mut notifications) = mpsc::unbounded_channel();
    registry.register_pilot(100, tx);
    let mut s = System::new(0, registry, 5_200);
    let mut hidden = ball(1, Some(100), 0.0);
    hidden.cloak = CLOAK_RESTORE;
    s.insert(hidden);
    s.insert(ball(2, None, 3000.0));
    let mut own = watch(&mut s, 1, 1);
    let mut observer = watch(&mut s, 2, 2);
    assert!(s.drop_jump_cloak(1));
    assert!(!s.drop_jump_cloak(1));
    assert_eq!(notifications.try_recv().unwrap(), ("OnJumpCloakCancelled".to_string(), vec![Value::Int(1)]));
    assert!(notifications.try_recv().is_err());
    let before = crate::client::filetime_now();
    s.tick(6_000);
    let after = crate::client::filetime_now();
    let actual = super::events::destiny(observer.try_recv().unwrap());
    let mut expected_add = add_balls_event(5, s.ball(1).unwrap(), true);
    assert_eq!(super::events::destiny(own.try_recv().unwrap()), vec![event(5, "UncloakBall", vec![Value::Int(1)]), uncloak_fx(5, 1)]);
    super::events::reuse_checked_damage_time(&actual[0], &mut expected_add, before..=after);
    assert_eq!(actual, vec![expected_add, uncloak_fx(5, 1)]);
    super::events::fleet_snapshot(&mut observer);
    assert!(own.try_recv().is_err());
    assert!(observer.try_recv().is_err());
    assert!(!s.drop_jump_cloak(1));
    assert!(notifications.try_recv().is_err());
}

#[test]
fn immediate_uncloak_sends_one_batch_per_observer() {
    let mut s = System::new(0, Arc::default(), 5_200);
    let mut hidden = ball(1, Some(100), 0.0);
    hidden.cloak = CLOAK_RESTORE;
    s.insert(ball(2, None, 1.0));
    s.insert(hidden);
    let mut own = watch(&mut s, 1, 1);
    let mut observer = watch(&mut s, 2, 2);
    let before = crate::client::filetime_now();
    s.apply_uncloak_with_collisions(1, 5, crate::space::collision::enabled());
    let after = crate::client::filetime_now();
    assert_eq!(super::events::destiny(own.try_recv().unwrap()), vec![event(5, "UncloakBall", vec![Value::Int(1)]), uncloak_fx(5, 1)]);
    let actual = super::events::destiny(observer.try_recv().unwrap());
    let mut expected_add = add_balls_event(5, s.ball(1).unwrap(), true);
    super::events::reuse_checked_damage_time(&actual[0], &mut expected_add, before..=after);
    assert_eq!(actual, vec![expected_add, uncloak_fx(5, 1)]);
    super::events::fleet_snapshot(&mut observer);
    assert!(own.try_recv().is_err());
    assert!(observer.try_recv().is_err());
}

#[test]
fn expiry_uncloaks_without_cancellation_notification() {
    let registry = Arc::new(Pilots::default());
    let (tx, mut notifications) = mpsc::unbounded_channel();
    registry.register_pilot(100, tx);
    let mut s = System::new(0, registry, 0);
    let mut hidden = ball(1, Some(100), 0.0);
    hidden.cloak = CLOAK_RESTORE;
    hidden.cloak_until = crate::client::filetime_from_unix_ms(1000);
    s.insert(hidden);
    let mut own = watch(&mut s, 1, 1);
    s.tick(1000);
    s.tick(2000);
    let batch = super::events::destiny(own.try_recv().unwrap());
    assert_eq!(batch.len(), 2);
    assert_eq!(name(&batch[0]), b"UncloakBall");
    assert_eq!(batch[1], uncloak_fx(1, 1));
    assert!(notifications.try_recv().is_err());
}

#[test]
fn module_cloak_exit_uses_default_duration_and_preserves_order() {
    let mut s = System::new(0, Arc::default(), 0);
    s.insert(ball(1, Some(100), 0.0));
    s.insert(ball(2, None, 3000.0));
    let mut own = watch(&mut s, 1, 1);
    let mut observer = watch(&mut s, 2, 2);
    let generation = Arc::new(std::sync::atomic::AtomicBool::new(false));
    assert!(s.begin_module_cloak(1, 10, generation.clone(), 0, 0, |_| true, None));
    s.tick(1_000);
    let expected = vec![special_fx(0, 1, None, None, None, "effects.Cloak", false, 1, 0,
        Value::Int(SPECIAL_FX_UNSPECIFIED_DURATION), 0, None, 0, Value::None), event(0, "CloakBall", vec![Value::Int(1)])];
    assert_eq!(super::events::destiny(own.try_recv().unwrap()), expected);
    assert_eq!(super::events::destiny(observer.try_recv().unwrap()), expected);
    super::events::fleet_snapshot(&mut observer);
    s.stop_module_cloak(1, 10, &generation);
    s.stop_module_cloak(1, 10, &generation);
    let before = crate::client::filetime_now();
    s.tick(2_000);
    let after = crate::client::filetime_now();
    assert_eq!(super::events::destiny(own.try_recv().unwrap()), vec![event(1, "UncloakBall", vec![Value::Int(1)]), uncloak_fx(1, 1)]);
    let actual = super::events::destiny(observer.try_recv().unwrap());
    let mut expected_add = add_balls_event(1, s.ball(1).unwrap(), true);
    super::events::reuse_checked_damage_time(&actual[0], &mut expected_add, before..=after);
    assert_eq!(actual, vec![expected_add, uncloak_fx(1, 1)]);
    super::events::fleet_snapshot(&mut observer);
    assert!(own.try_recv().is_err());
    assert!(observer.try_recv().is_err());
    assert_eq!(s.ball(1).unwrap().cloak, 0);
}

#[test]
fn module_cloak_entry_sends_cloak_fx_before_cloakball_at_one_stamp() {
    let mut s = System::new(0, Arc::default(), 0);
    s.insert(ball(1, Some(100), 0.0));
    s.insert(ball(2, None, 3000.0));
    let mut own = watch(&mut s, 1, 1);
    let mut observer = watch(&mut s, 2, 2);
    let generation = Arc::new(std::sync::atomic::AtomicBool::new(false));
    assert!(s.begin_module_cloak(1, 10, generation.clone(), 0, 0, |_| true, None));
    s.tick(1_000);
    for rx in [&mut own, &mut observer] {
        assert_eq!(super::events::destiny(rx.try_recv().unwrap()), vec![
            special_fx(0, 1, None, None, None, "effects.Cloak", false, 1, 0,
                Value::Int(SPECIAL_FX_UNSPECIFIED_DURATION), 0, None, 0, Value::None),
            event(0, "CloakBall", vec![Value::Int(1)])]);
    }
    super::events::fleet_snapshot(&mut observer);
    assert!(own.try_recv().is_err() && observer.try_recv().is_err(), "no RemoveBalls follows a native cloak");
    assert_eq!(s.ball(1).unwrap().cloak, 1);
}
