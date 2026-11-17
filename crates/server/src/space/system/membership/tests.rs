//! Scene readiness, shared deltas and pending-initial lifecycle.

use super::*;
use crate::space::system::tests::{ball, events, name, watch};

fn names(delivery: crate::space::SceneDelivery) -> Vec<Vec<u8>> {
    events::destiny(delivery).iter().map(|e| crate::space::system::tests::name(e).to_vec()).collect()
}

/// Ball ids named inside one AddBalls event, read from its slim items.
fn add_ids(add: &Value) -> Vec<i64> {
    add.items().unwrap()[1].items().unwrap()[1].items().unwrap()[0].items().unwrap()[1]
        .items().unwrap().iter().map(|slim| crate::services::value_field(slim, "itemID").and_then(Value::as_int).unwrap()).collect()
}

#[test]
fn births_ride_the_fifo_and_enter_only_at_the_boundary() {
    let mut s = System::new(1, Arc::default(), 0);
    s.commit_cold_scene();
    s.insert(ball(1, Some(100), 0.0));
    let mut rx = watch(&mut s, 1, 1);
    s.add_ship(ball(2, None, 10.0));
    assert!(rx.try_recv().is_err(), "unborn ball must not be delivered early");
    s.apply_scene_prefix(crate::space::collision::enabled());
    assert_eq!(names(rx.try_recv().unwrap()), [b"AddBalls"]);
}

#[test]
fn pending_gets_no_ordinary_scene_and_closed_channel_is_refused() {
    let mut s = System::new(1, Arc::default(), 0);
    s.insert(ball(1, Some(100), 0.0));
    let (tx, mut rx) = mpsc::unbounded_channel();
    let (completed, mut done) = tokio::sync::oneshot::channel();
    assert!(s.observe(1, 1, tx, completed));
    s.emit_destiny(DestinyAudience::System, vec![event(0, "before", Vec::new())]);
    assert!(rx.try_recv().is_err() && done.try_recv().is_err());
    s.apply_scene_prefix(crate::space::collision::enabled());
    assert_eq!(done.try_recv(), Ok(true));
    let _ = events::initial_state(&mut rx);
    let (dead, dead_rx) = mpsc::unbounded_channel();
    drop(dead_rx);
    let (refused, mut refused_done) = tokio::sync::oneshot::channel();
    assert!(!s.observe(2, 1, dead, refused));
    assert_eq!(refused_done.try_recv(), Ok(false));
}

#[test]
fn same_id_replacement_removes_before_it_adds() {
    let mut s = System::new(1, Arc::default(), 0);
    s.insert(ball(1, None, 0.0));
    let mut rx = watch(&mut s, 1, 1);
    s.destroy_ball(1);
    s.add_ship(ball(1, None, 3.0));
    s.apply_scene_prefix(crate::space::collision::enabled());
    let mut got: Vec<Vec<u8>> = Vec::new();
    while let Ok(delivery) = rx.try_recv() {
        if let crate::space::SceneDelivery::Destiny(_) = delivery { got.extend(names(delivery)); }
    }
    assert_eq!(got, [b"TerminalPlayDestructionEffect".as_slice(), b"RemoveBalls".as_slice(), b"AddBalls".as_slice()]);
}

#[test]
fn same_channel_successor_keeps_its_entry_completion_and_state() {
    let mut s = System::new(1, Arc::default(), 0);
    s.insert(ball(1, Some(100), 0.0));
    let (tx, mut rx) = mpsc::unbounded_channel();
    let (c1, mut d1) = tokio::sync::oneshot::channel();
    assert!(s.observe(1, 1, tx.clone(), c1));
    let first_epoch = s.observers[&1].epoch;
    let old = ObserverToken { client: 1, epoch: first_epoch };
    s.gm_previews.insert(1, crate::space::system::gm_effects::SentPreviews::owned(1, 100));
    let (c2, mut d2) = tokio::sync::oneshot::channel();
    assert!(s.observe(1, 1, tx, c2));
    assert_ne!(first_epoch, s.observers[&1].epoch);
    assert_eq!(d1.try_recv(), Ok(false), "superseded completion answers false once");
    assert!(s.gm_previews.is_empty(), "superseded GM previews never revive");
    let new_epoch = s.observers[&1].epoch;
    assert!(!s.present_initial(old, false));
    assert_eq!(s.observers[&1].epoch, new_epoch);
    assert!(d2.try_recv().is_err() && rx.try_recv().is_err());
    assert!(!s.finish_initial(old));
    assert_eq!(s.observers[&1].epoch, new_epoch);
    assert!(d2.try_recv().is_err() && rx.try_recv().is_err());
    assert!(!s.fail_initial(old));
    assert_eq!(s.observers[&1].epoch, new_epoch);
    assert!(d2.try_recv().is_err() && rx.try_recv().is_err());
    s.apply_scene_prefix(crate::space::collision::enabled());
    assert_eq!(d2.try_recv(), Ok(true));
    assert!(s.observers.contains_key(&1), "stale finalize must not drop the new entry");
    let _ = events::initial_state(&mut rx);
    events::fleet_snapshot(&mut rx);
    assert!(rx.try_recv().is_err(), "one real boundary yields one SetState");
}

