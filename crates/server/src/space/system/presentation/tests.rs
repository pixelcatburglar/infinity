use super::*;
use crate::space::system::tests::{ball, events, name, watch};

fn module_fx(module: i64, duration_ms: f64, repeat: i64) -> ModuleFx {
    ModuleFx { module: FxModule { item: Some(module), type_id: 7 }, guid: "effects.Test".into(),
        charge: None, offensive: false, duration_ms, repeat, graphic_info: Value::None }
}

fn effect_entries(state: &Value) -> Vec<Value> {
    let bag = &state.items().unwrap()[1].items().unwrap()[1].items().unwrap()[0];
    crate::services::value_field(bag, "effectStates").unwrap().items().unwrap().to_vec()
}

#[test]
fn add_balls_carries_three_layer_damage_for_free_balls() {
    let mut free = ball(1, Some(10), 0.0);
    free.health = [0.5; 3];
    let rigid = { let mut b = ball(2, None, 1.0); b.free = false; b };
    let chunk = |ball: &Ball| {
        let before = crate::client::filetime_now();
        let event = crate::space::add_balls_event(5, ball, false);
        let after = crate::client::filetime_now();
        (<[Value; 3]>::try_from(event.items().unwrap()[1].items().unwrap()[1].items().unwrap()[0].items().unwrap().to_vec()).unwrap(), before..=after)
    };
    let ([_, _, damage], bounds) = chunk(&free);
    let mut expected = Value::Dict(vec![(Value::Int(1), free.damage_state())]);
    events::reuse_checked_damage_time(&damage, &mut expected, bounds);
    assert_eq!(damage, expected);
    let ([_, _, damage], _) = chunk(&rigid);
    assert_eq!(damage, Value::Dict(vec![]));
}

#[test]
fn current_fx_replays_in_setstate_and_expiry_stops_once() {
    let mut s = System::new(1, Arc::default(), 0);
    s.insert(ball(1, Some(10), 0.0));
    let generation = Arc::new(AtomicBool::new(false));
    assert!(s.begin_fx_activation(9, 1, None, generation.clone())
        && s.publish_module_fx(9, &generation, module_fx(9, 5000.0, 1), true, FxRetention::Current));
    let (tx, mut rx) = mpsc::unbounded_channel();
    s.tick(6_250);
    let (completed, mut done) = tokio::sync::oneshot::channel();
    assert!(s.observe(2, 1, tx, completed));
    s.apply_scene_prefix(crate::space::collision::enabled());
    assert_eq!(done.try_recv(), Ok(true));
    let [entry] = <[Value; 1]>::try_from(effect_entries(&events::initial_state(&mut rx))).unwrap();
    let args = entry.items().unwrap();
    assert_eq!(&args[0..6], &[Value::Int(1), Value::Int(9), Value::Int(7), Value::None, Value::None, Value::str("effects.Test")]);
    assert_eq!(&args[9..13], &[Value::Float(5000.0), Value::Int(0), Value::Int(crate::client::filetime_from_unix_ms(0)), Value::Int(12_500_000)]);
    events::fleet_snapshot(&mut rx);
    // A successor activation's stop cannot retire the previous generation's record.
    let next = Arc::new(AtomicBool::new(false));
    assert!(s.begin_fx_activation(9, 1, None, next.clone())
        && !s.publish_module_fx(9, &next, module_fx(9, 5000.0, 1), false, FxRetention::Current));
    assert!(s.presentation.current.len() == 1
        && s.publish_module_fx(9, &next, module_fx(9, 5000.0, 1), true, FxRetention::Current));
    assert_eq!(events::destiny(rx.try_recv().unwrap()).iter().map(name).collect::<Vec<_>>(), vec![b"OnSpecialFX".as_slice()]);
    s.tick(11_000); assert!(rx.try_recv().is_err() && s.presentation.current.len() == 1);
    s.tick(17_000);
    assert_eq!(events::destiny(rx.try_recv().unwrap()).iter().map(name).collect::<Vec<_>>(), vec![b"OnSpecialFX".as_slice()]);
    let (tx2, mut rx2) = mpsc::unbounded_channel();
    let (completed, mut done) = tokio::sync::oneshot::channel();
    assert!(s.observe(3, 1, tx2, completed));
    s.apply_scene_prefix(crate::space::collision::enabled());
    assert_eq!(done.try_recv(), Ok(true));
    assert!(effect_entries(&events::initial_state(&mut rx2)).is_empty());
}
#[test]
fn uncloak_replays_sustained_fx_to_newly_seeing_observers_only() {
    let mut s = System::new(1, Arc::default(), 0);
    s.insert(ball(1, Some(10), 0.0));
    s.insert(ball(2, Some(20), 1.0));
    let generation = Arc::new(AtomicBool::new(false));
    assert!(s.begin_fx_activation(9, 1, Some(2), generation.clone()));
    assert!(s.publish_module_fx(9, &generation, module_fx(9, 5000.0, 0), true, FxRetention::Current));
    let mut owner = watch(&mut s, 1, 1);
    let cloak = Arc::new(AtomicBool::new(false));
    assert!(s.begin_module_cloak(1, 11, cloak, 0, 0, |_| true, None));
    s.apply_scene_prefix(crate::space::collision::enabled());
    assert_eq!(events::destiny(owner.try_recv().unwrap()).iter().map(name).collect::<Vec<_>>(),
        vec![b"OnSpecialFX".as_slice(), b"CloakBall".as_slice()]);
    let mut other = watch(&mut s, 2, 2);
    s.apply_uncloak_with_collisions(1, 7, false);
    assert_eq!(events::destiny(owner.try_recv().unwrap()).iter().map(name).collect::<Vec<_>>(),
        vec![b"UncloakBall".as_slice(), b"SetBallMassive", b"OnSpecialFX"]);
    assert_eq!(events::destiny(other.try_recv().unwrap()).iter().map(name).collect::<Vec<_>>(),
        vec![b"AddBalls".as_slice(), b"OnSpecialFX"]);
    let replayed = events::destiny(other.try_recv().unwrap());
    let args = replayed[0].items().unwrap()[1].items().unwrap()[1].items().unwrap();
    assert_eq!(&args[0..4], &[Value::Int(1), Value::Int(9), Value::Int(7), Value::Int(2)]);
    assert_eq!(&args[9..11], &[Value::Float(5000.0), Value::Int(0)]);
    assert!(owner.try_recv().is_err());
    let _ = events::fleet_snapshot(&mut other);
}

