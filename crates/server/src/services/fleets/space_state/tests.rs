use super::*;
use crate::space;
use crate::space::system::tests::events::{destiny, fleet_snapshot, initial_state};
use std::sync::Arc;

fn ball(id: i64, pilot: Option<i64>, x: f64) -> space::Ball {
    space::Ball { id, type_id: 1, owner_id: pilot.unwrap_or(0), corp_id: 1, alliance_id: -1, char_id: pilot,
        mode: space::MODE_STOP, radius: 1.0, signature_radius: None, mass: 1.0, max_vel: 100.0,
        agility: 1.0, speed_fraction: 0.0, pos: [x, 0.0, 0.0], vel: [0.0; 3], old_pos: [x, 0.0, 0.0], old_vel: [0.0; 3],
        segment_acceleration: None, goto: [0.0; 3], follow_id: 0, follow_range: 0.0, free: true, global: false,
        massive: false, interactive: pilot.is_some(), warp: space::WarpState::default(), 
        last_ms: 0, interpolation_anchor_ms: 0, effect_stamp: 0, cloak: 0, cloak_until: 0, health: [1.0; 3], launcher: 0, slim: vec![] }
}

async fn rpc(client: &mut Client, service: &str, method: &str, args: &[Value]) -> CallResult {
    crate::services::dispatch(client, &Call { service, method, args, kwargs: &Value::None, bound: false, bind_params: None }).await
}