#[test]
fn scene_exit_is_an_ordinary_remove_never_a_terminal() {
    let mut s = System::new(1, Arc::default(), 0);
    s.insert(ball(1, Some(100), 0.0));
    s.insert(ball(2, None, 10.0));
    let mut rx = watch(&mut s, 1, 1);
    s.command(2, |b, _, _| { b.pos = [3.0 * crate::space::system::interest::BUBBLE_EDGE, 0.0, 0.0]; Vec::new() });
    s.apply_scene_prefix(crate::space::collision::enabled());
    assert_eq!(names(rx.try_recv().unwrap()), [b"RemoveBalls"]);
    assert_eq!(events::fleet_snapshot(&mut rx), Some(vec![crate::services::keyval([
        ("targetTags", Value::Dict(vec![(Value::Int(2), Value::None)]))])]));
    assert!(!s.visible_to(100, 2));
    s.command(2, |b, _, _| { b.pos = [10.0, 0.0, 0.0]; Vec::new() });
    s.apply_scene_prefix(crate::space::collision::enabled());
    assert_eq!(names(rx.try_recv().unwrap()), [b"AddBalls"]);
    assert!(s.visible_to(100, 2));
}

#[test]
fn take_delivers_a_plain_remove_and_globals_reach_every_scene() {
    let mut s = System::new(1, Arc::default(), 0);
    s.insert(ball(1, Some(100), 0.0));
    s.insert(ball(2, None, 10.0));
    let mut far = ball(3, None, 1e12);
    far.global = true;
    s.insert(far);
    let mut rx = watch(&mut s, 1, 1);
    assert!(s.visible_to(100, 3));
    s.take(2);
    s.apply_scene_prefix(crate::space::collision::enabled());
    assert_eq!(names(rx.try_recv().unwrap()), [b"RemoveBalls"]);
    let (tx, _rx) = mpsc::unbounded_channel();
    let (completed, mut done) = tokio::sync::oneshot::channel();
    assert!(s.observe(9, 1, tx, completed));
    s.apply_scene_prefix(crate::space::collision::enabled());
    assert_eq!(done.try_recv(), Ok(true));
    let token = s.observer_token(9).unwrap();
    assert!(s.scene_has(token, s.ball_token(3).unwrap()), "the global reaches every scene");
    assert!(s.ball_token(2).is_none(), "the taken ball leaves with its token");
}

