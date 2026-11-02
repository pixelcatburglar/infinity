use super::*;
use crate::space::{MODE_GOTO, MODE_STOP, MODE_WARP};

fn ship() -> Ball {
    let mut b = Ball {
        id: 123, type_id: 456, owner_id: 789, corp_id: 12, alliance_id: 34,
        char_id: None, mode: MODE_GOTO, radius: 50.0, signature_radius: None,
        mass: 1_000_000.0, max_vel: 200.0, agility: 3.0, speed_fraction: 0.0,
        pos: [10.0, 20.0, 30.0], vel: [4.0, 5.0, 6.0],
        old_pos: [1.0, 2.0, 3.0], old_vel: [7.0, 8.0, 9.0],
        segment_acceleration: Some([11.0, 12.0, 13.0]),
        goto: [0.0; 3], follow_id: 0, follow_range: 0.0,
        free: true, global: false, massive: false, interactive: false,
        warp: crate::space::WarpState::default(),
        warp_factor: 3000, last_ms: 1000, interpolation_anchor_ms: 500, effect_stamp: 42,
        cloak: 0, cloak_until: 0, health: [1.0; 3], launcher: 0, slim: vec![],
    };
    b.warp.length = 20.0;
    b.warp.min_range = 10.0;
    b.warp.factor = 3;
    b
}

#[test]
fn property_setters_preserve_retained_warp_metadata() {
    let mut b = ship();
    b.mode = MODE_WARP;
    b.set_speed_fraction(0.5);
    b.set_max_speed(250.0);
    b.set_ball_mass(2_000_000.0);
    assert_eq!(b.mode, MODE_WARP);
    assert!(b.is_warping());
    assert_eq!((b.warp.length, b.warp.min_range, b.warp.factor), (20.0, 10.0, 3));
    assert_eq!(b.warp_factor, 3000);
    assert_eq!(b.effect_stamp, 42);
    assert_eq!(b.segment_acceleration, Some([11.0, 12.0, 13.0]));
}

#[test]
fn short_warp_uses_raw_point_and_preserves_motion_state() {
    let point = [100_009.0, 20.0, 30.0];
    for (object, min_range, throttle) in [
        (None, -5000.0, 0.0),
        (None, 5000.0, 0.4),
        (Some((point, 10_000.0)), 5000.0, 0.4),
    ] {
        let mut b = ship();
        b.speed_fraction = throttle;
        let events = warp_command(&mut b, 77, object, point, min_range);
        assert_eq!(b.mode, MODE_GOTO);
        assert_eq!(b.goto, point);
        assert!(!b.is_warping());
        assert_eq!((b.warp.length, b.warp.min_range, b.warp.factor), (20.0, 10.0, 3));
        assert_eq!(b.speed_fraction, if throttle == 0.0 { 1.0 } else { throttle });
        assert_eq!(b.pos, [10.0, 20.0, 30.0]);
        assert_eq!(b.vel, [4.0, 5.0, 6.0]);
        assert_eq!(b.old_pos, [1.0, 2.0, 3.0]);
        assert_eq!(b.old_vel, [7.0, 8.0, 9.0]);
        assert_eq!(b.segment_acceleration, Some([11.0, 12.0, 13.0]));
        assert_eq!(b.last_ms, 1000);
        assert_eq!(b.interpolation_anchor_ms, 500);
        assert_eq!(b.effect_stamp, 42);
        assert_eq!(events, vec![event(77, "GotoPoint", vec![Value::Int(123), Value::Float(point[0]), Value::Float(point[1]), Value::Float(point[2])])]);
    }
}

#[test]
fn warp_threshold_uses_unadjusted_distance_and_keeps_long_warp_events() {
    for (distance, min_range) in [(100_000.0, 0.0), (100_001.0, 5000.0)] {
        let mut b = ship();
        let point = [b.pos[0] + distance, b.pos[1], b.pos[2]];
        let events = warp_command(&mut b, 77, None, point, min_range);
        assert_eq!(b.mode, MODE_WARP);
        assert_eq!((b.warp.length, b.warp.min_range, b.warp.factor), (20.0, min_range, 3000));
        assert_eq!(b.goto, point);
        assert_eq!(events, vec![
            event(77, "WarpTo", vec![Value::Int(123), Value::Float(point[0]), Value::Float(point[1]), Value::Float(point[2]), Value::Float(min_range), Value::Int(3000)]),
            event(77, "SetSpeedFraction", vec![Value::Int(123), Value::Float(1.0)]),
            special_fx(77, 123, None, None, "effects.Warping", 1, 0, SPECIAL_FX_UNSPECIFIED_DURATION, None),
        ]);
    }
}

