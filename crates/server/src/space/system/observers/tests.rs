//! Ordered observer binding and ball-instance regression tests.

use super::*;
use crate::space::system::tests::{ball, name, watch};

#[test]
fn membership_cutoff_preserves_old_history_and_new_snapshot() {
    let mut s = System::new(1, Arc::default(), 0);
    s.insert(ball(1, None, 0.0));
    let mut old = watch(&mut s, 1, 1);
    s.destroy_ball(1);
    s.add_ship(ball(1, None, 3.0));
    let mut new = watch(&mut s, 2, 1);
    s.tick(1000);
    let batch = crate::space::system::tests::events::destiny(old.try_recv().unwrap());
    assert_eq!(batch.iter().map(name).collect::<Vec<_>>(), vec![b"TerminalPlayDestructionEffect".as_slice(), b"RemoveBalls"]);
    let batch = crate::space::system::tests::events::destiny(old.try_recv().unwrap());
    assert_eq!(batch.iter().map(name).collect::<Vec<_>>(), vec![b"AddBalls"]);
    assert!(new.try_recv().is_err());
}

#[test]
fn pending_motion_follows_snapshot_and_old_command_cannot_touch_replacement() {
    let mut s = System::new(1, Arc::default(), 0);
    s.insert(ball(1, None, 0.0));
    s.command(1, |ball, stamp, _| { ball.pos[0] = 5.0; vec![event(stamp, "Motion", vec![])] });
    let (tx, mut rx) = mpsc::unbounded_channel();
    let (completed, mut done) = tokio::sync::oneshot::channel();
    assert!(s.observe(1, 1, tx, completed));
    s.apply_scene_prefix(crate::space::collision::enabled());
    assert_eq!(done.try_recv(), Ok(true));
    let _ = crate::space::system::tests::events::initial_state(&mut rx);
    assert!(rx.try_recv().is_err(), "a pending join replays no history");
    assert_eq!(s.ball(1).unwrap().pos[0], 5.0, "the snapshot already carries the applied command");
    s.command(1, |ball, _, _| { ball.pos[0] = 99.0; Vec::new() });
    s.insert(ball(1, None, 7.0));
    s.tick(2000);
    assert_eq!(s.ball(1).unwrap().pos[0], 7.0);
}

#[test]
fn rebind_drops_only_its_pending_and_closed_observer_is_removed() {
    let mut s = System::new(1, Arc::default(), 0);
    s.insert(ball(1, None, 0.0));
    let mut first = watch(&mut s, 1, 1);
    let mut second = watch(&mut s, 2, 1);
    s.begin_destiny_batch();
    s.emit_destiny(DestinyAudience::System, vec![event(0, "before", vec![])]);
    let mut rebound = watch(&mut s, 1, 1);
    s.emit_destiny(DestinyAudience::System, vec![event(0, "after", vec![])]);
    s.finish_destiny_batch();
    assert!(first.try_recv().is_err());
    assert_eq!(name(&crate::space::system::tests::events::destiny(rebound.try_recv().unwrap())[0]), b"after");
    assert_eq!(crate::space::system::tests::events::destiny(second.try_recv().unwrap()).iter().map(name).collect::<Vec<_>>(), vec![b"before".as_slice(), b"after"]);
    let (tx, rx) = mpsc::unbounded_channel(); drop(rx);
    let (completed, mut done) = tokio::sync::oneshot::channel();
    assert!(!s.observe(1, 1, tx, completed));
    assert_eq!(done.try_recv(), Ok(false));
    assert!(s.observer_token(1).is_none());
    assert!(s.observer_token(2).is_some());
}

#[test]
fn missing_subject_skips_factory_and_live_subject_is_delivered() {
    let mut s = System::new(1, Arc::default(), 0);
    s.publish_destiny(999, |_| panic!("empty audience must not encode"));
    s.insert(ball(1, None, 0.0));
    let mut rx = watch(&mut s, 1, 1);
    s.publish_destiny(999, |_| panic!("missing subject must not encode"));
    assert!(rx.try_recv().is_err());
    s.publish_destiny(1, |stamp| vec![event(stamp, "present", vec![])]);
    assert_eq!(name(&crate::space::system::tests::events::destiny(rx.try_recv().unwrap())[0]), b"present");
}
#[test]
fn empty_audience_membership_keeps_flush_boundary() {
    let mut s = System::new(1, Arc::default(), 0);
    s.add_ship(ball(1, None, 0.0));
    let (tx, mut rx) = tokio::sync::oneshot::channel(); s.queue_destiny_flush(tx);
    assert!(rx.try_recv().is_err());
    s.tick(1000);
    assert!(rx.try_recv().is_ok());
}