#[cfg(feature = "tests-space")] #[test]
fn set_state_contains_visible_balls_ego_damage_and_system() {
    let mut s = System::new(42, Arc::default(), 0);
    s.insert(ball(1, Some(100), 0.0));
    let mut hidden = ball(2, Some(200), 1.0);
    hidden.cloak = CLOAK_RESTORE;
    s.insert(hidden);
    let (tx, mut rx) = mpsc::unbounded_channel();
    let (completed, mut done) = tokio::sync::oneshot::channel();
    assert!(s.observe(1, 1, tx, completed));
    s.apply_scene_prefix(crate::space::collision::enabled());
    assert_eq!(done.try_recv(), Ok(true));
    let state = events::initial_state(&mut rx);
    assert_eq!(name(&state), b"SetState");
    let Value::Tuple(parts) = state else { panic!("event") };
    let Value::Tuple(call) = &parts[1] else { panic!("call") };
    let Value::Tuple(args) = &call[1] else { panic!("args") };
    let Value::Instance(_, bag) = &args[0] else { panic!("bag") };
    let Value::Dict(fields) = &**bag else { panic!("fields") };
    let field = |key: &str| &fields.iter().find(|(k, _)| k == &Value::str(key)).unwrap().1;
    assert_eq!(field("ego").as_int(), Some(1));
    let Value::List(slims) = field("slims") else { panic!("slims") };
    assert_eq!(slims.len(), 1);
    let Value::Dict(damage) = field("damageState") else { panic!("damage") };
    assert_eq!(damage.len(), 1);
    assert_eq!(damage[0].0.as_int(), Some(1));
    let Value::Bytes(bytes) = field("state") else { panic!("state") };
    assert_eq!(bytes[0], DESTINY_FULLSTATE);
    assert_eq!(i64::from_le_bytes(bytes[5..13].try_into().unwrap()), 1);
    let Value::Instance(_, sol) = field("solItem") else { panic!("system") };
    let Value::Dict(sol) = &**sol else { panic!("system fields") };
    assert!(sol.iter().any(|(k,v)| k == &Value::str("itemID") && v.as_int() == Some(42)));
}