#[test]
fn stale_warp_snapshot_has_no_side_effects_and_can_retry() {
    crate::tests::init_config();
    let mut s = space::System::new(0, std::sync::Arc::default(), 12_345);
    let mut b = ship();
    b.cloak = 1;
    s.insert(b);
    let old = s.projection_revision(123);
    s.refresh_projection_target(123);
    let point = [200_000.0, 20.0, 30.0];
    assert!(!commit_warp(&mut s, 123, old, false, None, point, 0.0, None).0);
    assert_eq!(s.ball(123).unwrap().cloak, 1);
    s.tick(13_000);
    assert_ne!(s.ball(123).unwrap().mode, MODE_WARP);
    let current = s.projection_revision(123);
    assert!(commit_warp(&mut s, 123, current, true, None, point, 0.0, None).0);
    assert_eq!(s.ball(123).unwrap().cloak, 1);
    s.refresh_projection_target(123);
    assert!(!commit_warp(&mut s, 123, current, true, None, point, 0.0, None).0);
    let current = s.projection_revision(123);
    assert!(commit_warp(&mut s, 123, current, false, None, point, 0.0, None).0);
    s.tick(14_000);
    assert_eq!(s.ball(123).unwrap().mode, MODE_WARP);
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn docking_checks_surface_range_before_accepting() {
    use std::sync::Arc;
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let world = space::World::new(db.clone(), sde.clone(), 1);
    let system = -1;
    world.system(system).await.unwrap();
    let cfg = Arc::new(crate::config::Config::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut peer = tokio::net::TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
    let (stream, address) = listener.accept().await.unwrap();
    let login = crate::handshake::Login { user_id: 1, user_name: "test".into(), role: 0, client_id: 1, session_id: 1, language_id: "EN".into() };
    let chat = crate::chat::Hub::new(&cfg.chat.domain, db.clone(), world.clone());
    let mut client = Client::new(crate::conn::Conn::new(stream, address), cfg, db, sde, chat, world.clone(), login);
    client.session.apply(vec![("solarsystemid", Value::Int(system)), ("shipid", Value::Int(123))]);
    let mut vessel = ship();
    vessel.mode = MODE_STOP;
    vessel.pos = [0.0; 3];
    vessel.old_pos = vessel.pos;
    vessel.vel = [0.0; 3];
    vessel.old_vel = vessel.vel;
    vessel.segment_acceleration = None;
    let mut station = vessel.clone();
    station.id = 456;
    station.radius = 1000.0;
    station.pos = [3551.0, 0.0, 0.0];
    station.old_pos = station.pos;
    world.with(system, move |s| { s.insert(vessel); s.insert(station); }).await.unwrap();
    for target in [999, 456] {
        let args = [Value::Int(target)];
        let call = Call { service: "beyonce", method: "CmdDock", args: &args, kwargs: &Value::None, bound: false, bind_params: None };
        assert!(matches!(crate::services::dispatch(&mut client, &call).await, Err(CallError::User { msg, .. }) if msg == messages::MSG_DOCKING_REQUEST_DENIED));
    }
    world.with(system, |s| {
        let mut station = s.ball(456).unwrap().clone();
        station.pos = [3550.0, 0.0, 0.0];
        station.old_pos = station.pos;
        s.insert(station);
    }).await.unwrap();
    let args = [Value::Int(456)];
    let call = Call { service: "beyonce", method: "CmdDock", args: &args, kwargs: &Value::None, bound: false, bind_params: None };
    let docking = crate::services::dispatch(&mut client, &call);
    tokio::pin!(docking);
    let frame = tokio::select! {
        _ = &mut docking => panic!("docking finalized before acceptance delay"),
        frame = tokio::time::timeout(std::time::Duration::from_secs(2), crate::framing::read_frame(&mut peer)) => frame.unwrap().unwrap().unwrap(),
    };
    let packet = crate::macho::Packet::from_value(&marshal::loads(&frame.payload).unwrap()).unwrap();
    assert!(matches!(packet.destination, crate::macho::Address::Broadcast { broadcast_id, .. } if broadcast_id.as_text().as_deref() == Some("OnDockingAccepted")));
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn completed_jumps_record_only_destination_arrivals() {
    use std::sync::Arc;
    use crate::dao::map_telemetry::{self, EventKind, EventSource, HistoryStat};
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::deployment::DatabaseConfig { schema_on_start: false, ..crate::config::tests::test_database() }).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let systems: Vec<i64> = sde.map.systems().filter(|s| s.constellation_id.is_some() && s.region_id.is_some()).take(2).map(|s| s.solar_system_id).collect();
    let (origin, destination) = (systems[0], systems[1]);
    let account: i64 = db.query_one("INSERT INTO game.accounts (user_name,password_hash,role) VALUES (nextval('game.accounts_user_id_seq')::text,''::bytea,0) RETURNING user_id", &[]).await.unwrap().get("user_id");
    let character: i64 = db.query_one("INSERT INTO game.characters (user_id,name,race_id,bloodline_id,gender_id,corporation_id) VALUES ($1,nextval('game.characters_character_id_seq')::text,1,1,1,1) RETURNING character_id", &[&account]).await.unwrap().get("character_id");
    let hull = crate::dao::items::creation::positioned(&db, TYPE_CAPSULE, character, origin, [0.0; 3]).await.unwrap();
    let marker = map_telemetry::next_occurrence(&db).await.unwrap();
    let start = crate::client::filetime_now();
    let world = space::World::new(db.clone(), sde.clone(), 1);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let _peer = tokio::net::TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
    let (stream, address) = listener.accept().await.unwrap();
    let cfg = Arc::new(crate::config::Config::default());
    let chat = crate::chat::Hub::new(&cfg.chat.domain, db.clone(), world.clone());
    let login = crate::handshake::Login { user_id: account, user_name: account.to_string(), role: 0, client_id: character, session_id: character, language_id: "EN".into() };
    let mut client = Client::new(crate::conn::Conn::new(stream, address), cfg, db.clone(), sde, chat, world.clone(), login);
    client.session.apply(vec![("charid", Value::Int(character)), ("corpid", Value::Int(1)), ("shipid", Value::Int(hull)), ("solarsystemid", Value::Int(origin))]);
    let mut vessel = ship();
    vessel.id = hull;
    vessel.type_id = TYPE_CAPSULE;
    vessel.owner_id = character;
    vessel.char_id = Some(character);
    world.add_ship(origin, vessel).await.unwrap();
    let w = world.clone();
    let d = db.clone();
    let result = tokio::spawn(async move {
        let jump = |dest_system| Jump { through: 1, guid: "effects.JumpOut", other_type: None, dest_system, pos: [0.0; 3] };
        assert!(jump_out_and_arrive(&mut client, origin, hull, character, jump(-1)).await.is_err());
        w.system(destination).await.unwrap();
        let mut blocker = ship();
        blocker.id = -77;
        blocker.char_id = Some(-77);
        w.add_ship(destination, blocker).await.unwrap();
        assert!(matches!(jump_out_and_arrive(&mut client, origin, hull, character, jump(destination)).await, Err(CallError::User { msg, .. }) if msg == "SystemCheck_JumpFailed_Queued"));
        assert!(map_telemetry::visits(&d, character).await.unwrap().is_empty());
        assert!(d.query("SELECT source_id FROM game.map_events WHERE source=$1 AND source_id>$2", &[&(EventSource::JumpOccurrence as i16), &marker]).await.unwrap().is_empty());
        w.remove_ball(destination, -77).await;
        jump_out_and_arrive(&mut client, origin, hull, character, jump(destination)).await.unwrap();
        let visits = map_telemetry::visits(&d, character).await.unwrap();
        assert_eq!(visits.len(), 1);
        assert_eq!((visits[0].solar_system_id, visits[0].visits), (destination, 1));
        let history = map_telemetry::history(&d, HistoryStat::Jumps, start, crate::client::filetime_now()).await.unwrap();
        assert_eq!(history.iter().find(|row| row.solar_system_id == destination).map(|row| row.value1), Some(1));
        assert!(history.iter().all(|row| row.solar_system_id != origin));
        let events = d.query("SELECT solar_system_id,kind FROM game.map_events WHERE source=$1 AND source_id>$2", &[&(EventSource::JumpOccurrence as i16), &marker]).await.unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!((events[0].get::<_, i64>("solar_system_id"), events[0].get::<_, i16>("kind")), (destination, EventKind::Jump as i16));
        teleport(&mut client, origin, [0.0; 3]).await.unwrap();
        assert_eq!(map_telemetry::visits(&d, character).await.unwrap()[0].visits, 1);
        assert_eq!(d.query("SELECT source_id FROM game.map_events WHERE source=$1 AND source_id>$2", &[&(EventSource::JumpOccurrence as i16), &marker]).await.unwrap().len(), 1);
        jump_out_and_arrive(&mut client, origin, hull, character, jump(destination)).await.unwrap();
        let visits = map_telemetry::visits(&d, character).await.unwrap();
        assert_eq!(visits.len(), 1);
        assert_eq!((visits[0].solar_system_id, visits[0].visits), (destination, 2));
        let events = d.query("SELECT solar_system_id FROM game.map_events WHERE source=$1 AND source_id>$2", &[&(EventSource::JumpOccurrence as i16), &marker]).await.unwrap();
        assert_eq!(events.len(), 2);
        assert!(events.iter().all(|row| row.get::<_, i64>("solar_system_id") == destination));
        let history = map_telemetry::history(&d, HistoryStat::Jumps, start, crate::client::filetime_now()).await.unwrap();
        assert_eq!(history.iter().find(|row| row.solar_system_id == destination).map(|row| row.value1), Some(2));
        assert!(history.iter().all(|row| row.solar_system_id != origin));
    }).await;
    world.remove_ball(origin, hull).await;
    world.remove_ball(destination, hull).await;
    db.execute("DELETE FROM game.map_events WHERE source=$1 AND source_id>$2", &[&(EventSource::JumpOccurrence as i16), &marker]).await.unwrap();
    db.execute("DELETE FROM game.map_visits WHERE character_id=$1", &[&character]).await.unwrap();
    db.execute("DELETE FROM game.item_attributes WHERE item_id=$1", &[&hull]).await.unwrap();
    db.execute("DELETE FROM game.items WHERE item_id=$1", &[&hull]).await.unwrap();
    db.execute("DELETE FROM game.characters WHERE character_id=$1", &[&character]).await.unwrap();
    db.execute("DELETE FROM game.accounts WHERE user_id=$1", &[&account]).await.unwrap();
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn headless_docking_returns_session_and_event() {
    use std::sync::Arc;
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let system = -1;
    let station_id = -2;
    let account: i64 = db.query_one("INSERT INTO game.accounts (user_name,password_hash,role) VALUES (nextval('game.accounts_user_id_seq')::text,''::bytea,0) RETURNING user_id", &[]).await.unwrap().get("user_id");
    let character: i64 = db.query_one("INSERT INTO game.characters (user_id,name,race_id,bloodline_id,gender_id,corporation_id) VALUES ($1,nextval('game.characters_character_id_seq')::text,1,1,1,1) RETURNING character_id", &[&account]).await.unwrap().get("character_id");
    let hull = crate::dao::items::creation::positioned(&db, TYPE_CAPSULE, character, system, [0.0; 3]).await.unwrap();
    let world = space::World::new(db.clone(), sde.clone(), 1);
    world.system(system).await.unwrap();
    let w = world.clone();
    let d = db.clone();
    let result = tokio::spawn(async move {
        let mut vessel = ship();
        vessel.id = hull;
        vessel.type_id = TYPE_CAPSULE;
        vessel.owner_id = character;
        vessel.char_id = Some(character);
        vessel.mode = MODE_STOP;
        vessel.pos = [0.0; 3];
        vessel.old_pos = vessel.pos;
        vessel.vel = [0.0; 3];
        vessel.old_vel = vessel.vel;
        vessel.segment_acceleration = None;
        let mut station = vessel.clone();
        station.id = station_id;
        station.char_id = None;
        station.radius = 1000.0;
        station.pos = [3551.0, 0.0, 0.0];
        station.old_pos = station.pos;
        w.with(system, move |s| { s.insert(vessel); s.insert(station); }).await.unwrap();
        let identity = Identity { char_id: character, ship_id: Some(hull), corp_id: 0, station_id: None, system_id: None };
        let missing_ship = Identity { char_id: character, ship_id: None, corp_id: 0, station_id: None, system_id: None };
        for (pilot, space_system) in [(&identity, None), (&missing_ship, Some(system)), (&identity, Some(system))] {
            assert!(matches!(dock(pilot, &d, &w, space_system, station_id, None::<&mut Client>).await,
                Err(CallError::User { msg, .. }) if msg == messages::MSG_DOCKING_REQUEST_DENIED));
        }
        let row = d.query_one("SELECT location_id FROM game.items WHERE item_id=$1", &[&hull]).await.unwrap();
        assert_eq!(row.get::<_, i64>("location_id"), system);
        w.with(system, move |s| {
            let mut station = s.ball(station_id).unwrap().clone();
            station.pos = [3550.0, 0.0, 0.0];
            station.old_pos = station.pos;
            s.insert(station);
        }).await.unwrap();
        let change = dock(&identity, &d, &w, Some(system), station_id, None::<&mut Client>).await.ok().unwrap();
        assert_eq!(change.updates, vec![("solarsystemid", Value::None), ("stationid", Value::Int(station_id))]);
        assert!(matches!(change.event, Some(GameEvent::Docked { char_id, ship_id, station_id: st, system_id }) if (char_id, ship_id, st, system_id) == (character, hull, station_id, system)));
        let row = d.query_one("SELECT location_id, flag_id FROM game.items WHERE item_id=$1", &[&hull]).await.unwrap();
        assert_eq!((row.get::<_, i64>("location_id"), row.get::<_, i32>("flag_id")), (station_id, FLAG_HANGAR));
        let row = d.query_one("SELECT station_id, solar_system_id FROM game.characters WHERE character_id=$1", &[&character]).await.unwrap();
        assert_eq!((row.get::<_, Option<i64>>("station_id"), row.get::<_, i64>("solar_system_id")), (Some(station_id), system));
        item_dao::position(&d, hull, system, [0.0; 3]).await.unwrap();
        let mut vessel = ship();
        vessel.id = hull;
        vessel.type_id = TYPE_CAPSULE;
        vessel.owner_id = character;
        vessel.char_id = Some(character);
        vessel.mode = MODE_STOP;
        vessel.pos = [0.0; 3];
        vessel.old_pos = vessel.pos;
        vessel.vel = [0.0; 3];
        vessel.old_vel = vessel.vel;
        vessel.segment_acceleration = None;
        w.with(system, move |s| s.insert(vessel)).await.unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut peer = tokio::net::TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
        let (stream, address) = listener.accept().await.unwrap();
        let cfg = Arc::new(crate::config::Config::default());
        let chat = crate::chat::Hub::new(&cfg.chat.domain, d.clone(), w.clone());
        let login = crate::handshake::Login { user_id: account, user_name: account.to_string(), role: 0, client_id: character, session_id: character, language_id: "EN".into() };
        let mut client = Client::new(crate::conn::Conn::new(stream, address), cfg, d.clone(), sde, chat, w.clone(), login);
        client.session.apply(vec![("charid", Value::Int(character)), ("shipid", Value::Int(hull)), ("solarsystemid", Value::Int(system))]);
        let args = [Value::Int(station_id)];
        let call = Call { service: "beyonce", method: "CmdDock", args: &args, kwargs: &Value::None, bound: false, bind_params: None };
        {
            let docking = crate::services::dispatch(&mut client, &call);
            tokio::pin!(docking);
            let frame = tokio::select! {
                _ = &mut docking => panic!("docking finalized before acceptance delay"),
                frame = tokio::time::timeout(std::time::Duration::from_secs(2), crate::framing::read_frame(&mut peer)) => frame.unwrap().unwrap().unwrap(),
            };
            let packet = crate::macho::Packet::from_value(&marshal::loads(&frame.payload).unwrap()).unwrap();
            assert!(matches!(packet.destination, crate::macho::Address::Broadcast { broadcast_id, .. } if broadcast_id.as_text().as_deref() == Some("OnDockingAccepted")));
            assert!(docking.await.is_ok());
        }
        assert_eq!(client.session.int("solarsystemid"), None);
        assert_eq!(client.session.int("stationid"), Some(station_id));
        assert!(client.outbox.iter().any(|packet| packet.command == crate::macho::cmd::SESSIONCHANGENOTIFICATION));
        item_dao::position(&d, hull, system, [0.0; 3]).await.unwrap();
        let mut vessel = ship();
        vessel.id = hull;
        vessel.type_id = TYPE_CAPSULE;
        vessel.mode = MODE_STOP;
        vessel.pos = [0.0; 3];
        vessel.old_pos = vessel.pos;
        vessel.vel = [0.0; 3];
        vessel.old_vel = vessel.vel;
        vessel.segment_acceleration = None;
        w.with(system, move |s| s.insert(vessel)).await.unwrap();
        let no_character = Identity { char_id: 0, ship_id: Some(hull), corp_id: 0, station_id: None, system_id: None };
        let change = dock(&no_character, &d, &w, Some(system), station_id, None::<&mut Client>).await.ok().unwrap();
        assert!(change.event.is_none());
    }).await;
    world.remove_ball(system, hull).await;
    db.execute("DELETE FROM game.item_attributes WHERE item_id=$1", &[&hull]).await.unwrap();
    db.execute("DELETE FROM game.items WHERE item_id=$1", &[&hull]).await.unwrap();
    db.execute("DELETE FROM game.characters WHERE character_id=$1", &[&character]).await.unwrap();
    db.execute("DELETE FROM game.accounts WHERE user_id=$1", &[&account]).await.unwrap();
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn undock_headless_and_rpc_preserve_departure_and_economy_lock() {
    use std::{future::{poll_fn, Future}, sync::Arc, task::Poll};
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let station = sde.map.stations().find(|s| s.solar_system_id.is_some() && sde.map.celestial(s.station_id)
        .is_some_and(|c| c.x.is_some() && c.y.is_some() && c.z.is_some())).expect("station celestial");
    let (station_id, system) = (station.station_id, station.solar_system_id.unwrap());
    let account: i64 = db.query_one("INSERT INTO game.accounts (user_name,password_hash,role) VALUES (nextval('game.accounts_user_id_seq')::text,''::bytea,0) RETURNING user_id", &[]).await.unwrap().get("user_id");
    let character: i64 = db.query_one("INSERT INTO game.characters (user_id,name,race_id,bloodline_id,gender_id,corporation_id,station_id,solar_system_id) VALUES ($1,nextval('game.characters_character_id_seq')::text,1,1,1,1,$2,$3) RETURNING character_id", &[&account, &station_id, &system]).await.unwrap().get("character_id");
    let hull = crate::dao::items::creation::rookie_ship(&db, TYPE_CAPSULE, character, station_id, FLAG_HANGAR).await.unwrap();
    let world = space::World::new(db.clone(), sde.clone(), 1);
    let d = db.clone();
    let w = world.clone();
    let result = tokio::spawn(async move {
        let identity = Identity { char_id: character, ship_id: Some(hull), corp_id: 1, station_id: Some(station_id), system_id: Some(system) };
        for missing in [
            Identity { station_id: None, ..identity },
            Identity { system_id: None, ..identity },
            Identity { ship_id: None, ..identity },
        ] {
            assert!(matches!(ship::undock(&missing, &d, &w, &sde, None, None).await,
                Err(CallError::User { msg, .. }) if msg == messages::MSG_CANT_UNDOCK_NOT_IN_STATION));
        }
        let expected = balls::selected(&identity, &d, &w, &sde, None, None, hull, [0.0; 3]).await.expect("load headless undock ball");
        let undocked = ship::undock(&identity, &d, &w, &sde, None, None).await.expect("headless undock");
        assert_eq!(undocked.change.updates, vec![("stationid", Value::None), ("solarsystemid", Value::Int(system))]);
        assert!(matches!(undocked.change.event, Some(GameEvent::Undocked { char_id, ship_id, station_id: st, system_id }) if (char_id, ship_id, st, system_id) == (character, hull, station_id, system)));
        let waiting = crate::services::market::lock();
        tokio::pin!(waiting);
        assert!(poll_fn(|cx| Poll::Ready(waiting.as_mut().poll(cx).is_pending())).await);
        let row = d.query_one("SELECT location_id, flag_id, x, y, z FROM game.items WHERE item_id=$1", &[&hull]).await.unwrap();
        assert_eq!((row.get::<_, i64>("location_id"), row.get::<_, i32>("flag_id")), (system, 0));
        let position = [row.get::<_, f64>("x"), row.get::<_, f64>("y"), row.get::<_, f64>("z")];
        let row = d.query_one("SELECT station_id, solar_system_id FROM game.characters WHERE character_id=$1", &[&character]).await.unwrap();
        assert_eq!((row.get::<_, Option<i64>>("station_id"), row.get::<_, i64>("solar_system_id")), (None, system));
        let ball = w.with(system, move |s| s.ball(hull).cloned()).await.unwrap().unwrap();
        assert_eq!(ball.pos, position);
        assert_eq!((ball.mass, ball.max_vel, ball.agility, ball.signature_radius), (expected.mass, expected.max_vel, expected.agility, expected.signature_radius));
        drop(undocked.economy);
        drop(waiting.await);
        w.remove_ball(system, hull).await;
        item_dao::dock(&d, hull, station_id, FLAG_HANGAR).await.unwrap();
        character_dao::dock(&d, character, station_id, system).await.unwrap();
        let expected_space = balls::selected(&identity, &d, &w, &sde, Some(system), None, hull, [0.0; 3]).await.expect("load RPC undock ball");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let _peer = tokio::net::TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
        let (stream, address) = listener.accept().await.unwrap();
        let cfg = Arc::new(crate::config::Config::default());
        let chat = crate::chat::Hub::new(&cfg.chat.domain, d.clone(), w.clone());
        let login = crate::handshake::Login { user_id: account, user_name: account.to_string(), role: 0, client_id: character, session_id: character, language_id: "EN".into() };
        let mut client = Client::new(crate::conn::Conn::new(stream, address), cfg, d.clone(), sde.clone(), chat, w.clone(), login);
        client.session.apply(vec![("charid", Value::Int(character)), ("corpid", Value::Int(1)), ("shipid", Value::Int(hull)), ("stationid", Value::Int(station_id)), ("solarsystemid2", Value::Int(system)), ("solarsystemid", Value::Int(system))]);
        let call = Call { service: "ship", method: "Undock", args: &[], kwargs: &Value::None, bound: false, bind_params: None };
        crate::services::dispatch(&mut client, &call).await.expect("dispatch undock RPC");
        assert_eq!(client.session.int("stationid"), None);
        assert_eq!(client.session.int("solarsystemid"), Some(system));
        assert!(client.outbox.iter().any(|packet| packet.command == crate::macho::cmd::SESSIONCHANGENOTIFICATION));
        let rpc_ball = w.with(system, move |s| s.ball(hull).cloned()).await.unwrap().unwrap();
        assert_eq!((rpc_ball.mass, rpc_ball.max_vel, rpc_ball.agility, rpc_ball.signature_radius), (expected_space.mass, expected_space.max_vel, expected_space.agility, expected_space.signature_radius));
        w.remove_ball(system, hull).await;
        item_dao::dock(&d, hull, station_id, FLAG_HANGAR).await.unwrap();
        assert!(d.query_opt("SELECT character_id FROM game.characters WHERE character_id=$1", &[&0i64]).await.expect("check anonymous character").is_none());
        let missing_bloodline = crate::dao::characters::attributes::bloodline(&d, 0).await.expect_err("anonymous character has no bloodline");
        let before = d.query_one("SELECT location_id, flag_id, x, y, z FROM game.items WHERE item_id=$1", &[&hull]).await.expect("read docked hull before anonymous undock");
        let anonymous = Identity { char_id: 0, ..identity };
        match ship::undock(&anonymous, &d, &w, &sde, None, None).await {
            Err(CallError::Internal(error)) => assert_eq!(error.to_string(), missing_bloodline.to_string(), "anonymous undock must fail loading the missing character's bloodline"),
            Err(error) => panic!("anonymous undock returned unexpected error: {error:?}"),
            Ok(_) => panic!("anonymous undock unexpectedly succeeded without a character"),
        }
        let after = d.query_one("SELECT location_id, flag_id, x, y, z FROM game.items WHERE item_id=$1", &[&hull]).await.expect("read docked hull after anonymous undock");
        assert_eq!((after.get::<_, i64>("location_id"), after.get::<_, i32>("flag_id")), (station_id, FLAG_HANGAR));
        for column in ["x", "y", "z"] {
            assert_eq!(after.get::<_, Option<f64>>(column), before.get::<_, Option<f64>>(column));
        }
        let row = d.query_one("SELECT station_id, solar_system_id FROM game.characters WHERE character_id=$1", &[&character]).await.expect("read character after anonymous undock");
        assert_eq!((row.get::<_, Option<i64>>("station_id"), row.get::<_, i64>("solar_system_id")), (None, system));
        assert!(w.with(system, move |s| s.ball(hull).is_none()).await.expect("inspect system after anonymous undock"));
    }).await;
    world.remove_ball(system, hull).await;
    db.execute("DELETE FROM game.item_attributes WHERE item_id=$1", &[&hull]).await.unwrap();
    db.execute("DELETE FROM game.items WHERE item_id=$1", &[&hull]).await.unwrap();
    db.execute("DELETE FROM game.characters WHERE character_id=$1", &[&character]).await.unwrap();
    db.execute("DELETE FROM game.accounts WHERE user_id=$1", &[&account]).await.unwrap();
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn movement_dispatch_and_headless_guards() {
    use std::sync::Arc;
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let world = space::World::new(db.clone(), sde.clone(), 1);
    let system = -1;
    world.system(system).await.unwrap();
    let mut vessel = ship();
    vessel.speed_fraction = 1.0;
    let mut target = ship();
    target.id = 456;
    target.mode = MODE_STOP;
    target.pos = [1000.0, 0.0, 0.0];
    target.vel = [0.0; 3];
    world.with(system, move |s| { s.insert(vessel); s.insert(target); }).await.unwrap();
    let identity = Identity { char_id: 789, ship_id: Some(123), corp_id: 12, station_id: None, system_id: Some(system) };
    let missing_ship = Identity { ship_id: None, ..identity };
    for result in [
        beyonce::stop(&identity, &world, None),
        beyonce::orbit(&identity, &world, None, 456, 100.0),
        beyonce::follow_ball(&identity, &world, None, 456, 100.0),
        beyonce::stop(&missing_ship, &world, Some(system)),
        beyonce::orbit(&missing_ship, &world, Some(system), 456, 100.0),
        beyonce::follow_ball(&missing_ship, &world, Some(system), 456, 100.0),
    ] {
        assert!(matches!(result, Ok(Value::None)));
    }
    assert!(world.after(system, 1100).await);
    let ball = world.ball(system, 123).await.unwrap();
    assert_eq!((ball.mode, ball.follow_id, ball.speed_fraction), (MODE_GOTO, 0, 1.0));

    let cfg = Arc::new(crate::config::Config::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let _peer = tokio::net::TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
    let (stream, address) = listener.accept().await.unwrap();
    let login = crate::handshake::Login { user_id: 1, user_name: "test".into(), role: 0, client_id: 1, session_id: 1, language_id: "EN".into() };
    let chat = crate::chat::Hub::new(&cfg.chat.domain, db.clone(), world.clone());
    let mut client = Client::new(crate::conn::Conn::new(stream, address), cfg, db, sde, chat, world.clone(), login);
    client.session.apply(vec![("shipid", Value::Int(123)), ("solarsystemid", Value::Int(system)), ("solarsystemid2", Value::Int(system))]);
    for (method, mode, range) in [
        ("CmdStop", MODE_STOP, 0.0),
        ("CmdOrbit", space::MODE_ORBIT, 100.0),
        ("CmdFollowBall", space::MODE_FOLLOW, 200.0),
    ] {
        let args = [Value::Int(456), Value::Float(range)];
        let call = Call { service: "beyonce", method, args: &args, kwargs: &Value::None, bound: false, bind_params: None };
        assert!(matches!(crate::services::dispatch(&mut client, &call).await, Ok(Value::None)));
        assert!(world.after(system, 1100).await);
        let ball = world.ball(system, 123).await.unwrap();
        assert_eq!(ball.mode, mode);
        if mode != MODE_STOP {
            assert_eq!((ball.follow_id, ball.follow_range), (456, range as f32));
            assert_eq!(ball.speed_fraction, 1.0);
        }
    }
}
