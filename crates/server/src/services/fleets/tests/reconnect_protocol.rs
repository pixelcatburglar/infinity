use super::*;
use crate::{conn::Conn, handshake::Login, macho::Address};
use std::{sync::Arc, time::Duration};

async fn rpc(c: &mut Client, fleet: i64, method: &str, args: &[Value], kwargs: &Value) -> CallResult {
    let bind = Value::Int(fleet);
    crate::services::dispatch(c, &Call { service: "fleetObjectHandler", method, args, kwargs, bound: true, bind_params: Some(&bind) }).await
}

async fn fail_character_query(c: &mut Client, db: &crate::db::Db, fleet: i64, method: &str) -> CallResult {
    let mut config = crate::config::tests::test_database();
    config.pool_size = 1;
    let failing = crate::db::Db::connect(&config).await.unwrap();
    failing.execute("SET statement_timeout = '100ms'", &[]).await.unwrap();
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let locker = db.clone();
    let task = tokio::spawn(async move {
        let mut ready = Some(ready_tx);
        let mut release = Some(release_rx);
        locker.transaction::<(), (), crate::db::Error, (), _, _, _>(
            move |tx, _| {
                let ready = ready.take().unwrap();
                let release = release.take().unwrap();
                Box::pin(async move {
                    tx.execute("LOCK TABLE game.characters IN ACCESS EXCLUSIVE MODE", &[]).await?;
                    let _ = ready.send(());
                    let _ = tokio::time::timeout(Duration::from_secs(5), release).await;
                    tx.query_one("SELECT 1 / 0 AS failure", &[]).await?;
                    Ok(())
                })
            },
            |(), ()| async { Ok(()) },
        ).await
    });
    tokio::time::timeout(Duration::from_secs(5), ready_rx).await.unwrap().unwrap();
    let original = std::mem::replace(&mut c.db, failing);
    let result = tokio::time::timeout(Duration::from_secs(3), rpc(c, fleet, method, &[Value::Int(0)], &Value::None)).await;
    c.db = original;
    let _ = release_tx.send(());
    let rollback = tokio::time::timeout(Duration::from_secs(5), task).await.expect("locker did not finish").unwrap().expect_err("locker committed instead of rolling back");
    assert!(matches!(&rollback, crate::db::Error::Postgres(error) if error.code() == Some(&tokio_postgres::error::SqlState::DIVISION_BY_ZERO)), "unexpected locker error: {rollback:?}");
    result.expect("query did not finish under statement timeout")
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external listener flock"]
async fn reconnect_dispatch_invite_accept_and_init_state() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let now = crate::client::filetime_now();
    let account = -now;
    let leader = 1_500_000_000 + now % 100_000_000;
    let pilot = leader + 1;
    let name = format!("reconnect-test-{now}");
    let password: Vec<u8> = Vec::new();
    db.execute("INSERT INTO game.accounts (user_id,user_name,password_hash,role) VALUES ($1,$2,$3,0)", &[&account, &name, &password]).await.unwrap();
    for id in [leader, pilot] {
        let n = format!("{name}-{id}");
        db.execute("INSERT INTO game.characters (character_id,user_id,name,race_id,bloodline_id,gender_id,corporation_id,balance) VALUES ($1,$2,$3,1,1,1,$4,0)", &[&id, &account, &n, &account]).await.unwrap();
    }
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let world = World::new(db.clone(), sde.clone(), 1);
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    world.register_pilot(leader, tx);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let _peer = tokio::net::TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
    let (stream, address) = listener.accept().await.unwrap();
    let cfg = Arc::new(crate::config::Config::default());
    let mut c = Client::new(Conn::new(stream, address), cfg, db.clone(), sde, crate::chat::Hub::new("localhost", db.clone(), world.clone()), world.clone(), Login { user_id: account, user_name: name, role: 0, client_id: 1, session_id: 12, language_id: "EN".into() });
    c.session.apply(vec![("charid", Value::Int(pilot))]);
    let id = next_id();
    let mut f = Fleet::new(leader, vec![Wing { id: 10, name: String::new(), squads: vec![Squad { id: 11, name: String::new() }, Squad { id: 12, name: String::new() }] }]);
    f.initialized = true;
    f.members.insert(leader, Member::new(NONE_ID, NONE_ID, ROLE_LEADER, JOB_CREATOR, 0));
    let mut m = Member::new(10, 11, ROLE_MEMBER, JOB_NONE, 123);
    m.active_session = Some(11);
    f.members.insert(pilot, m);
    { let mut st = state(); st.fleets.insert(id, f); st.of.insert(leader, id); st.of.insert(pilot, id); lifecycle::reserve(&mut st, pilot, 11, Instant::now(), Duration::from_secs(120)); }
    let (pilot_tx, mut pilot_rx) = tokio::sync::mpsc::unbounded_channel();
    world.register_pilot(pilot, pilot_tx);
    let none = Value::None;
    c.session.apply(vec![("charid", Value::Int(leader))]);
    rpc(&mut c, id, "MoveMember", &[Value::Int(pilot), Value::Int(10), Value::Int(12), Value::Int(ROLE_MEMBER)], &none).await.unwrap();
    assert!(pilot_rx.try_recv().is_err());
    { let mut st = state(); st.fleets.get_mut(&id).unwrap().members.get_mut(&pilot).unwrap().opt_outs.accepts_fleet_warp = false; }
    c.session.apply(vec![("charid", Value::Int(pilot))]);
    while rx.try_recv().is_ok() {}
    c.session.apply(vec![("charid", Value::None)]);
    assert!(rpc(&mut c, id, "Reconnect", &[], &none).await.is_err());
    c.session.apply(vec![("charid", Value::Int(pilot))]);
    let bind = Value::Int(id);
    let unbound = Call { service: "fleetObjectHandler", method: "Reconnect", args: &[], kwargs: &none, bound: false, bind_params: Some(&bind) };
    assert!(super::super::fleet_object_handler::dispatch(&mut c, &unbound).await.is_none());
    let wrong = Call { service: "wrongService", bound: true, ..unbound };
    assert!(super::super::fleet_object_handler::dispatch(&mut c, &wrong).await.is_none());
    assert!(rpc(&mut c, id, "Reconnect", &[Value::Int(1)], &none).await.is_err());
    assert!(rpc(&mut c, id, "Reconnect", &[], &Value::dict([("bad", Value::Int(1))])).await.is_err());
    let (generation, deadline) = { let st = state(); let r = st.fleets[&id].members[&pilot].reconnect.as_ref().unwrap(); (r.generation, r.deadline) };
    { let mut st = state(); lifecycle::expire(&mut st, &world, id, pilot, generation, deadline, deadline - Duration::from_nanos(1)); assert!(st.has_member(id, pilot)); lifecycle::expire(&mut st, &world, id + 1, pilot, generation, deadline, deadline); assert!(st.has_member(id, pilot)); }
    { let mut st = state(); st.fleets.get_mut(&id).unwrap().members.get_mut(&pilot).unwrap().reconnect.as_mut().unwrap().deadline = Instant::now(); }
    assert!(rpc(&mut c, id, "Reconnect", &[], &none).await.is_err());
    { let mut st = state(); st.fleets.get_mut(&id).unwrap().members.get_mut(&pilot).unwrap().reconnect.as_mut().unwrap().deadline = deadline; }
    assert!(rpc(&mut c, id + 1, "Reconnect", &[], &none).await.is_err());
    let result = rpc(&mut c, id, "Reconnect", &[], &none).await.unwrap();
    assert_eq!(marshal::loads(&marshal::dumps(&result)).unwrap(), Value::None);
    assert!(c.session.int("fleetid").is_none());
    assert_eq!(c.outbox.len(), 1);
    assert!(matches!(&c.outbox[0].destination, Address::Broadcast { broadcast_id, .. } if broadcast_id.as_text().as_deref() == Some("OnFleetInvite")));
    let envelope = c.outbox[0].body[0].items().unwrap();
    let payload = marshal::loads(envelope[1].as_bytes().unwrap()).unwrap();
    let args = payload.items().unwrap()[1].items().unwrap();
    assert_eq!(args.len(), 4);
    assert_eq!(args[0], Value::Int(id));
    assert_eq!(args[1], Value::Int(leader));
    assert_eq!(crate::services::value_field(&args[3], "autoAccept"), Some(&Value::Bool(true)));
    rpc(&mut c, id, "Reconnect", &[], &none).await.unwrap();
    assert_eq!(c.outbox.len(), 1);
    c.session.sid = 13;
    assert!(rpc(&mut c, id, "Reconnect", &[], &none).await.is_err());
    assert!(rpc(&mut c, id, "AcceptInvite", &[Value::Int(999)], &none).await.is_err());
    c.session.sid = 12;
    assert!(fail_character_query(&mut c, &db, id, "AcceptInvite").await.is_err());
    { let st = state(); let r = st.fleets[&id].members[&pilot].reconnect.as_ref().unwrap(); assert_eq!((r.generation, r.deadline, r.offered_session), (generation, deadline, Some(12))); }
    assert!(c.session.int("fleetid").is_none());
    assert!(rx.try_recv().is_err());
    rpc(&mut c, id, "AcceptInvite", &[Value::Int(999)], &none).await.unwrap();
    assert_eq!(c.session.int("fleetid"), Some(id));
    assert_eq!(c.session.int("wingid"), Some(10));
    assert_eq!(c.session.int("squadid"), Some(12));
    assert_eq!(c.session.int("fleetrole"), Some(ROLE_MEMBER));
    assert_eq!(rx.try_recv().unwrap().0, "OnFleetJoin");
    { let st = state(); let member = &st.fleets[&id].members[&pilot]; assert_eq!(member.ship_type, 0); assert!(!member.opt_outs.accepts_fleet_warp); }
    rpc(&mut c, id, "Reconnect", &[], &none).await.unwrap();
    rpc(&mut c, id, "AcceptInvite", &[Value::Int(999)], &none).await.unwrap();
    assert!(rx.try_recv().is_err());
    { let mut st = state(); lifecycle::expire(&mut st, &world, id, pilot, generation, deadline, deadline); assert!(st.is_member(id, pilot)); }
    let init = rpc(&mut c, id, "GetInitState", &[], &none).await.unwrap();
    for field in ["fleetID", "members", "wings", "options", "motd"] { assert!(crate::services::value_field(&init, field).is_some()); }
    let other = next_id();
    while pilot_rx.try_recv().is_ok() {}
    {
        let mut st = state();
        lifecycle::reserve(&mut st, pilot, c.session.sid, Instant::now(), Duration::from_secs(120)).unwrap();
        let mut other_fleet = Fleet::new(leader, vec![Wing { id: 10, name: String::new(), squads: vec![Squad { id: 11, name: String::new() }] }]);
        other_fleet.initialized = true;
        other_fleet.invites.insert(pilot, (10, 11, ROLE_MEMBER));
        other_fleet.join_requests.insert(pilot, Value::None);
        st.fleets.insert(other, other_fleet);
    }
    c.session.apply(vec![("fleetid", Value::None), ("wingid", Value::None), ("squadid", Value::None), ("fleetrole", Value::None)]);
    rpc(&mut c, id, "Reconnect", &[], &none).await.unwrap();
    rpc(&mut c, id, "RejectInvite", &[Value::Bool(true)], &none).await.unwrap();
    assert!(pilot_rx.try_recv().is_err());
    {
        let mut st = state();
        assert!(!st.has_member(id, pilot));
        assert!(st.fleets[&other].invites.contains_key(&pilot));
        assert!(st.fleets[&other].join_requests.contains_key(&pilot));
        let deadline = Instant::now();
        let mut reserved = Member::new(10, 11, ROLE_MEMBER, JOB_NONE, 0);
        reserved.reconnect = Some(ReconnectState { generation: 999, deadline, offered_session: None });
        st.fleets.get_mut(&id).unwrap().members.insert(pilot, reserved.clone());
        st.of.insert(pilot, id);
        lifecycle::expire(&mut st, &world, id, pilot, 999, deadline, deadline);
        assert!(!st.has_member(id, pilot));
        assert!(st.fleets[&other].invites.contains_key(&pilot));
        assert!(st.fleets[&other].join_requests.contains_key(&pilot));
        st.fleets.get_mut(&id).unwrap().members.insert(pilot, reserved);
        st.of.insert(pilot, id);
    }
    c.session.apply(vec![("charid", Value::Int(leader))]);
    rpc(&mut c, id, "KickMember", &[Value::Int(pilot)], &none).await.unwrap();
    assert!(pilot_rx.try_recv().is_err());
    c.session.apply(vec![("charid", Value::Int(pilot))]);
    assert!(rpc(&mut c, id, "Reconnect", &[], &none).await.is_err());
    {
        let mut st = state();
        assert!(!st.has_member(id, pilot));
        assert!(st.fleets[&other].invites.contains_key(&pilot));
        assert!(st.fleets[&other].join_requests.contains_key(&pilot));
        let mut reserved = Member::new(10, 11, ROLE_MEMBER, JOB_NONE, 0);
        reserved.reconnect = Some(ReconnectState { generation: 1000, deadline: Instant::now() + Duration::from_secs(120), offered_session: None });
        st.fleets.get_mut(&id).unwrap().members.insert(pilot, reserved);
        st.of.insert(pilot, id);
        st.fleets.get_mut(&other).unwrap().max_members = 0;
    }
    assert!(pilot_rx.try_recv().is_err());
    assert!(rpc(&mut c, other, "AcceptInvite", &[Value::Int(0)], &none).await.is_err());
    { let mut st = state(); assert!(st.has_member(id, pilot)); st.fleets.get_mut(&other).unwrap().max_members = MAX_MEMBERS; }
    rpc(&mut c, other, "AcceptInvite", &[Value::Int(0)], &none).await.unwrap();
    { let st = state(); assert!(!st.has_member(id, pilot)); assert!(st.is_member(other, pilot)); assert!(!st.fleets[&other].join_requests.contains_key(&pilot)); }
    assert!(rpc(&mut c, other, "AcceptInvite", &[Value::Int(0)], &none).await.is_err());
    let shell = next_id();
    {
        let mut st = state();
        lifecycle::reserve(&mut st, pilot, c.session.sid, Instant::now(), Duration::from_secs(120)).unwrap();
        st.fleets.insert(shell, Fleet::new(pilot, vec![]));
    }
    c.session.apply(vec![("fleetid", Value::None), ("wingid", Value::None), ("squadid", Value::None), ("fleetrole", Value::None)]);
    let create = Call { service: "fleetObjectHandler", method: "CreateFleet", args: &[], kwargs: &none, bound: false, bind_params: None };
    assert!(crate::services::dispatch(&mut c, &create).await.is_err());
    { let mut st = state(); assert!(st.has_member(other, pilot)); st.fleets.remove(&shell); }
    c.session.apply(vec![("charid", Value::Int(0))]);
    assert!(crate::services::dispatch(&mut c, &create).await.is_err());
    assert!(state().has_member(other, pilot));
    c.session.apply(vec![("charid", Value::Int(pilot))]);
    crate::services::dispatch(&mut c, &create).await.unwrap();
    let created = c.bound.values().find_map(|(service, value)| (service == "fleetObjectHandler").then(|| value.as_int()).flatten()).unwrap();
    { let st = state(); assert!(!st.has_member(other, pilot)); assert!(st.fleets.contains_key(&created)); }
    assert!(fail_character_query(&mut c, &db, created, "Init").await.is_err());
    { let st = state(); assert!(!st.has_member(other, pilot)); assert!(!st.fleets[&created].initialized); assert_eq!(st.fleets[&created].creator_session, Some(c.session.sid)); }
    assert!(c.session.int("fleetid").is_none());
    rpc(&mut c, created, "Init", &[], &none).await.unwrap();
    assert_eq!(c.session.int("fleetid"), Some(created));
    assert!(rpc(&mut c, created, "Reconnect", &[], &none).await.is_err());
    { let mut st = state(); st.of.remove(&pilot); st.fleets.remove(&created); }
    { let mut st = state(); st.of.remove(&leader); st.of.remove(&pilot); st.fleets.remove(&id); }
    let peer2 = tokio::net::TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
    let (stream2, address2) = listener.accept().await.unwrap();
    let mut rival = Client::new(Conn::new(stream2, address2), c.cfg.clone(), db.clone(), c.sde.clone(), c.chat.clone(), world.clone(), Login { user_id: account, user_name: "race".into(), role: 0, client_id: 2, session_id: 13, language_id: "EN".into() });
    rival.session.apply(vec![("charid", Value::Int(pilot))]);
    let race_old = next_id();
    let race_new = next_id();
    {
        let mut st = state();
        let mut old = Fleet::new(leader, vec![]);
        old.initialized = true;
        old.members.insert(leader, Member::new(NONE_ID, NONE_ID, ROLE_LEADER, JOB_CREATOR, 0));
        let mut reserved = Member::new(NONE_ID, NONE_ID, ROLE_MEMBER, JOB_NONE, 0);
        reserved.reconnect = Some(ReconnectState { generation: 1001, deadline: Instant::now() + Duration::from_secs(120), offered_session: Some(c.session.sid) });
        old.members.insert(pilot, reserved);
        let mut new = Fleet::new(leader + 10, vec![Wing { id: 10, name: String::new(), squads: vec![Squad { id: 11, name: String::new() }] }]);
        new.initialized = true;
        new.invites.insert(pilot, (10, 11, ROLE_MEMBER));
        st.fleets.insert(race_old, old);
        st.fleets.insert(race_new, new);
        st.of.insert(pilot, race_old);
        st.of.insert(leader, race_old);
    }
    c.session.apply(vec![("fleetid", Value::None), ("wingid", Value::None), ("squadid", Value::None), ("fleetrole", Value::None)]);
    while rx.try_recv().is_ok() {}
    let (recovery, ordinary) = tokio::join!(rpc(&mut c, race_old, "AcceptInvite", &[Value::Int(0)], &none), rpc(&mut rival, race_new, "AcceptInvite", &[Value::Int(0)], &none));
    assert_ne!(recovery.is_ok(), ordinary.is_ok());
    { let st = state(); assert_ne!(st.has_member(race_old, pilot), st.has_member(race_new, pilot)); }
    assert_eq!(rx.try_recv().unwrap().0, if recovery.is_ok() { "OnFleetJoin" } else { "OnFleetLeave" });
    assert!(rx.try_recv().is_err());
    if recovery.is_ok() {
        assert!(rpc(&mut rival, race_new, "AcceptInvite", &[Value::Int(0)], &none).await.is_err());
    } else {
        assert!(rpc(&mut c, race_old, "AcceptInvite", &[Value::Int(0)], &none).await.is_err());
    }
    { let st = state(); assert_eq!(st.is_member(race_old, pilot), recovery.is_ok()); assert_eq!(st.is_member(race_new, pilot), ordinary.is_ok()); assert_eq!(st.of.get(&pilot), Some(&if recovery.is_ok() { race_old } else { race_new })); }
    assert!(rx.try_recv().is_err());
    { let mut st = state(); st.of.remove(&pilot); st.of.remove(&leader); st.fleets.remove(&race_old); st.fleets.remove(&race_new); }
    for action in ["kick", "expiry", "reject"] {
        let fleet_id = next_id();
        let deadline = Instant::now() + Duration::from_secs(120);
        {
            let mut st = state();
            let mut fleet = Fleet::new(leader, vec![]);
            fleet.initialized = true;
            fleet.members.insert(leader, Member::new(NONE_ID, NONE_ID, ROLE_LEADER, JOB_CREATOR, 0));
            let mut reserved = Member::new(NONE_ID, NONE_ID, ROLE_MEMBER, JOB_NONE, 0);
            reserved.reconnect = Some(ReconnectState { generation: 1003, deadline, offered_session: Some(c.session.sid) });
            fleet.members.insert(pilot, reserved);
            st.fleets.insert(fleet_id, fleet);
            st.of.insert(leader, fleet_id);
            st.of.insert(pilot, fleet_id);
        }
        while rx.try_recv().is_ok() {}
        while pilot_rx.try_recv().is_ok() {}
        for client in [&mut c, &mut rival] {
            client.session.apply(vec![("fleetid", Value::None), ("wingid", Value::None), ("squadid", Value::None), ("fleetrole", Value::None)]);
        }
        rival.session.sid = c.session.sid;
        rival.session.apply(vec![("charid", Value::Int(if action == "kick" { leader } else { pilot }))]);
        let competing = async {
            match action {
                "kick" => rpc(&mut rival, fleet_id, "KickMember", &[Value::Int(pilot)], &none).await.is_ok(),
                "reject" => rpc(&mut rival, fleet_id, "RejectInvite", &[Value::Bool(true)], &none).await.is_ok(),
                _ => {
                    let mut st = state();
                    let before = st.has_member(fleet_id, pilot);
                    lifecycle::expire(&mut st, &world, fleet_id, pilot, 1003, deadline, deadline);
                    before && !st.has_member(fleet_id, pilot)
                }
            }
        };
        let (accepted, removed) = tokio::join!(rpc(&mut c, fleet_id, "AcceptInvite", &[Value::Int(0)], &none), competing);
        let accepted = accepted.is_ok();
        if action == "kick" {
            assert!(accepted || removed);
        } else {
            assert_ne!(accepted, removed);
        }
        if !accepted {
            assert!(rpc(&mut c, fleet_id, "AcceptInvite", &[Value::Int(0)], &none).await.is_err());
        }
        if action == "kick" && !removed {
            assert!(rpc(&mut rival, fleet_id, "KickMember", &[Value::Int(pilot)], &none).await.is_err());
        } else if action == "reject" && !removed {
            assert!(rpc(&mut rival, fleet_id, "RejectInvite", &[Value::Bool(true)], &none).await.is_err());
        } else if action == "expiry" && accepted {
            let mut st = state();
            lifecycle::expire(&mut st, &world, fleet_id, pilot, 1003, deadline, deadline);
        }
        {
            let st = state();
            assert_eq!(st.has_member(fleet_id, pilot), !removed);
            assert_eq!(st.is_member(fleet_id, pilot), !removed);
            assert_eq!(st.of.get(&pilot), if removed { None } else { Some(&fleet_id) });
        }
        let mut joins = 0;
        let mut leaves = 0;
        while let Ok((event, _)) = rx.try_recv() {
            match event.as_str() {
                "OnFleetJoin" => joins += 1,
                "OnFleetLeave" => leaves += 1,
                _ => panic!("unexpected leader event: {event}"),
            }
        }
        assert_eq!(joins, usize::from(accepted));
        assert_eq!(leaves, usize::from(removed));
        let mut sessions = 0;
        let mut direct_leaves = 0;
        while let Ok((event, _)) = pilot_rx.try_recv() {
            match event.as_str() {
                "__session" => sessions += 1,
                "OnFleetLeave" => direct_leaves += 1,
                _ => panic!("unexpected pilot event: {event}"),
            }
        }
        let active_kicked = action == "kick" && accepted && removed;
        assert_eq!(sessions, usize::from(active_kicked));
        assert_eq!(direct_leaves, usize::from(active_kicked));
        { let mut st = state(); st.of.remove(&pilot); st.of.remove(&leader); st.fleets.remove(&fleet_id); }
    }
    drop(peer2);
    let disband = next_id();
    let active = pilot + 1;
    let (active_tx, mut active_rx) = tokio::sync::mpsc::unbounded_channel();
    world.register_pilot(active, active_tx);
    while rx.try_recv().is_ok() {}
    while pilot_rx.try_recv().is_ok() {}
    {
        let mut st = state();
        let mut fleet = Fleet::new(leader, vec![]);
        fleet.initialized = true;
        fleet.members.insert(leader, Member::new(NONE_ID, NONE_ID, ROLE_LEADER, JOB_CREATOR, 0));
        fleet.members.insert(active, Member::new(1, 1, ROLE_MEMBER, JOB_NONE, 0));
        let mut reserved = Member::new(1, 1, ROLE_MEMBER, JOB_NONE, 0);
        reserved.reconnect = Some(ReconnectState { generation: 1002, deadline: Instant::now() + Duration::from_secs(120), offered_session: None });
        fleet.members.insert(pilot, reserved);
        st.fleets.insert(disband, fleet);
        for id in [leader, active, pilot] { st.of.insert(id, disband); }
    }
    let ship = leader + 100;
    let ball = crate::space::Ball { id: ship, type_id: 1, owner_id: leader, corp_id: 1, alliance_id: -1, char_id: Some(leader), signature_radius: None,
        mode: crate::space::MODE_STOP, radius: 1.0, mass: 1.0, max_vel: 0.0, agility: 1.0, speed_fraction: 0.0,
        pos: [0.0; 3], vel: [0.0; 3], old_pos: [0.0; 3], old_vel: [0.0; 3], segment_acceleration: None,
        goto: [0.0; 3], follow_id: 0, follow_range: 0.0, free: false, global: false, massive: false, interactive: false,
        warp: crate::space::WarpState::default(), last_ms: 0, interpolation_anchor_ms: 0, effect_stamp: 0,
        cloak: 0, cloak_until: 0, health: [1.0; 3], launcher: 0, slim: Vec::new() };
    world.add_ship(1, ball).await.unwrap();
    c.session.apply(vec![("charid", Value::Int(leader)), ("solarsystemid", Value::Int(1)), ("shipid", Value::Int(ship))]);
    rpc(&mut c, disband, "SendBroadcast", &[Value::str("NeedBackup"), Value::Int(3), Value::None, Value::None], &none).await.unwrap();
    assert_eq!(active_rx.try_recv().unwrap().0, "OnFleetBroadcast");
    assert!(pilot_rx.try_recv().is_err());
    c.session.apply(vec![("charid", Value::Int(pilot))]);
    assert!(rpc(&mut c, disband, "SendBroadcast", &[Value::str("NeedBackup"), Value::Int(3), Value::None, Value::None], &none).await.is_err());
    while rx.try_recv().is_ok() {}
    c.session.apply(vec![("charid", Value::Int(leader))]);
    rpc(&mut c, disband, "DisbandFleet", &[], &none).await.unwrap();
    { let st = state(); assert!(!st.fleets.contains_key(&disband)); for id in [leader, active, pilot] { assert!(!st.of.contains_key(&id)); } }
    assert!(pilot_rx.try_recv().is_err());
    while let Ok((event, _)) = active_rx.try_recv() { assert_ne!(event, "OnFleetMemberChanged"); }
    while let Ok((event, _)) = rx.try_recv() { assert_ne!(event, "OnFleetMemberChanged"); }
    c.session.apply(vec![("charid", Value::Int(pilot))]);
    let succession = next_id();
    let (reserved_tx, mut reserved_rx) = tokio::sync::mpsc::unbounded_channel();
    world.register_pilot(pilot, reserved_tx);
    {
        let mut st = state();
        let mut fleet = Fleet::new(leader, vec![]);
        fleet.initialized = true;
        fleet.members.insert(leader, Member::new(NONE_ID, NONE_ID, ROLE_LEADER, JOB_CREATOR, 0));
        let mut reserved = Member::new(10, 11, ROLE_MEMBER, JOB_NONE, 0);
        reserved.reconnect = Some(ReconnectState { generation: 1, deadline: Instant::now(), offered_session: None });
        fleet.members.insert(pilot, reserved);
        let active = pilot + 1;
        fleet.members.insert(active, Member::new(10, 11, ROLE_MEMBER, JOB_NONE, 0));
        st.fleets.insert(succession, fleet);
        for ch in [leader, pilot, active] { st.of.insert(ch, succession); }
        lifecycle::remove_member_from(&mut st, &world, succession, leader);
        assert_eq!(st.fleets[&succession].members[&active].role, ROLE_LEADER);
        lifecycle::remove_member_from(&mut st, &world, succession, active);
        assert_eq!(st.fleets[&succession].members[&pilot].role, ROLE_LEADER);
        lifecycle::remove_member_from(&mut st, &world, succession, pilot);
        assert!(!st.fleets.contains_key(&succession));
    }
    assert!(reserved_rx.try_recv().is_err());
    db.execute("DELETE FROM game.characters WHERE character_id = ANY($1)", &[&vec![leader, pilot]]).await.unwrap();
    db.execute("DELETE FROM game.accounts WHERE user_id = $1", &[&account]).await.unwrap();
}