#[test]
fn fifo_replacement_pending_join_and_return_share_current_state() {
    let mut s = System::new(1, Arc::default(), 0);
    s.insert(ball(1, Some(100), 0.0));
    s.insert(ball(2, Some(200), 1.0));
    s.insert(ball(10, Some(1000), 5.0));
    s.insert(ball(9, Some(900), 4.0));
    let mut old = ball(3, None, 2.0);
    old.health = [0.9, 0.8, 0.7];
    old.slim.push(("name", Value::unicode("旧")));
    s.insert(old);
    let mut a = watch(&mut s, 1, 1);
    let mut b = watch(&mut s, 2, 2);
    let retired = s.ball_token(3).unwrap();
    let token1 = s.ball_token(1).unwrap();
    // The pending join lands behind the already-queued FIFO work.
    s.set_ball_position(2, [10.0, 0.0, 0.0], |_, _| true);
    s.take(3);
    let mut replacement = ball(3, None, 3.0);
    replacement.health = [0.5, 0.4, 0.3];
    replacement.slim.push(("name", Value::unicode("新")));
    s.add_ship(replacement);
    let successor = s.ball_token(3).unwrap();
    assert!(retired != successor);
    // A birth appended inside a real action lands after the captured count.
    s.actions.push_back((s.current_time, Box::new(move |s, _| s.add_ship(ball(4, None, 5.0)))));
    let (tx, mut j) = mpsc::unbounded_channel();
    let (completed, mut j_done) = tokio::sync::oneshot::channel();
    assert!(s.observe(9, 9, tx, completed));
    let before = crate::client::filetime_now();
    s.apply_scene_prefix(crate::space::collision::enabled());
    for rx in [&mut a, &mut b] {
        assert_eq!(events::destiny(rx.try_recv().unwrap()),
            vec![event(0, "SetBallPosition", vec![Value::Int(2), Value::Float(10.0), Value::Float(0.0), Value::Float(0.0)])]);
        assert_eq!(events::destiny(rx.try_recv().unwrap()), vec![remove_balls_event(0, &[3])]);
        let actual = events::destiny(rx.try_recv().unwrap());
        let mut expected = vec![add_balls_batch(0, &s.balls, &[successor], crate::space::collision::enabled())];
        events::reuse_checked_damage_time(&actual[0], &mut expected[0], before..=crate::client::filetime_now());
        assert_eq!(actual, expected, "the same-id Add carries the successor's current state");
    }
    assert_eq!(j_done.try_recv(), Ok(true));
    let state = events::initial_state(&mut j);
    let bag = &state.items().unwrap()[1].items().unwrap()[1].items().unwrap()[0];
    let slims = crate::services::value_field(bag, "slims").unwrap().items().unwrap();
    let slim3 = slims.iter().find(|v| crate::services::value_field(v, "itemID").and_then(Value::as_int) == Some(3)).unwrap();
    assert_eq!(crate::services::value_field(slim3, "name"), Some(&Value::unicode("新")), "the join sees the successor's current slim");
    let Value::Dict(damage) = crate::services::value_field(bag, "damageState").unwrap() else { panic!("damageState") };
    let layer3 = damage.iter().find(|(k, _)| k.as_int() == Some(3)).map(|(_, v)| v).expect("successor damage entry");
    let Value::Tuple(layers) = layer3 else { panic!("damage layers") };
    let Value::Tuple(shield) = &layers[0] else { panic!("shield layer") };
    assert_eq!((&shield[0], &layers[1], &layers[2]), (&Value::Float(0.5), &Value::Float(0.4), &Value::Float(0.3)),
        "the pending join reads the successor's exact three-layer health");
    events::fleet_snapshot(&mut j);
    assert!(j.try_recv().is_err(), "a pending join replays no history: no Terminal, Remove or earlier Motion");
    for rx in [&mut a, &mut b] { events::fleet_snapshot(rx); }
    // The appended birth applies only at the next real prefix, once for every holder.
    let before4 = crate::client::filetime_now();
    s.apply_scene_prefix(crate::space::collision::enabled());
    for rx in [&mut a, &mut b, &mut j] {
        let actual = events::destiny(rx.try_recv().unwrap());
        let mut expected = vec![add_balls_batch(0, &s.balls, &[s.ball_token(4).unwrap()], crate::space::collision::enabled())];
        events::reuse_checked_damage_time(&actual[0], &mut expected[0], before4..=crate::client::filetime_now());
        assert_eq!(actual, expected, "the deferred birth arrives with its own current state");
        events::fleet_snapshot(rx);
    }
    // Later state changes still name the current incarnation.
    s.set_health(3, [0.6; 3]);
    for rx in [&mut a, &mut b, &mut j] {
        let batch = events::destiny(rx.try_recv().unwrap());
        assert_eq!((batch.len(), name(&batch[0])), (1, b"OnDamageStateChange".as_slice()));
        let args = batch[0].items().unwrap()[1].items().unwrap()[1].items().unwrap();
        let Value::Tuple(layers) = &args[1] else { panic!("damage layers") };
        let Value::Tuple(shield) = &layers[0] else { panic!("shield layer") };
        assert_eq!((&args[0], &shield[0], &layers[1], &layers[2]),
            (&Value::Int(3), &Value::Float(0.6), &Value::Float(0.6), &Value::Float(0.6)));
    }
    // A's real departure is ordinary: its own ego is never Removed or Terminal.
    s.set_ball_position(1, [3.0 * crate::space::system::interest::BUBBLE_EDGE, 0.0, 0.0], |_, _| true);
    s.apply_scene_prefix(crate::space::collision::enabled());
    assert_eq!(events::destiny(a.try_recv().unwrap()), vec![remove_balls_event(0, &[2, 3, 4, 9, 10])]);
    events::fleet_snapshot(&mut a);
    for rx in [&mut b, &mut j] {
        assert_eq!(events::destiny(rx.try_recv().unwrap()), vec![remove_balls_event(0, &[1])]);
        events::fleet_snapshot(rx);
    }
    assert!(s.ball_token(1) == Some(token1) && s.ball(1).is_some() && s.observer_token(1).is_some(),
        "the leaving observer keeps its live ego, token and binding");
    // Returning through a real boundary delivers the cohort's current state.
    let before3 = crate::client::filetime_now();
    s.set_ball_position(1, [0.0, 0.0, 0.0], |_, _| true);
    s.apply_scene_prefix(crate::space::collision::enabled());
    // Shared and transfer Adds are compared by payload, not by wire order.
    let mut adds = [
        events::destiny(a.try_recv().unwrap()),
        events::destiny(a.try_recv().unwrap()),
    ];
    adds.sort_by_key(|batch| add_ids(&batch[0])[0]);
    let cohort = [s.ball_token(2).unwrap(), successor, s.ball_token(4).unwrap(),
        s.ball_token(9).unwrap(), s.ball_token(10).unwrap()];
    let mut expected = vec![
        vec![add_balls_batch(0, &s.balls, &[token1], crate::space::collision::enabled())],
        vec![add_balls_batch(0, &s.balls, &cohort, crate::space::collision::enabled())],
    ];
    for (actual, want) in adds.iter().zip(expected.iter_mut()) {
        assert_eq!((actual.len(), name(&actual[0])), (1, b"AddBalls".as_slice()));
        events::reuse_checked_damage_time(&actual[0], &mut want[0], before3..=crate::client::filetime_now());
        assert_eq!(*actual, *want, "the return Adds carry the cohort's current state exactly once");
    }
    events::fleet_snapshot(&mut a);
    for rx in [&mut b, &mut j] {
        let actual = events::destiny(rx.try_recv().unwrap());
        let mut expected = vec![add_balls_batch(0, &s.balls, &[token1], crate::space::collision::enabled())];
        events::reuse_checked_damage_time(&actual[0], &mut expected[0], before3..=crate::client::filetime_now());
        assert_eq!(actual, expected, "the stayers see A's return as one current Add");
        events::fleet_snapshot(rx);
    }
    assert!(s.ball_token(1) == Some(token1) && s.observer_token(1).is_some());
    // A flush marker queued behind a pending initial never fires early.
    s.command(2, |b, _, _| { b.pos[0] = 11.0; Vec::new() });
    let (tx, mut j2) = mpsc::unbounded_channel();
    let (completed, mut j2_done) = tokio::sync::oneshot::channel();
    assert!(s.observe(10, 10, tx, completed));
    let (ack, mut acked) = tokio::sync::oneshot::channel();
    s.queue_destiny_flush(ack);
    assert!(acked.try_recv().is_err(), "the marker rides behind its own prefix and initial");
    s.apply_scene_prefix(crate::space::collision::enabled());
    assert_eq!(j2_done.try_recv(), Ok(true));
    let _ = events::initial_state(&mut j2);
    events::fleet_snapshot(&mut j2);
    assert!(acked.try_recv().is_err(), "the ack waits for the tick's real drain");
    s.tick(1000);
    assert_eq!(acked.try_recv(), Ok(()));
}

