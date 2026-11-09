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
        warp: space::WarpState::default(), last_ms: 1000,
        interpolation_anchor_ms: 1000, effect_stamp: 0, cloak: 0, cloak_until: 0,
        health: [1.0; 3], launcher: 0, slim: vec![],
    }
}

#[test]
fn actor_identity_visibility_and_command_fence() {
    crate::tests::init_config();
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
    crate::tests::init_config();
    let mut st = setup();
    let mut order = snapshot(&st, 3, 103).unwrap();
    target(&mut order, 5, 105);
    let mut s = space::System::new(1, std::sync::Arc::default(), 1000);
    s.insert(ball(103, 3));
    s.insert(ball(104, 4));
    s.insert(ball(105, 5));
    assert!(commit_in(&st, &mut s, &order, 4, 104, |_| ()).is_some());
    let mut hidden = ball(105, 5);
    hidden.cloak = 1;
    s.insert(hidden);
    assert!(commit_in(&st, &mut s, &order, 4, 104, |_| ()).is_none());
    s.insert(ball(105, 5));
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    s.observe(1_000_000_000_001, 103, tx);
    assert!(s.fleet_broadcast_visible(3, 103));
    let mut self_cloaked = ball(103, 3);
    self_cloaked.cloak = 1;
    s.insert(self_cloaked);
    assert!(s.fleet_broadcast_visible(3, 103));
    assert!(s.visible_to(3, 103));
    s.insert(ball(103, 3));
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
    s.insert(ball(105, 5));
    s.unobserve(1_000_000_000_001);
    let (stale, _stale_rx) = tokio::sync::mpsc::unbounded_channel();
    s.observe(1_000_000_000_001, 104, stale);
    assert!(!s.fleet_broadcast_visible(3, 104));
    assert!(commit_in(&st, &mut s, &order, 4, 104, |_| ()).is_some());
    let (closed, closed_rx) = tokio::sync::mpsc::unbounded_channel();
    s.observe(1_000_000_000_001, 103, closed);
    drop(closed_rx);
    assert!(!s.fleet_broadcast_visible(3, 104));
    assert!(commit_in(&st, &mut s, &order, 4, 104, |_| ()).is_some());
}
#[test]
fn fleet_commits_keep_each_members_factor() {
    crate::tests::init_config();
    let st = setup();
    let order = snapshot(&st, 3, 103).unwrap();
    let mut s = space::System::new(1, std::sync::Arc::default(), 1000);
    s.insert(ball(103, 3));
    s.insert(ball(104, 4));
    for (pilot, ship, factor) in [(3, 103, 4000), (4, 104, 4320)] {
        let revision = s.projection_revision(ship);
        assert_eq!(commit_in(&st, &mut s, &order, pilot, ship, |s| {
            super::super::super::ship::commit_warp(s, ship, revision, false, factor, WarpDestination::Exact([1_000_000.0, 0.0, 0.0]), None)
        }).unwrap(), super::super::super::ship::WarpCommit::Queued);
    }
    s.tick(2000);
    assert_eq!(s.ball(103).unwrap().warp.factor, 4000);
    assert_eq!(s.ball(104).unwrap().warp.factor, 4320);
}
#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated database with SDE; run serially"]
async fn fleet_execute_uses_each_pilots_real_fitted_speed() {
    use infinity_core::consts::{FLAG_IMPLANT, TYPE_CAPSULE};
    use std::sync::Arc;
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::deployment::DatabaseConfig {
        schema_on_start: false, ..crate::config::tests::test_database()
    }).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let system = -1;
    let mut fixtures = Vec::new();
    for _ in 0..2 {
        let account: i64 = db.query_one("INSERT INTO game.accounts (user_name,password_hash,role) VALUES (nextval('game.accounts_user_id_seq')::text,''::bytea,0) RETURNING user_id", &[]).await.unwrap().get("user_id");
        let pilot: i64 = db.query_one("INSERT INTO game.characters (user_id,name,race_id,bloodline_id,gender_id,corporation_id) VALUES ($1,nextval('game.characters_character_id_seq')::text,1,1,1,1) RETURNING character_id", &[&account]).await.unwrap().get("character_id");
        let ship = crate::dao::items::creation::positioned(&db, TYPE_CAPSULE, pilot, system, [0.0; 3]).await.unwrap();
        fixtures.push((account, pilot, ship));
    }
    let (_, leader, leader_ship) = fixtures[0];
    let (_, member, member_ship) = fixtures[1];
    db.execute("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton,brain_slot) VALUES ($1,$2,$2,$3,1,true,$4)", &[&3117_i32, &member, &FLAG_IMPLANT, &6_i32]).await.unwrap();
    let world = space::World::new(db.clone(), sde, 1);
    let fleet_id = leader;
    let mut fleet = Fleet::new(leader, vec![]);
    fleet.members.insert(leader, Member::new(NONE_ID, NONE_ID, ROLE_LEADER, JOB_NONE, 1));
    fleet.members.insert(member, Member::new(NONE_ID, NONE_ID, ROLE_MEMBER, JOB_NONE, 1));
    { let mut st = state(); st.fleets.insert(fleet_id, fleet); st.of.insert(leader, fleet_id); st.of.insert(member, fleet_id); }
    let order = snapshot(&state(), leader, leader_ship).unwrap();
    let w = world.clone();
    let d = db.clone();
    let result = tokio::spawn(async move {
        w.system(system).await.unwrap();
        for (pilot, ship) in [(leader, leader_ship), (member, member_ship)] {
            let row = crate::dao::items::space_ships::FreeShip {
                item_id: ship, type_id: TYPE_CAPSULE, owner_id: pilot, corporation_id: 1,
                alliance_id: -1, pos: [0.0; 3],
            };
            let prepared = crate::services::ship::balls::piloted(&d, w.sde().clone(), &row, pilot, 0.0).await.unwrap();
            w.with(system, move |s| prepared.install(s, |s, ball| s.add_ship(ball))).await.unwrap();
        }
        execute(&d, &w, Some(system), order.clone(), WarpDestination::Exact([200_000_000.0, 0.0, 0.0]), None, Value::None).await.unwrap();
        assert!(w.after(system, 1200).await);
        assert_eq!(w.ball(system, leader_ship).await.unwrap().warp.factor, 4000);
        assert_eq!(w.ball(system, member_ship).await.unwrap().warp.factor, 4320);
        for ship in [leader_ship, member_ship] {
            w.with(system, move |s| s.command(ship, |ball, _, _| { ball.stop(); Vec::new() })).await.unwrap();
        }
        assert!(w.after(system, 1200).await);
        assert!(crate::dao::items::dogma::set_override(&d, leader_ship,
            infinity_core::consts::dogma::ATTR_WARP_SPEED_MULTIPLIER, 0.0).await.unwrap());
        let (arrived_tx, arrived_rx) = tokio::sync::oneshot::channel();
        let callback: Timer = Box::new(move |_| { let _ = arrived_tx.send(()); });
        let error = execute(&d, &w, Some(system), order, WarpDestination::Exact([200_000.0, 0.0, 0.0]),
            Some(callback), Value::None).await;
        assert!(error.is_err(), "later member's invalid fit must fail the order");
        w.with(system, move |s| s.command(member_ship, |ball, _, _| {
            ball.vel = [f64::from(ball.max_vel), 0.0, 0.0];
            Vec::new()
        })).await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(45), arrived_rx).await
            .expect("earlier accepted member did not complete").expect("arrival callback dropped");
        let (accepted_tx, accepted_rx) = tokio::sync::oneshot::channel();
        let (arrival_tx, arrival_rx) = tokio::sync::oneshot::channel();
        let cancelled_world = w.clone();
        let cancelled_db = d.clone();
        let mut request = Box::pin(async move {
            let guard = cancelled_world.begin_warp_arrival(system, Box::new(move |_| { let _ = arrival_tx.send(()); })).await.unwrap();
            assert!(super::super::super::ship::warp_pilot(&cancelled_db, &cancelled_world, system, member, member_ship,
                WarpDestination::Exact([1_000_000.0, 0.0, 0.0]), Some(guard.id()), None).await.unwrap());
            accepted_tx.send(guard.id()).unwrap();
            std::future::pending::<()>().await;
            drop(guard);
        });
        let arrival_id = tokio::select! {
            _ = &mut request => panic!("producer completed before cancellation"),
            accepted = accepted_rx => accepted.unwrap(),
        };
        drop(request);
        w.with(system, move |s| s.command(member_ship, |ball, _, _| {
            ball.vel = [f64::from(ball.max_vel), 0.0, 0.0];
            Vec::new()
        })).await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(45), arrival_rx).await
            .expect("accepted member did not complete after producer cancellation").expect("arrival callback dropped");
        assert!(!w.with(system, move |s| s.retain_warp_arrival(arrival_id)).await.unwrap());
    }).await;
    { let mut st = state(); st.fleets.remove(&fleet_id); st.of.remove(&leader); st.of.remove(&member); }
    for (account, pilot, ship) in fixtures {
        world.remove_ball(system, ship).await;
        db.execute("DELETE FROM game.item_attributes WHERE item_id IN (SELECT item_id FROM game.items WHERE owner_id=$1)", &[&pilot]).await.unwrap();
        db.execute("DELETE FROM game.items WHERE owner_id=$1", &[&pilot]).await.unwrap();
        db.execute("DELETE FROM game.characters WHERE character_id=$1", &[&pilot]).await.unwrap();
        db.execute("DELETE FROM game.accounts WHERE user_id=$1", &[&account]).await.unwrap();
    }
    result.unwrap();
}



