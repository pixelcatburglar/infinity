use super::*;

fn setup() -> State {
    let mut st = State::default();
    let mut fleet = Fleet::new(1, vec![]);
    for (id, wing, squad, role) in [(1, -1, -1, ROLE_LEADER), (2, 10, -1, ROLE_WING_COMMANDER),
        (3, 10, 20, ROLE_SQUAD_COMMANDER), (4, 10, 20, ROLE_MEMBER), (5, 10, 21, ROLE_MEMBER),
        (6, 11, 22, ROLE_MEMBER)] {
        fleet.members.insert(id, Member::new(wing, squad, role, JOB_NONE, 1));
        st.of.insert(id, 100);
    }
    st.fleets.insert(100, fleet);
    st
}
fn ids(order: &Order) -> Vec<i64> { order.members.iter().map(|(id, _)| *id).collect() }

#[test]
fn authority_scope_and_consent() {
    let mut st = setup();
    assert_eq!(ids(&snapshot(&st, 1, 101).unwrap()), vec![2, 3, 4, 5, 6, 1]);
    assert_eq!(ids(&snapshot(&st, 2, 102).unwrap()), vec![3, 4, 5, 2]);
    assert_eq!(ids(&snapshot(&st, 3, 103).unwrap()), vec![4, 3]);
    assert!(snapshot(&st, 4, 104).is_none());
    st.fleets.get_mut(&100).unwrap().members.get_mut(&4).unwrap().opt_outs.accepts_fleet_warp = false;
    assert_eq!(ids(&snapshot(&st, 3, 103).unwrap()), vec![3]);
}

#[test]
fn current_topology_and_consent_fence_snapshot() {
    let mut st = setup();
    let order = snapshot(&st, 3, 103).unwrap();
    let position = order.members.iter().find(|(id, _)| *id == 4).unwrap().1;
    assert!(current(&st, &order, 4, position));
    st.fleets.get_mut(&100).unwrap().members.get_mut(&4).unwrap().squad = 21;
    assert!(!current(&st, &order, 4, position));
    st.fleets.get_mut(&100).unwrap().members.get_mut(&4).unwrap().squad = 20;
    st.fleets.get_mut(&100).unwrap().members.get_mut(&4).unwrap().opt_outs.accepts_fleet_warp = false;
    assert!(!current(&st, &order, 4, position));
    st.fleets.get_mut(&100).unwrap().members.get_mut(&4).unwrap().opt_outs.accepts_fleet_warp = true;
    st.of.remove(&4);
    assert!(!current(&st, &order, 4, position));
    st.of.insert(4, 100);
    st.fleets.get_mut(&100).unwrap().members.get_mut(&3).unwrap().role = ROLE_MEMBER;
    assert!(!current(&st, &order, 4, position));
}

#[test]
fn reservations_remove_warp_authority_and_commit_consent() {
    let mut st = setup();
    let order = snapshot(&st, 3, 103).unwrap();
    let position = order.members.iter().find(|(id, _)| *id == 4).unwrap().1;
    let reservation = ReconnectState { generation: 1, deadline: Instant::now(), offered_session: None };
    st.fleets.get_mut(&100).unwrap().members.get_mut(&4).unwrap().reconnect = Some(reservation.clone());
    assert!(!ids(&snapshot(&st, 3, 103).unwrap()).contains(&4));
    assert!(!current(&st, &order, 4, position));
    st.fleets.get_mut(&100).unwrap().members.get_mut(&3).unwrap().reconnect = Some(reservation);
    assert!(snapshot(&st, 3, 103).is_none());
}

fn ball(id: i64, pilot: i64) -> space::Ball {
    space::Ball {
        id, type_id: 1, owner_id: pilot, corp_id: 1, alliance_id: -1,
        char_id: Some(pilot), mode: space::MODE_STOP, radius: 1.0, signature_radius: None,
        mass: 1_000_000.0, max_vel: 100.0, agility: 1.0, speed_fraction: 0.0,
        pos: [0.0; 3], vel: [0.0; 3], old_pos: [0.0; 3], old_vel: [0.0; 3],
        segment_acceleration: None, goto: [0.0; 3], follow_id: 0, follow_range: 0.0,
        free: true, global: false, massive: false, interactive: true,
        warp: space::WarpState::default(), warp_factor: 3000, last_ms: 1000,
        interpolation_anchor_ms: 1000, effect_stamp: 0, cloak: 0, cloak_until: 0,
        health: [1.0; 3], launcher: 0, slim: vec![],
    }
}