#[test]
fn unchanged_boundaries_reuse_shared_publication() {
    let mut s = System::new(1, Arc::default(), 0);
    s.insert(ball(1, Some(100), 0.0));
    s.insert(ball(2, Some(200), 1.0));
    s.insert(ball(3, Some(300), 3.0 * crate::space::system::interest::BUBBLE_EDGE));
    s.insert({ let mut global = ball(5, None, 1e12); global.global = true; global });
    let mut a = watch(&mut s, 1, 1);
    let mut b = watch(&mut s, 2, 2);
    let mut c = watch(&mut s, 3, 3);
    let home = s.bubble_of(1).unwrap();
    assert_ne!(home, s.bubble_of(3).unwrap());
    let delivered = (s.attribution.delivery.events, s.attribution.delivery.copies);
    let ready = s.membership.ready.get(&home).cloned().unwrap();
    let committed = s.membership.committed.get(&home).cloned().unwrap();
    let capacity = s.membership.committed.get(&home).unwrap().capacity();
    s.apply_scene_prefix(crate::space::collision::enabled());
    s.apply_scene_prefix(crate::space::collision::enabled());
    assert!(a.try_recv().is_err() && b.try_recv().is_err() && c.try_recv().is_err());
    assert_eq!((s.attribution.delivery.events, s.attribution.delivery.copies), delivered,
        "an unchanged boundary submits nothing");
    assert!(s.membership.ready.get(&home) == Some(&ready), "ready roster changed on an idle boundary");
    assert!(s.membership.committed.get(&home) == Some(&committed), "committed roster changed on an idle boundary");
    assert!(s.membership.ready_global == s.membership.global_committed, "the global ready and committed layers diverged");
    assert_eq!(s.membership.committed.get(&home).unwrap().capacity(), capacity,
        "the delivered buffer is reused in place");
    // One dirty bubble emits a single shared Add to both of its holders.
    s.insert(ball(4, None, 2.0));
    s.apply_scene_prefix(crate::space::collision::enabled());
    let shared = events::destiny(a.try_recv().unwrap());
    assert_eq!(shared, events::destiny(b.try_recv().unwrap()), "both holders receive the same encoded event");
    assert_eq!(shared.len(), 1);
    assert_eq!(name(&shared[0]), b"AddBalls");
    assert!(c.try_recv().is_err(), "the outer bubble receives nothing");
    assert_eq!(s.attribution.delivery.events, delivered.0 + 1);
    assert_eq!(s.attribution.delivery.copies, delivered.1 + 2);
    events::fleet_snapshot(&mut a);
    events::fleet_snapshot(&mut b);
}