#[test]
fn loss_replacement_keeps_hidden_ego_exit_before_handoff() {
    let mut s = System::new(1, Arc::default(), 0);
    let mut hidden = ball(1, Some(9), 0.0);
    hidden.cloak = 1;
    s.insert(hidden);
    s.insert(ball(2, None, 0.0));
    s.insert(ball(3, None, 3.0 * crate::space::system::interest::BUBBLE_EDGE));
    let mut pilot = watch(&mut s, 1, 1);
    let mut near = watch(&mut s, 2, 2);
    let mut far = watch(&mut s, 3, 3);
    let ticket = s.observer_token(1).unwrap();
    let departed = s.capture_ball(1).unwrap();
    assert!(s.destroy_ball(1).is_some());
    s.add_loss_replacement(departed, ball(5, Some(9), 0.0), None);
    s.apply_scene_prefix(crate::space::collision::enabled());
    let gone = crate::space::system::tests::events::destiny(pilot.try_recv().unwrap());
    assert_eq!(gone.iter().map(name).collect::<Vec<_>>(), vec![b"TerminalPlayDestructionEffect".as_slice(), b"RemoveBalls"]);
    assert_eq!(gone[0].items().unwrap()[0].as_int(), gone[1].items().unwrap()[0].as_int(), "one stamp for the hidden exit");
    let added = crate::space::system::tests::events::destiny(pilot.try_recv().unwrap());
    assert_eq!(added.iter().map(name).collect::<Vec<_>>(), vec![b"AddBalls".as_slice()]);
    assert!(s.observer_token(1) == Some(ticket), "the handoff kept the original epoch and channel");
    assert_eq!(s.ticket_ego(ticket), Some(5), "the binding was re-aimed at the replacement");
    let seen = crate::space::system::tests::events::destiny(near.try_recv().unwrap());
    assert_eq!(seen.iter().map(name).collect::<Vec<_>>(), vec![b"AddBalls".as_slice()], "a hidden ship's terminal never leaks to its bubble");
    assert!(near.try_recv().is_err());
    assert!(far.try_recv().is_err(), "a far spectator gains nothing from the handoff");
}

#[test]
fn loss_replacement_handoff_refuses_stale_binding_and_incarnation() {
    for stale in 0..3 {
        let registry = Arc::new(crate::space::Pilots::default());
        let (notify, _notices) = mpsc::unbounded_channel();
        registry.register_pilot(9, notify);
        let mut s = System::new(1, registry.clone(), 0);
        s.insert(ball(1, Some(9), 0.0));
        s.insert(ball(3, None, 0.0));
        let mut observer = watch(&mut s, 1, 1);
        let session = |ship| infinity_core::session::OnlineSession { character_id: 9, user_id: None,
            session_id: 7, ship_id: Some(ship), station_id: None, system_id: Some(1),
            corporation_id: None, alliance_id: None, faction_id: None, fleet_id: None,
            role: 0, corporation_role: 0 };
        registry.set_online_session(9, Some(session(2)));
        let departed = s.capture_ball(1).unwrap();
        assert!(s.destroy_ball(1).is_some());
        s.add_loss_replacement(departed, ball(2, Some(9), 0.0), Some(session(2)));
        let rebound = match stale {
            0 => Some(watch(&mut s, 1, 3)),
            1 => { let _ = s.take(2); s.insert(ball(2, Some(9), 0.0)); None }
            _ => { registry.set_online_session(9, Some(session(3))); None }
        };
        s.apply_scene_prefix(crate::space::collision::enabled());
        assert_eq!(s.observer_ego(1), Some(if stale == 0 { 3 } else { 1 }), "stale case {stale} must never inherit the departed binding");
        assert!(rebound.is_none() || s.observer_token(1).is_some(), "a live new epoch is never hijacked by a stale ticket");
        assert!(!std::iter::from_fn(|| observer.try_recv().ok()).flat_map(crate::space::system::tests::events::destiny)
            .any(|v| name(&v) == b"AddBalls".as_slice()), "stale case {stale} must replay no successor Add");
    }
}
