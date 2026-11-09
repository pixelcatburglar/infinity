use super::*;
use crate::space;
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
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let (member_tx, mut member_notices) = tokio::sync::mpsc::unbounded_channel();
    world.register_pilot(member, member_tx);
    let (outsider_tx, mut outsider_notices) = tokio::sync::mpsc::unbounded_channel();
    world.register_pilot(outsider, outsider_tx);
    world.observe(system, 101, 11, tx).await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let _peer = tokio::net::TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
    let (stream, address) = listener.accept().await.unwrap();
    let cfg = Arc::new(crate::config::Config::default());
    let chat = crate::chat::Hub::new(&cfg.chat.domain, db.clone(), world.clone());
    let login = crate::handshake::Login { user_id: 1, user_name: "test".into(), role: 0, client_id: 101, session_id: 1, language_id: "EN".into() };
    let mut client = Client::new(crate::conn::Conn::new(stream, address), cfg, db, sde, chat, world.clone(), login);
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
    let other_system = system - 1;
    world.system(other_system).await.unwrap();
    world.with(other_system, move |s| { s.insert(ball(12, Some(member), 20.0)); }).await.unwrap();
    let Value::Dict(absent) = rpc(&mut client, "beyonce", "GetFleetLocationAndInfo", &[]).await.unwrap() else { panic!("fleet positions must be a dictionary") };
    assert_eq!(absent.len(), 1);
    world.remove_ball(other_system, 12).await.unwrap();
    world.with(system, move |s| { s.insert(ball(12, Some(member), 20.0)); }).await.unwrap();
    client.session.apply(vec![("solarsystemid", Value::None)]);
    assert_eq!(rpc(&mut client, "beyonce", "GetFleetLocationAndInfo", &[]).await.unwrap(), Value::Dict(vec![]));
    client.session.apply(vec![("solarsystemid", Value::Int(system))]);
    let empty = keyval([("targetTags", Value::Dict(vec![]))]);
    assert_eq!(rpc(&mut client, "beyonce", "GetFleetState", &[]).await.unwrap(), empty);
    assert!(rpc(&mut client, "beyonce", "CmdFleetTagTarget", &[Value::Int(11), Value::str("1")]).await.is_err());
    assert!(rpc(&mut client, "beyonce", "CmdFleetTagTarget", &[Value::Int(14), Value::str("invalid")]).await.is_err());
    rpc(&mut client, "beyonce", "CmdFleetTagTarget", &[Value::Int(14), Value::str("1")]).await.unwrap();
    let expected = keyval([("targetTags", Value::Dict(vec![(Value::Int(14), Value::str("1"))]))]);
    assert_eq!(rpc(&mut client, "beyonce", "GetFleetState", &[]).await.unwrap(), expected);
    assert_eq!(notices.try_recv().unwrap(), ("OnFleetStateChange".to_string(), vec![expected]));
    rpc(&mut client, "beyonce", "CmdFleetTagTarget", &[Value::Int(15), Value::str("B")]).await.unwrap();
    notices.try_recv().unwrap();
    rpc(&mut client, "beyonce", "CmdFleetTagTarget", &[Value::Int(14), Value::None]).await.unwrap();
    assert_eq!(notices.try_recv().unwrap(), ("OnFleetStateChange".to_string(), vec![keyval([("targetTags", Value::Dict(vec![(Value::Int(15), Value::str("B")), (Value::Int(14), Value::None)]))])]));
    assert_eq!(rpc(&mut client, "beyonce", "GetFleetState", &[]).await.unwrap(), keyval([("targetTags", Value::Dict(vec![(Value::Int(15), Value::str("B"))]))]));
    rpc(&mut client, "beyonce", "CmdFleetTagTarget", &[Value::Int(15), Value::None]).await.unwrap();
    assert_eq!(notices.try_recv().unwrap(), ("OnFleetStateChange".to_string(), vec![keyval([("targetTags", Value::Dict(vec![(Value::Int(15), Value::None)]))])]));
    rpc(&mut client, "beyonce", "UpdateStateRequest", &[]).await.unwrap();
    world.with(system, |_| ()).await.unwrap();
    assert_eq!(notices.try_recv().unwrap(), ("OnFleetStateChange".to_string(), vec![empty.clone()]));
    rpc(&mut client, "beyonce", "CmdFleetTagTarget", &[Value::Int(14), Value::str("A")]).await.unwrap();
    notices.try_recv().unwrap();
    rpc(&mut client, "beyonce", "UpdateStateRequest", &[]).await.unwrap();
    world.with(system, |_| ()).await.unwrap();
    assert_eq!(notices.try_recv().unwrap(), ("OnFleetStateChange".to_string(), vec![keyval([("targetTags", Value::Dict(vec![(Value::Int(14), Value::str("A"))]))])]));
    world.remove_ball(system, 14).await.unwrap();
    assert_eq!(notices.try_recv().unwrap(), ("OnFleetStateChange".to_string(), vec![keyval([("targetTags", Value::Dict(vec![(Value::Int(14), Value::None)]))])]));
    assert_eq!(rpc(&mut client, "beyonce", "GetFleetState", &[]).await.unwrap(), empty);
    assert_eq!(rpc(&mut client, "beyonce", "UnknownFleetMethod", &[]).await.unwrap(), Value::None);
    assert!(rpc(&mut client, "beyonce", "BookmarkLocation", &[Value::Int(999)]).await.is_err());
    client.session.apply(vec![("charid", Value::Int(outsider)), ("shipid", Value::Int(13))]);
    assert_eq!(rpc(&mut client, "beyonce", "GetFleetLocationAndInfo", &[]).await.unwrap(), Value::Dict(vec![]));
    assert_eq!(rpc(&mut client, "beyonce", "GetFleetState", &[]).await.unwrap(), empty);
    rpc(&mut client, "beyonce", "UpdateStateRequest", &[]).await.unwrap();
    world.with(system, |_| ()).await.unwrap();
    assert_eq!(outsider_notices.try_recv().unwrap(), ("OnFleetStateChange".to_string(), vec![empty.clone()]));
    assert!(rpc(&mut client, "beyonce", "CmdFleetTagTarget", &[Value::Int(15), Value::str("A")]).await.is_err());
    client.session.apply(vec![("charid", Value::Int(member)), ("shipid", Value::Int(12))]);
    rpc(&mut client, "beyonce", "UpdateStateRequest", &[]).await.unwrap();
    world.with(system, |_| ()).await.unwrap();
    assert_eq!(member_notices.try_recv().unwrap(), ("OnFleetStateChange".to_string(), vec![empty.clone()]));
    assert!(rpc(&mut client, "beyonce", "CmdFleetTagTarget", &[Value::Int(15), Value::str("A")]).await.is_err());
    for role in [ROLE_WING_COMMANDER, ROLE_SQUAD_COMMANDER] {
        { let mut st = state(); st.fleets.get_mut(&fleet_id).unwrap().members.get_mut(&member).unwrap().role = role; }
        rpc(&mut client, "beyonce", "CmdFleetTagTarget", &[Value::Int(15), Value::str("A")]).await.unwrap();
        assert_eq!(rpc(&mut client, "beyonce", "GetFleetState", &[]).await.unwrap(), keyval([("targetTags", Value::Dict(vec![(Value::Int(15), Value::str("A"))]))]));
        assert_eq!(member_notices.try_recv().unwrap(), ("OnFleetStateChange".to_string(), vec![keyval([("targetTags", Value::Dict(vec![(Value::Int(15), Value::str("A"))]))])]));
        rpc(&mut client, "beyonce", "CmdFleetTagTarget", &[Value::Int(15), Value::None]).await.unwrap();
        assert_eq!(member_notices.try_recv().unwrap(), ("OnFleetStateChange".to_string(), vec![keyval([("targetTags", Value::Dict(vec![(Value::Int(15), Value::None)]))])]));
    }
    { let mut st = state(); st.fleets.get_mut(&fleet_id).unwrap().members.get_mut(&member).unwrap().role = ROLE_MEMBER; }
    assert_eq!(rpc(&mut client, "beyonce", "GetFleetState", &[]).await.unwrap(), empty);
    assert!(rpc(&mut client, "beyonce", "CmdFleetTagTarget", &[Value::Int(15), Value::str("A")]).await.is_err());
    client.session.apply(vec![("charid", Value::Int(leader)), ("shipid", Value::Int(11))]);
    rpc(&mut client, "beyonce", "UpdateStateRequest", &[]).await.unwrap();
    world.with(system, |_| ()).await.unwrap();
    assert_eq!(notices.try_recv().unwrap(), ("OnFleetStateChange".to_string(), vec![empty.clone()]));
    rpc(&mut client, "beyonce", "CmdFleetTagTarget", &[Value::Int(15), Value::str("A")]).await.unwrap();
    notices.try_recv().unwrap();
    world.set_online_session(leader, Some(infinity_core::session::OnlineSession {
        character_id: leader, user_id: None, session_id: 1, ship_id: Some(11), station_id: None,
        system_id: Some(system), corporation_id: None, alliance_id: None, faction_id: None,
        fleet_id: Some(fleet_id), role: 0, corporation_role: 0,
    }));
    lifecycle::leave(&world, leader);
    world.with(system, |_| ()).await.unwrap();
    let mut clear = false;
    while let Ok((name, args)) = notices.try_recv() {
        if name == "OnFleetStateChange" {
            assert_eq!(args, vec![keyval([("targetTags", Value::Dict(vec![(Value::Int(15), Value::None)]))])]);
            clear = true;
        }
    }
    assert!(clear);
    { let mut st = state(); st.fleets.remove(&fleet_id); st.of.remove(&leader); st.of.remove(&member); }
    world.unregister_pilot_generation(leader, None);
    world.unregister_pilot_generation(member, None);
    world.unregister_pilot_generation(outsider, None);
}