#[test]
fn unobserved_arrival_native_cloak_retires_resident_once() {
    let mut s = System::new(1, Arc::default(), 0);
    s.insert(ball(1, Some(100), 0.0));
    s.insert(ball(2, None, 3.0 * crate::space::system::interest::BUBBLE_EDGE + 1.0));
    let mut a = watch(&mut s, 1, 1);
    let resident = s.ball_token(2).unwrap();
    let far = s.bubble_of(2).unwrap();
    assert!(s.scene_ready(resident));
    let observer = s.observer_token(1).unwrap();
    assert!(!s.scene_has(observer, resident) && !s.membership.committed.contains_key(&far),
        "the far bubble has no committed publication yet");
    // The ego's real arrival materializes the resident exactly once.
    s.set_ball_position(1, [3.0 * crate::space::system::interest::BUBBLE_EDGE + 1.0, 0.0, 0.0], |_, _| true);
    let before = crate::client::filetime_now();
    s.apply_scene_prefix(crate::space::collision::enabled());
    let actual = events::destiny(a.try_recv().unwrap());
    let mut expected = vec![add_balls_batch(s.current_time(), &s.balls, &[resident], crate::space::collision::enabled())];
    events::reuse_checked_damage_time(&actual[0], &mut expected[0], before..=crate::client::filetime_now());
    assert_eq!(actual, expected);
    events::fleet_snapshot(&mut a);
    assert!(a.try_recv().is_err());
    assert!(s.scene_has(s.observer_token(1).unwrap(), resident));
    assert!(s.membership.committed.get(&far).is_some_and(|set| set.contains(&resident)));
    assert_eq!(s.scene_home(resident), (Some(far), false));
    assert!(s.ball_token(2) == Some(resident));
    // A native cloak retires the delivered projection once: CloakBall implies
    // the client-side removal, so no RemoveBalls or Terminal follows.
    let cloak = Arc::new(std::sync::atomic::AtomicBool::new(false));
    assert!(s.begin_module_cloak(2, 11, cloak, 0, 0, |_| true, None));
    s.apply_scene_prefix(crate::space::collision::enabled());
    assert_eq!(events::destiny(a.try_recv().unwrap()), vec![
        special_fx(s.current_time(), 2, None, None, None, "effects.Cloak", false, 1, 0,
            Value::Int(SPECIAL_FX_UNSPECIFIED_DURATION), 0, None, 0, Value::None),
        event(s.current_time(), "CloakBall", vec![Value::Int(2)])]);
    events::fleet_snapshot(&mut a);
    assert!(a.try_recv().is_err(), "no RemoveBalls follows a native cloak");
    let observer = s.observer_token(1).unwrap();
    assert!(!s.scene_has(observer, resident));
    assert!(!s.membership.committed.get(&far).is_some_and(|set| set.contains(&resident)));
    assert_eq!(s.scene_home(resident), (None, false));
    assert!(s.scene_ready(resident) && s.ball(2).is_some_and(|b| b.cloak == 1),
        "the world entity lives while its projection is gone");
    // An unchanged prefix repeats nothing; the entity stays cloaked, not removed.
    s.apply_scene_prefix(crate::space::collision::enabled());
    assert!(a.try_recv().is_err());
    assert!(s.scene_ready(resident) && s.ball(2).is_some_and(|b| b.cloak == 1));
    // The real uncloak re-adds the same live token once.
    assert!(s.uncloak(2));
    s.apply_scene_prefix(crate::space::collision::enabled());
    let actual = events::destiny(a.try_recv().unwrap());
    assert_eq!(actual.len(), 2);
    assert_eq!(name(&actual[0]), b"AddBalls");
    assert_eq!(name(&actual[1]), b"OnSpecialFX");
    assert!(s.ball_token(2) == Some(resident));
    assert!(s.scene_has(s.observer_token(1).unwrap(), resident));
    events::fleet_snapshot(&mut a);
}

