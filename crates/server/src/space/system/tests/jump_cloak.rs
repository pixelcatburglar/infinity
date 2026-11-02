use super::*;

fn uncloak_fx(stamp: i64, ship: i64) -> Value {
    event(stamp, "OnSpecialFX", vec![
        Value::Int(ship), Value::None, Value::None, Value::None, Value::None,
        Value::str("effects.Uncloak"), Value::Bool(false), Value::Int(1), Value::Int(0),
        Value::Int(5), Value::Int(0), Value::None, Value::Int(0), Value::None,
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
    let mut own = watch(&mut s, 1, 1);
    let mut observer = watch(&mut s, 2, 2);
    assert!(s.drop_jump_cloak(1));
    assert!(!s.drop_jump_cloak(1));
    assert_eq!(notifications.try_recv().unwrap(), ("OnJumpCloakCancelled".to_string(), vec![Value::Int(1)]));
    assert!(notifications.try_recv().is_err());
    s.tick(6_000);
    let expected_add = add_balls_event(5, s.ball(1).unwrap(), true);
    assert_eq!(own.try_recv().unwrap(), vec![event(5, "UncloakBall", vec![Value::Int(1)]), uncloak_fx(5, 1)]);
    assert_eq!(observer.try_recv().unwrap(), vec![expected_add, uncloak_fx(5, 1)]);
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
    s.insert(hidden);
    let mut own = watch(&mut s, 1, 1);
    let mut observer = watch(&mut s, 2, 2);
    s.apply_uncloak(1, 5);
    assert_eq!(own.try_recv().unwrap(), vec![event(5, "UncloakBall", vec![Value::Int(1)]), uncloak_fx(5, 1)]);
    assert_eq!(observer.try_recv().unwrap(), vec![add_balls_event(5, s.ball(1).unwrap(), true), uncloak_fx(5, 1)]);
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
    let batch = own.try_recv().unwrap();
    assert_eq!(batch.len(), 2);
    assert_eq!(name(&batch[0]), b"UncloakBall");
    assert_eq!(batch[1], uncloak_fx(1, 1));
    assert!(notifications.try_recv().is_err());
}