#[test]
fn revision_and_scramble_preserve_single_arrival_callback() {
    crate::tests::init_config();
    use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};
    let st = setup();
    let order = snapshot(&st, 3, 103).unwrap();
    let mut s = space::System::new(1, Arc::default(), 1000);
    s.insert(ball(103, 3));
    s.insert(ball(104, 4));
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let callback: Timer = Box::new(move |_| { count.fetch_add(1, Ordering::SeqCst); });
    let arrival = s.register_warp_arrival(callback);
    let stale = s.projection_revision(104);
    s.refresh_projection_target(104);
    let result = commit_in(&st, &mut s, &order, 4, 104, |s| {
        super::super::super::ship::commit_warp(s, 104, stale, false, 3000, WarpDestination::Exact([1000.0, 0.0, 0.0]), Some(arrival))
    }).unwrap();
    assert_eq!(result, super::super::super::ship::WarpCommit::RetryProjection);
    let revision = s.projection_revision(104);
    let result = commit_in(&st, &mut s, &order, 4, 104, |s| {
        super::super::super::ship::commit_warp(s, 104, revision, true, 3000, WarpDestination::Exact([1000.0, 0.0, 0.0]), Some(arrival))
    }).unwrap();
    assert_eq!(result, super::super::super::ship::WarpCommit::Scrambled);
    let result = commit_in(&st, &mut s, &order, 4, 104, |s| {
        super::super::super::ship::commit_warp(s, 104, revision, false, 3000, WarpDestination::Exact([1000.0, 0.0, 0.0]), Some(arrival))
    }).unwrap();
    assert_eq!(result, super::super::super::ship::WarpCommit::Queued);
    s.release_warp_arrival(arrival);
    s.tick(2000);
    s.tick(3000);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}