#[test]
fn actor_identity_visibility_and_command_fence() {
    let st = setup();
    let order = snapshot(&st, 3, 103).unwrap();
    let mut s = space::System::new(1, std::sync::Arc::default(), 1000);
    s.insert(ball(103, 3));
    s.insert(ball(104, 4));
    assert!(live(&s, 4, 104));
    assert!(!live(&s, 4, 105));
    let mut boarded = ball(105, 4);
    boarded.owner_id = 99;
    s.insert(boarded);
    assert!(!live(&s, 4, 104));
    assert!(!live(&s, 4, 105));
    s.take(105);
    s.insert(ball(104, 4));
    let mut cloaked = ball(104, 4);
    cloaked.cloak = 1;
    s.insert(cloaked);
    assert!(commit_in(&st, &mut s, &order, 4, 104, |_| ()).is_none());
    let mut warping = ball(104, 4);
    warping.mode = space::MODE_WARP;
    s.insert(warping);
    assert!(commit_in(&st, &mut s, &order, 4, 104, |_| ()).is_none());
    s.insert(ball(104, 4));
    assert!(commit_in(&st, &mut s, &order, 4, 104, |s| {
        s.command(104, |b, _, _| {
            b.warp_to([1_000_000.0, 0.0, 0.0], 0.0, 3000);
            b.set_speed_fraction(1.0);
            vec![]
        });
    }).is_some());
    s.tick(2000);
    assert_eq!(s.ball(104).unwrap().mode, space::MODE_WARP);
}

#[test]
fn character_target_is_current_visible_fleet_pilot_at_commit() {
    let mut st = setup();
    let mut order = snapshot(&st, 3, 103).unwrap();
    target(&mut order, 5, 105);
    let mut s = space::System::new(1, std::sync::Arc::default(), 1000);
    s.insert(ball(103, 3));
    s.insert(ball(104, 4));
    s.insert(ball(105, 5));
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    s.observe(3, 103, tx);
    assert!(commit_in(&st, &mut s, &order, 4, 104, |_| ()).is_some());
    st.of.remove(&5);
    assert!(commit_in(&st, &mut s, &order, 4, 104, |_| ()).is_none());
    st.of.insert(5, 999);
    assert!(commit_in(&st, &mut s, &order, 4, 104, |_| ()).is_none());
    st.of.insert(5, 100);
    let mut hidden = ball(105, 5);
    hidden.cloak = 1;
    s.insert(hidden);
    assert!(commit_in(&st, &mut s, &order, 4, 104, |_| ()).is_none());
    s.insert(ball(106, 5));
    assert!(commit_in(&st, &mut s, &order, 4, 104, |_| ()).is_none());
}

#[test]
fn revision_and_scramble_preserve_single_arrival_callback() {
    use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};
    let st = setup();
    let order = snapshot(&st, 3, 103).unwrap();
    let mut s = space::System::new(1, Arc::default(), 1000);
    s.insert(ball(103, 3));
    s.insert(ball(104, 4));
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let callback: Timer = Box::new(move |_| { count.fetch_add(1, Ordering::SeqCst); });
    let stale = s.projection_revision(104);
    s.refresh_projection_target(104);
    let (current, callback) = commit_in(&st, &mut s, &order, 4, 104, |s| {
        super::super::super::ship::commit_warp(s, 104, stale, false, None, [1000.0, 0.0, 0.0], 0.0, Some(callback))
    }).unwrap();
    assert!(!current);
    let revision = s.projection_revision(104);
    let (current, callback) = commit_in(&st, &mut s, &order, 4, 104, |s| {
        super::super::super::ship::commit_warp(s, 104, revision, true, None, [1000.0, 0.0, 0.0], 0.0, callback)
    }).unwrap();
    assert!(current);
    assert!(callback.is_some());
    let (current, callback) = commit_in(&st, &mut s, &order, 4, 104, |s| {
        super::super::super::ship::commit_warp(s, 104, revision, false, None, [1000.0, 0.0, 0.0], 0.0, callback)
    }).unwrap();
    assert!(current);
    assert!(callback.is_none());
    s.tick(2000);
    s.tick(3000);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}