#[test]
fn scene_exit_replay_and_global_cloak_keep_exact_holders() {
    let mut s = System::new(1, Arc::default(), 0);
    s.insert(ball(1, Some(10), 0.0));
    s.insert(ball(2, Some(20), 1.0));
    s.insert(ball(3, Some(30), 3.0 * crate::space::system::interest::BUBBLE_EDGE));
    let mut global = ball(4, Some(40), 1e12);
    global.global = true;
    s.insert(global);
    // R1 spans the departing ball and its staying target; R2 stays entirely.
    let gone = Arc::new(AtomicBool::new(false));
    assert!(s.begin_fx_activation(9, 1, Some(2), gone.clone())
        && s.publish_module_fx(9, &gone, module_fx(9, 60_000.0, 1), true, FxRetention::Current));
    let stays = Arc::new(AtomicBool::new(false));
    assert!(s.begin_fx_activation(8, 2, None, stays.clone())
        && s.publish_module_fx(8, &stays, module_fx(8, 60_000.0, 1), true, FxRetention::Current));
    let mut a = watch(&mut s, 1, 1);
    let mut b = watch(&mut s, 2, 2);
    let mut c = watch(&mut s, 3, 3);
    let mut g = watch(&mut s, 4, 4);
    // A Transient start reaches live holders once and is never replayed.
    assert!(s.publish_module_fx(8, &stays, module_fx(8, 1_000.0, 0), true, FxRetention::Transient));
    for rx in [&mut a, &mut b] {
        assert_eq!(events::destiny(rx.try_recv().unwrap()).iter().map(name).collect::<Vec<_>>(),
            vec![b"OnSpecialFX".as_slice()]);
    }
    // A retired record stops exactly once for its current holders.
    let retired = Arc::new(AtomicBool::new(false));
    assert!(s.begin_fx_activation(10, 2, None, retired.clone())
        && s.publish_module_fx(10, &retired, module_fx(10, 60_000.0, 1), true, FxRetention::Current));
    for rx in [&mut a, &mut b] {
        assert_eq!(events::destiny(rx.try_recv().unwrap()).iter().map(name).collect::<Vec<_>>(),
            vec![b"OnSpecialFX".as_slice()]);
    }
    s.retire_fx_activation(10, &retired);
    for rx in [&mut a, &mut b] {
        assert_eq!(events::destiny(rx.try_recv().unwrap()).iter().map(name).collect::<Vec<_>>(),
            vec![b"OnSpecialFX".as_slice()]);
    }
    assert_eq!(s.presentation.current.len(), 2);
    // The stayer's matching stop lands before the departing ball's Remove.
    s.take(1);
    s.apply_scene_prefix(crate::space::collision::enabled());
    let stop = events::destiny(b.try_recv().unwrap());
    assert_eq!((stop.len(), name(&stop[0])), (1, b"OnSpecialFX".as_slice()));
    let args = stop[0].items().unwrap()[1].items().unwrap()[1].items().unwrap();
    assert_eq!(&args[0..4], &[Value::Int(1), Value::Int(9), Value::Int(7), Value::Int(2)]);
    assert_eq!(events::destiny(b.try_recv().unwrap()), vec![event(0, "RemoveBalls", vec![Value::list([Value::Int(1)])])]);
    events::fleet_snapshot(&mut b);
    assert!(b.try_recv().is_err(), "the retained record never stops");
    assert_eq!(s.presentation.current.len(), 1, "only the departing record drained");
    while a.try_recv().is_ok() {}
    // The returner replays only the live record: the drained one is gone.
    s.add_ship(ball(1, Some(10), 0.0));
    s.apply_scene_prefix(crate::space::collision::enabled());
    assert_eq!(names_add(&mut a), [b"AddBalls"]);
    assert_eq!(names_add(&mut a), [b"AddBalls"]);
    let replay = events::destiny(a.try_recv().unwrap());
    let args = replay[0].items().unwrap()[1].items().unwrap()[1].items().unwrap();
    assert_eq!(&args[0..6], &[Value::Int(2), Value::Int(8), Value::Int(7), Value::None, Value::None, Value::str("effects.Test")]);
    assert_eq!(&args[9..13], &[Value::Float(60_000.0), Value::Int(1), Value::Int(crate::client::filetime_from_unix_ms(0)), Value::Int(0)]);
    assert_eq!(names_add(&mut b), [b"AddBalls"]);
    events::fleet_snapshot(&mut a);
    events::fleet_snapshot(&mut b);
    // A global cloak reaches every foreign scene once as its FX then CloakBall.
    let cloak = Arc::new(AtomicBool::new(false));
    assert!(s.begin_module_cloak(4, 12, cloak, 0, 0, |_| true, None));
    s.apply_scene_prefix(crate::space::collision::enabled());
    for rx in [&mut a, &mut b, &mut c, &mut g] {
        assert_eq!(events::destiny(rx.try_recv().unwrap()),
            vec![special_fx(0, 4, None, None, None, "effects.Cloak", false, 1, 0,
                Value::Int(SPECIAL_FX_UNSPECIFIED_DURATION), 0, None, 0, Value::None),
                event(0, "CloakBall", vec![Value::Int(4)])]);
        events::fleet_snapshot(rx);
    }
    assert!(a.try_recv().is_err() && b.try_recv().is_err() && c.try_recv().is_err() && g.try_recv().is_err(),
        "no RemoveBalls follows a native cloak");
    let token4 = s.ball_token(4).unwrap();
    assert!(!s.scene_has(s.observer_token(1).unwrap(), token4) && s.scene_ready(token4));
    assert_eq!(s.ball(4).unwrap().cloak, 1);
    // The real uncloak re-adds the live token once, to every holder but its own.
    assert!(s.uncloak(4));
    let before2 = crate::client::filetime_now();
    s.apply_scene_prefix(false);
    assert_eq!(events::destiny(g.try_recv().unwrap()).iter().map(name).collect::<Vec<_>>(),
        vec![b"UncloakBall".as_slice(), b"SetBallMassive", b"OnSpecialFX"]);
    for rx in [&mut a, &mut b, &mut c] {
        let actual = events::destiny(rx.try_recv().unwrap());
        let mut expected = vec![
            crate::space::add_balls_event(0, s.ball(4).unwrap(), false),
            special_fx(0, 4, None, None, None, "effects.Uncloak", false, 1, 0,
                Value::Int(SPECIAL_FX_UNSPECIFIED_DURATION), 0, None, 0, Value::None),
        ];
        events::reuse_checked_damage_time(&actual[0], &mut expected[0], before2..=crate::client::filetime_now());
        assert_eq!(actual, expected, "the foreign reveal carries current slim and three-layer health exactly once");
        events::fleet_snapshot(rx);
    }
}