/// Args of each `OnFleetStateChange` notification packet queued in the client's
/// outbox, in send order.
fn fleet_snapshots(client: &mut Client) -> Vec<Vec<Value>> {
    std::mem::take(&mut client.outbox).into_iter().filter_map(|p| {
        let infinity_wire::macho::Address::Broadcast { broadcast_id, .. } = &p.destination else { return None };
        if broadcast_id != &Value::str("OnFleetStateChange") { return None; }
        let Value::Tuple(body) = &p.body[0] else { return None };
        let Value::Bytes(dump) = &body[1] else { return None };
        let Value::Tuple(payload) = marshal::loads(dump).unwrap() else { return None };
        Some(payload[1].items().unwrap().to_vec())
    }).collect()
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with imported SDE; run serially"]
async fn fleet_space_dispatch_and_explicit_clears() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let target_type = sde.types.iter().find(|t| t.group_id.is_some_and(|g| !matches!(g, infinity_core::consts::GROUP_PLANET | infinity_core::consts::GROUP_ASTEROID_BELT | infinity_core::consts::GROUP_MOON))).unwrap().type_id;
    let world = space::World::new(db.clone(), sde.clone(), 1);
    let system = -123;
    let leader = 90_111_001;
    let member = 90_111_002;
    let outsider = 90_111_003;
    let fleet_id = next_id();
    let mut fleet = Fleet::new(leader, vec![]);
    fleet.members.insert(leader, Member::new(NONE_ID, NONE_ID, ROLE_LEADER, JOB_NONE, 0));
    fleet.members.insert(member, Member::new(NONE_ID, NONE_ID, ROLE_MEMBER, JOB_NONE, 0));
    { let mut st = state(); st.fleets.insert(fleet_id, fleet); st.of.insert(leader, fleet_id); st.of.insert(member, fleet_id); }
    world.system(system).await.unwrap();
    world.with(system, move |s| {
        s.insert(ball(11, Some(leader), 10.0)); s.insert(ball(12, Some(member), 20.0));
        s.insert(ball(13, Some(outsider), 30.0));
        let mut target = ball(14, None, 40.0); target.type_id = target_type; s.insert(target);
        let mut target = ball(15, None, 50.0); target.type_id = target_type; s.insert(target);
    }).await.unwrap();
    let (notice_tx, mut notices) = tokio::sync::mpsc::unbounded_channel();
    world.register_pilot(leader, notice_tx);
    let (member_tx, _member_notices) = tokio::sync::mpsc::unbounded_channel();
    world.register_pilot(member, member_tx);
    let (leader_tx, mut leader_rx) = tokio::sync::mpsc::unbounded_channel();
    let (member_scene_tx, _member_rx) = tokio::sync::mpsc::unbounded_channel();
    let empty = keyval([("targetTags", Value::Dict(vec![]))]);
    world.observe(system, 101, 11, leader_tx.clone()).await.unwrap();
    initial_state(&mut leader_rx);
    assert_eq!(fleet_snapshot(&mut leader_rx), Some(vec![empty.clone()]));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let _peer = tokio::net::TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
    let (stream, address) = listener.accept().await.unwrap();
    let cfg = Arc::new(crate::config::Config::default());
    let chat = crate::chat::Hub::new(&cfg.chat.domain, db.clone(), world.clone());
    let login = crate::handshake::Login { user_id: 1, user_name: "test".into(), role: 0, client_id: 101, session_id: 1, language_id: "EN".into() };
    let mut client = Client::new(infinity_wire::conn::Conn::new(stream, address), cfg, db, sde, chat, world.clone(), login);
    client.session.apply(vec![("charid", Value::Int(leader)), ("shipid", Value::Int(11)), ("solarsystemid", Value::Int(system))]);
    assert_eq!(rpc(&mut client, "sovDashboard", "GetCynoJammerState", &[]).await.unwrap(), Value::None);
    let expected = Value::Dict(vec![
        (Value::Int(leader), keyval([("pos", Value::tuple([Value::Float(10.0), Value::Float(0.0), Value::Float(0.0)]))])),
        (Value::Int(member), keyval([("pos", Value::tuple([Value::Float(20.0), Value::Float(0.0), Value::Float(0.0)]))])),
    ]);
    let positions = rpc(&mut client, "beyonce", "GetFleetLocationAndInfo", &[]).await.unwrap();
    let Value::Dict(rows) = positions else { panic!("fleet positions must be a dictionary") };
    let Value::Dict(expected_rows) = expected else { unreachable!() };
    assert_eq!(rows.len(), 2); for row in expected_rows { assert!(rows.contains(&row)); }
    world.with(system, move |s| { let mut hidden = ball(12, Some(member), 20.0); hidden.cloak = 1; s.insert(hidden); }).await.unwrap();
    let Value::Dict(visible) = rpc(&mut client, "beyonce", "GetFleetLocationAndInfo", &[]).await.unwrap() else { panic!("fleet positions must be a dictionary") };
    assert_eq!(visible.len(), 1);
    world.with(system, move |s| { s.insert(ball(12, Some(member), 20.0)); }).await.unwrap();
    world.remove_ball(system, 12).await.unwrap();
    assert!(world.after(system, crate::space::seconds_to_ms(1.0)).await);
    while leader_rx.try_recv().is_ok() {}
    let other_system = system - 1;
    world.system(other_system).await.unwrap();
    world.with(other_system, move |s| { s.insert(ball(12, Some(member), 20.0)); }).await.unwrap();
    let Value::Dict(absent) = rpc(&mut client, "beyonce", "GetFleetLocationAndInfo", &[]).await.unwrap() else { panic!("fleet positions must be a dictionary") };
    assert_eq!(absent.len(), 1);
    world.remove_ball(other_system, 12).await.unwrap();
    let before = crate::client::filetime_now();
    let (stamp, born) = world.with(system, move |s| {
        s.insert(ball(12, Some(member), 20.0));
        crate::space::system::tests::events::publish_scene_prefix(s, crate::space::collision::enabled());
        (s.current_time(), s.ball(12).unwrap().clone())
    }).await.unwrap();
    assert!(born.slim.is_empty(), "fixture ball carries no extra slim fields");
    let mut stream = vec![1u8]; // DESTINY_BALLS kind byte precedes the stamp and ball bytes
    stream.extend_from_slice(&(stamp as i32).to_le_bytes());
    stream.extend_from_slice(&crate::space::system::tests::events::encoded_ball_for_test(&born, crate::space::collision::enabled()));
    let slim = Value::instance("eve.common.script.util.slimItem.SlimItem", vec![
        ("itemID", Value::Int(born.id)), ("typeID", Value::Int(born.type_id as i64)), ("ownerID", Value::Int(born.owner_id)),
        ("charID", Value::Int(member)), ("corpID", Value::Int(born.corp_id)),
        ("allianceID", if born.alliance_id > 0 { Value::Int(born.alliance_id) } else { Value::None }),
        ("warFactionID", Value::None), ("securityStatus", Value::Float(0.0)), ("bounty", Value::Int(0)),
    ]);
    let actual = destiny(leader_rx.try_recv().unwrap());
    let mut expected = vec![crate::space::event(stamp, "AddBalls", vec![Value::tuple([
        Value::str(stream), Value::list([slim]),
        Value::Dict(if born.free { vec![(Value::Int(born.id), born.damage_state())] } else { vec![] })])])];
    crate::space::system::tests::events::reuse_checked_damage_time(&actual[0], &mut expected[0], before..=crate::client::filetime_now());
    assert_eq!(actual, expected);
    assert_eq!(fleet_snapshot(&mut leader_rx), Some(vec![keyval([("targetTags", Value::Dict(vec![]))])]));
    client.session.apply(vec![("solarsystemid", Value::None)]);
    assert_eq!(rpc(&mut client, "beyonce", "GetFleetLocationAndInfo", &[]).await.unwrap(), Value::Dict(vec![]));
    client.session.apply(vec![("solarsystemid", Value::Int(system))]);
    assert_eq!(rpc(&mut client, "beyonce", "GetFleetState", &[]).await.unwrap(), empty);
    assert!(rpc(&mut client, "beyonce", "CmdFleetTagTarget", &[Value::Int(11), Value::str("1")]).await.is_err());
    assert!(rpc(&mut client, "beyonce", "CmdFleetTagTarget", &[Value::Int(14), Value::str("invalid")]).await.is_err());
    rpc(&mut client, "beyonce", "CmdFleetTagTarget", &[Value::Int(14), Value::str("1")]).await.unwrap();
    let expected = keyval([("targetTags", Value::Dict(vec![(Value::Int(14), Value::str("1"))]))]);
    assert_eq!(rpc(&mut client, "beyonce", "GetFleetState", &[]).await.unwrap(), expected);
    assert_eq!(fleet_snapshot(&mut leader_rx), Some(vec![expected]));
    rpc(&mut client, "beyonce", "CmdFleetTagTarget", &[Value::Int(15), Value::str("B")]).await.unwrap();
    fleet_snapshot(&mut leader_rx).unwrap();
    rpc(&mut client, "beyonce", "CmdFleetTagTarget", &[Value::Int(14), Value::None]).await.unwrap();
    assert_eq!(fleet_snapshot(&mut leader_rx), Some(vec![keyval([("targetTags", Value::Dict(vec![(Value::Int(15), Value::str("B")), (Value::Int(14), Value::None)]))])]));
    assert_eq!(rpc(&mut client, "beyonce", "GetFleetState", &[]).await.unwrap(), keyval([("targetTags", Value::Dict(vec![(Value::Int(15), Value::str("B"))]))]));
    rpc(&mut client, "beyonce", "CmdFleetTagTarget", &[Value::Int(15), Value::None]).await.unwrap();
    assert_eq!(fleet_snapshot(&mut leader_rx), Some(vec![keyval([("targetTags", Value::Dict(vec![(Value::Int(15), Value::None)]))])]));
    rpc(&mut client, "beyonce", "UpdateStateRequest", &[]).await.unwrap();
    assert_eq!(fleet_snapshots(&mut client), vec![vec![empty.clone()]]);
    rpc(&mut client, "beyonce", "CmdFleetTagTarget", &[Value::Int(14), Value::str("A")]).await.unwrap();
    rpc(&mut client, "beyonce", "UpdateStateRequest", &[]).await.unwrap();
    let tag_a = keyval([("targetTags", Value::Dict(vec![(Value::Int(14), Value::str("A"))]))]);
    assert_eq!(fleet_snapshots(&mut client), vec![vec![tag_a.clone()]]);
    world.observe(system, 101, 11, leader_tx.clone()).await.unwrap();
    initial_state(&mut leader_rx);
    assert_eq!(fleet_snapshot(&mut leader_rx), Some(vec![tag_a.clone()]));
    // One actor job binds the member, removes both tagged balls, re-enters 14
    // with a NEW tag, then rebinds the member: the saved clear must not erase
    // re-occupied authority, and the saved ticket is provably stale.
    let (member_tx2, mut member_rx2) = tokio::sync::mpsc::unbounded_channel();
    let before = crate::client::filetime_now();
    let (member_reobserved, mut member_completion, born_stamp, born14, born15) = world.with(system, move |s| {
        let (completed_1, mut completion_1) = tokio::sync::oneshot::channel();
        let _ = s.observe(102, 12, member_scene_tx.clone(), completed_1);
        crate::space::system::tests::events::publish_scene_prefix(s, crate::space::collision::enabled());
        assert_eq!(completion_1.try_recv(), Ok(true));
        { let _ = state().fleets.get_mut(&fleet_id).unwrap().target_tags.insert((system, 15), "A".into()); }
        let _ = s.remove_ball(14); let _ = s.remove_ball(15);
        let mut reenter = ball(14, None, 40.0); reenter.type_id = target_type; s.insert(reenter);
        let mut far = ball(15, None, space::MIN_CLOAKING_DISTANCE * 2.0); far.type_id = target_type; s.insert(far);
        { let _ = state().fleets.get_mut(&fleet_id).unwrap().target_tags.insert((system, 14), "C".into()); }
        let (completed_2, completion_2) = tokio::sync::oneshot::channel();
        let observed = s.observe(102, 12, member_tx2.clone(), completed_2);
        crate::space::system::tests::events::publish_scene_prefix(s, crate::space::collision::enabled());
        (observed, completion_2, s.current_time(), s.ball(14).unwrap().clone(), s.ball(15).unwrap().clone())
    }).await.unwrap();
    assert!(member_reobserved);
    assert!(world.after(system, crate::space::seconds_to_ms(1.0)).await);
    assert_eq!(member_completion.try_recv(), Ok(true));
    assert!(destiny(leader_rx.try_recv().unwrap()).iter().any(|e| matches!(&e.items().unwrap()[1].items().unwrap()[0], Value::Bytes(n) if n.as_slice() == b"RemoveBalls")));
    assert_eq!(fleet_snapshot(&mut leader_rx), Some(vec![keyval([("targetTags", Value::Dict(vec![(Value::Int(14), Value::str("C"))]))])]));
    assert!(destiny(leader_rx.try_recv().unwrap()).iter().any(|e| matches!(&e.items().unwrap()[1].items().unwrap()[0], Value::Bytes(n) if n.as_slice() == b"RemoveBalls")));
    assert_eq!(fleet_snapshot(&mut leader_rx), Some(vec![keyval([("targetTags", Value::Dict(vec![(Value::Int(14), Value::str("C")), (Value::Int(15), Value::None)]))])]));
    assert!(born14.slim.is_empty() && born15.slim.is_empty(), "re-entered balls carry no extra slim fields");
    let actual = destiny(leader_rx.try_recv().unwrap());
    let mut stream = vec![1u8]; // DESTINY_BALLS kind byte
    stream.extend_from_slice(&(born_stamp as i32).to_le_bytes());
    stream.extend_from_slice(&crate::space::system::tests::events::encoded_ball_for_test(&born14, crate::space::collision::enabled()));
    stream.extend_from_slice(&crate::space::system::tests::events::encoded_ball_for_test(&born15, crate::space::collision::enabled()));
    let slim = |ball: &space::Ball| Value::instance("eve.common.script.util.slimItem.SlimItem", vec![
        ("itemID", Value::Int(ball.id)), ("typeID", Value::Int(ball.type_id as i64)), ("ownerID", Value::Int(ball.owner_id))]);
    let mut expected = vec![crate::space::event(born_stamp, "AddBalls", vec![Value::tuple([
        Value::str(stream), Value::list([slim(&born14), slim(&born15)]),
        Value::Dict([&born14, &born15].iter().filter_map(|ball| ball.free.then(|| (Value::Int(ball.id), ball.damage_state()))).collect())])])];
    crate::space::system::tests::events::reuse_checked_damage_time(&actual[0], &mut expected[0], before..=crate::client::filetime_now());
    assert_eq!(actual, expected, "one merged Add returns both re-entered balls once");
    assert_eq!(fleet_snapshot(&mut leader_rx), Some(vec![keyval([("targetTags", Value::Dict(vec![(Value::Int(14), Value::str("C"))]))])]));
    initial_state(&mut member_rx2);
    assert_eq!(fleet_snapshot(&mut member_rx2), Some(vec![keyval([("targetTags", Value::Dict(vec![(Value::Int(14), Value::str("C"))]))])]));
    assert!(member_rx2.try_recv().is_err());
    { let _ = state().fleets.get_mut(&fleet_id).unwrap().target_tags.remove(&(system, 14)); }
    assert_eq!(rpc(&mut client, "beyonce", "GetFleetState", &[]).await.unwrap(), empty);
    assert_eq!(rpc(&mut client, "beyonce", "UnknownFleetMethod", &[]).await.unwrap(), Value::None);
    assert!(rpc(&mut client, "beyonce", "BookmarkLocation", &[Value::Int(999)]).await.is_err());
    client.session.apply(vec![("charid", Value::Int(outsider)), ("shipid", Value::Int(13))]);
    assert_eq!(rpc(&mut client, "beyonce", "GetFleetLocationAndInfo", &[]).await.unwrap(), Value::Dict(vec![]));
    assert_eq!(rpc(&mut client, "beyonce", "GetFleetState", &[]).await.unwrap(), empty);
    rpc(&mut client, "beyonce", "UpdateStateRequest", &[]).await.unwrap();
    assert_eq!(fleet_snapshots(&mut client), vec![vec![empty.clone()]]);
    assert!(rpc(&mut client, "beyonce", "CmdFleetTagTarget", &[Value::Int(15), Value::str("A")]).await.is_err());
    client.session.apply(vec![("charid", Value::Int(member)), ("shipid", Value::Int(12))]);
    rpc(&mut client, "beyonce", "UpdateStateRequest", &[]).await.unwrap();
    assert_eq!(fleet_snapshots(&mut client), vec![vec![empty.clone()]]);
    world.unobserve(101);
    assert!(rpc(&mut client, "beyonce", "CmdFleetTagTarget", &[Value::Int(15), Value::str("A")]).await.is_err());
    for role in [ROLE_WING_COMMANDER, ROLE_SQUAD_COMMANDER] {
        { let mut st = state(); st.fleets.get_mut(&fleet_id).unwrap().members.get_mut(&member).unwrap().role = role; }
        rpc(&mut client, "beyonce", "CmdFleetTagTarget", &[Value::Int(15), Value::str("A")]).await.unwrap();
        assert_eq!(rpc(&mut client, "beyonce", "GetFleetState", &[]).await.unwrap(), keyval([("targetTags", Value::Dict(vec![(Value::Int(15), Value::str("A"))]))]));
        assert_eq!(fleet_snapshot(&mut member_rx2), Some(vec![keyval([("targetTags", Value::Dict(vec![(Value::Int(15), Value::str("A"))]))])]));
        rpc(&mut client, "beyonce", "CmdFleetTagTarget", &[Value::Int(15), Value::None]).await.unwrap();
        assert_eq!(fleet_snapshot(&mut member_rx2), Some(vec![keyval([("targetTags", Value::Dict(vec![(Value::Int(15), Value::None)]))])]));
    }
    { let mut st = state(); st.fleets.get_mut(&fleet_id).unwrap().members.get_mut(&member).unwrap().role = ROLE_MEMBER; }
    assert_eq!(rpc(&mut client, "beyonce", "GetFleetState", &[]).await.unwrap(), empty);
    assert!(rpc(&mut client, "beyonce", "CmdFleetTagTarget", &[Value::Int(15), Value::str("A")]).await.is_err());
    client.session.apply(vec![("charid", Value::Int(leader)), ("shipid", Value::Int(11))]);
    rpc(&mut client, "beyonce", "UpdateStateRequest", &[]).await.unwrap();
    assert_eq!(fleet_snapshots(&mut client), vec![vec![empty.clone()]]);
    world.observe(system, 101, 11, leader_tx.clone()).await.unwrap();
    initial_state(&mut leader_rx);
    assert_eq!(fleet_snapshot(&mut leader_rx), Some(vec![empty.clone()]));
    rpc(&mut client, "beyonce", "CmdFleetTagTarget", &[Value::Int(15), Value::str("A")]).await.unwrap();
    fleet_snapshot(&mut leader_rx).unwrap();
    world.with(system, |s| assert!(s.begin_module_cloak(15, -1, Arc::new(std::sync::atomic::AtomicBool::new(false)), 0, 0, |_| true, None))).await.unwrap();
    assert!(world.after(system, crate::space::seconds_to_ms(1.0)).await);
    assert!(destiny(leader_rx.try_recv().unwrap()).iter().any(|e| matches!(&e.items().unwrap()[1].items().unwrap()[0], Value::Bytes(n) if n.as_slice() == b"CloakBall")));
    assert_eq!(fleet_snapshot(&mut leader_rx), Some(vec![keyval([("targetTags", Value::Dict(vec![(Value::Int(15), Value::None)]))])]));
    { let st = state(); assert_eq!(st.fleets[&fleet_id].target_tags[&(system, 15)].as_str(), "A"); }
    world.with(system, |s| assert!(s.uncloak(15))).await.unwrap();
    assert!(world.after(system, crate::space::seconds_to_ms(1.0)).await);
    assert!(destiny(leader_rx.try_recv().unwrap()).iter().any(|e| matches!(&e.items().unwrap()[1].items().unwrap()[0], Value::Bytes(n) if n.as_slice() == b"AddBalls")));
    assert_eq!(fleet_snapshot(&mut leader_rx), Some(vec![keyval([("targetTags", Value::Dict(vec![(Value::Int(15), Value::str("A"))]))])]));
    world.set_online_session(leader, Some(infinity_core::session::OnlineSession {
        character_id: leader, user_id: None, session_id: 1, ship_id: Some(11), station_id: None,
        system_id: Some(system), corporation_id: None, alliance_id: None, faction_id: None,
        fleet_id: Some(fleet_id), role: 0, corporation_role: 0,
    }));
    lifecycle::leave(&world, leader);
    world.with(system, |_| ()).await.unwrap();
    assert_eq!(fleet_snapshot(&mut leader_rx), Some(vec![keyval([("targetTags", Value::Dict(vec![(Value::Int(15), Value::None)]))])]));
    while let Ok((name, _)) = notices.try_recv() { assert_ne!(name, "OnFleetStateChange"); }
    { let mut st = state(); st.fleets.remove(&fleet_id); st.of.remove(&leader); st.of.remove(&member); }
    world.unregister_pilot_generation(leader, None);
    world.unregister_pilot_generation(member, None);
    world.unregister_pilot_generation(outsider, None);
}