fn names_add(rx: &mut mpsc::UnboundedReceiver<crate::space::SceneDelivery>) -> Vec<Vec<u8>> {
    events::destiny(rx.try_recv().unwrap()).iter().map(|e| name(e).to_vec()).collect()
}

#[test]
fn same_ego_initial_fleet_snapshot_stays_on_each_observer() {
    let mut s = System::new(1, Arc::default(), 0);
    s.insert(ball(1, Some(10), 0.0));
    let (first_tx, mut first_rx) = mpsc::unbounded_channel();
    let (first_done, mut first_complete) = tokio::sync::oneshot::channel();
    assert!(s.observe(101, 1, first_tx, first_done));
    let (second_tx, mut second_rx) = mpsc::unbounded_channel();
    let (second_done, mut second_complete) = tokio::sync::oneshot::channel();
    assert!(s.observe(102, 1, second_tx, second_done));
    let before = crate::client::filetime_now();
    s.apply_scene_prefix(crate::space::collision::enabled());
    assert_eq!(first_complete.try_recv(), Ok(true));
    assert_eq!(second_complete.try_recv(), Ok(true));
    let mut first_state = events::initial_state(&mut first_rx);
    let mut second_state = events::initial_state(&mut second_rx);
    let after = crate::client::filetime_now();
    let expected = vec![crate::services::keyval([("targetTags", Value::Dict(vec![]))])];
    assert_eq!(events::fleet_snapshot(&mut first_rx), Some(expected.clone()));
    assert_eq!(events::fleet_snapshot(&mut second_rx), Some(expected.clone()));
    assert!(first_rx.try_recv().is_err());
    assert!(second_rx.try_recv().is_err());
    let selected = s.pilot_observer(10).expect("same-ego observer").1;
    let first_selected = s.observer_token(101) == Some(selected);
    let reobserve = if first_selected { 102 } else { 101 };
    let (re_tx, mut re_rx) = mpsc::unbounded_channel();
    let (re_done, mut re_complete) = tokio::sync::oneshot::channel();
    assert!(s.observe(reobserve, 1, re_tx, re_done));
    let re_before = crate::client::filetime_now();
    s.apply_scene_prefix(crate::space::collision::enabled());
    assert_eq!(re_complete.try_recv(), Ok(true));
    let mut re_state = events::initial_state(&mut re_rx);
    let re_after = crate::client::filetime_now();
    let normalize = |state: &mut Value, bounds: std::ops::RangeInclusive<i64>| {
        let Value::Tuple(parts) = state else { panic!("SetState tuple") };
        let Value::Tuple(call) = &mut parts[1] else { panic!("SetState call") };
        let Value::Tuple(args) = &mut call[1] else { panic!("SetState args") };
        let Value::Instance(_, fields) = &mut args[0] else { panic!("SetState bag") };
        let Value::Dict(fields) = fields.as_mut() else { panic!("KeyVal fields") };
        let value = fields.iter_mut()
            .find_map(|(key, value)| (key.as_text().as_deref() == Some("damageState")).then_some(value))
            .expect("damageState");
        let Value::Dict(rows) = &mut *value else { panic!("damage dictionary") };
        let Value::Tuple(row) = &mut rows[0].1 else { panic!("damage row") };
        let Value::Tuple(shield) = &mut row[0] else { panic!("shield row") };
        let Value::Int(time) = &mut shield[2] else { panic!("damage time") };
        assert!(bounds.contains(time));
        *time = 0;
    };
    normalize(&mut first_state, before..=after);
    normalize(&mut second_state, before..=after);
    normalize(&mut re_state, re_before..=re_after);
    assert_eq!(first_state, second_state, "same ego receives the same complete SetState");
    assert_eq!(re_state, first_state, "reobserve preserves the complete SetState");
    assert_eq!(events::fleet_snapshot(&mut re_rx), Some(expected));
    assert!(re_rx.try_recv().is_err());
    if first_selected {
        assert!(first_rx.try_recv().is_err());
    } else {
        assert!(second_rx.try_recv().is_err());
    }
}
