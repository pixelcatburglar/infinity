use super::*;
use std::collections::BTreeSet;
use std::sync::Arc;
use infinity_core::consts::{AU, CATEGORY_SHIP, GROUP_STATION, dogma::{ATTR_CAPACITOR_CAPACITY, ATTR_MASS, ATTR_WARP_CAPACITOR_NEED}};
use crate::services::fit::{Fit, Key};
use crate::services::tests::fixture::{cleanup_ship, create_ship, ShipFixture, ShipLocation};

fn capture_identity(client: &Client) -> (i64, i64, i64, i64) {
    (client.session.sid, client.session.int("charid").unwrap(), client.session.int("shipid").unwrap(),
        client.session.int("solarsystemid").unwrap())
}

async fn navigation_skill_priming(client: &mut Client, objects: &mut BTreeMap<String, Json>) -> Json {
    let identity = capture_identity(client);
    let call = Call { service: crate::services::skills::SKILL_HANDLER, method: "GetSkills",
        args: &[], kwargs: &Value::None, bound: true, bind_params: None };
    let result = crate::services::dispatch(client, &call).await;
    let observed_at = crate::client::filetime_now();
    assert_eq!(capture_identity(client), identity);
    let outcome = match result {
        Ok(value) => json!({"kind":"return", "value_marshaled_hex":opaque(&value, objects)["marshal_hex"]}),
        Err(CallError::User { msg, dict }) => {
            let value = Value::Dict(dict.into_iter().map(|(key, value)| (Value::str(key), value)).collect());
            json!({"kind":"user_error", "message":msg, "dict_marshaled_hex":opaque(&value, objects)["marshal_hex"]})
        }
        Err(CallError::Internal(error)) => json!({"kind":"internal_error", "message":format!("{error:#}")}),
    };
    json!({"provenance":"production_dispatch", "service":call.service, "method":call.method,
        "args":[], "kwargs":null, "bound":true, "bind_params":null, "session_id":identity.0,
        "character_id":identity.1, "ship_id":identity.2, "system_id":identity.3,
        "observed_at_filetime":observed_at, "outcome":outcome})
}

fn notification_arg(value: &Value, objects: &mut BTreeMap<String, Json>) -> Json {
    match value {
        Value::Tuple(values) | Value::List(values) =>
            Json::Array(values.iter().map(|value| notification_arg(value, objects)).collect()),
        Value::None | Value::Bool(_) | Value::Int(_) | Value::Float(_) | Value::Bytes(_) | Value::Unicode(_) => scalar(value),
        _ => opaque(value, objects),
    }
}

fn priming(value: &Value, objects: &mut BTreeMap<String, Json>) -> Json {
    let Value::Dict(rows) = value else { panic!("ShipGetInfo dictionary") };
    let ship_info: Vec<_> = rows.iter().map(|(item_id, row)| {
        let Value::Instance(_, fields) = row else { panic!("ShipGetInfo KeyVal") };
        let item = fields.get("invItem").expect("invItem");
        let effects = fields.get("activeEffects").expect("activeEffects");
        let Value::Dict(attrs) = fields.get("attributes").expect("attributes") else { panic!("attribute dictionary") };
        let attrs: Vec<_> = attrs.iter().map(|(key, value)| json!([key.as_int().expect("attribute ID"), scalar(value)])).collect();
        let item_hex = hex(&marshal::dumps(item));
        let effects_hex = hex(&marshal::dumps(effects));
        opaque(item, objects);
        opaque(effects, objects);
        json!({"item_id":item_id.as_int().expect("item ID"), "inv_item_marshaled_hex":item_hex,
            "attributes":attrs, "active_effects_marshaled_hex":effects_hex,
            "time":fields.get("time").and_then(Value::as_int).expect("time"),
            "wallclock_time":fields.get("wallclockTime").and_then(Value::as_int).expect("wallclock time")})
    }).collect();
    json!({"provenance":"production_dispatch", "result_marshaled_hex":hex(&marshal::dumps(value)), "ship_info":ship_info})
}

struct ActorSample {
    tracked: Ball,
    visible: [Vec<i64>; 4],
    local: [Vec<i64>; 4],
    positions: Vec<(i64, space::V3)>,
}

fn capture_ticks(s: &mut space::System, ship: i64, characters: [i64; 4], last: i64, tx: tokio::sync::mpsc::UnboundedSender<(i64, ActorSample)>) {
    s.schedule(100, move |s| {
        let stamp = s.current_time();
        if stamp != last {
            let Some(tracked) = s.ball(ship).cloned() else { return };
            let egos: [i64; 4] = std::array::from_fn(|seat| s.pilot_ball(characters[seat]).expect("sampled pilot ego").id);
            let mut visible = [Vec::new(), Vec::new(), Vec::new(), Vec::new()];
            let mut local = [Vec::new(), Vec::new(), Vec::new(), Vec::new()];
            let mut positions = Vec::new();
            for ball in s.balls() {
                positions.push((ball.id, ball.pos));
                for seat in 0..4 {
                    if s.visible_to(characters[seat], ball.id) { visible[seat].push(ball.id); }
                    if s.same_bubble(egos[seat], ball.id) { local[seat].push(ball.id); }
                }
            }
            if tx.send((stamp, ActorSample { tracked, visible, local, positions })).is_err() { return; }
        }
        capture_ticks(s, ship, characters, stamp, tx);
    });
}

async fn capacitor_sample(world: &space::World, system: i64, row: &ShipFixture) -> (u64, f64, f64, i64, f64) {
    let (character, ship) = (row.character, row.ship);
    world.with(system, move |actor| {
        assert_eq!(actor.pilot_ball(character).map(|ball| ball.id), Some(ship));
        let (generation, capacity, tau) = crate::space::system::tests::events::fitted_capacitor_curve_for_capture(actor, ship)
            .expect("live matching fitted ship");
        let (at, level) = crate::services::combat::groups_health::tests::capacitor_snapshot_for_capture(ship, capacity, tau);
        (generation, capacity, tau, at, level)
    }).await.expect("system actor")
}

fn sample_record(case_id: &str, sample_id: &str, kind: &str, row: &ShipFixture, system: i64,
    after_request: Option<usize>, after_packet: Option<usize>, reference: Option<(usize, usize)>,
    (generation, capacity, tau, at, level): (u64, f64, f64, i64, f64)) -> Json {
    json!({"sample_id":sample_id, "case_id":case_id, "sample_kind":kind,
        "after_request_index":after_request, "after_packet_index":after_packet,
        "reference_notice":reference.map(|(packet_index, row_index)| json!({"packet_index":packet_index,"row_index":row_index})),
        "character_id":row.character, "ship_id":row.ship, "system_id":system,
        "membership_generation":generation, "sampled_filetime":at, "capacity":capacity,
        "recharge_tau_ms":tau, "level":level, "provenance":"production_capacitor_snapshot"})
}

/// The first published NPC station whose celestial row carries every fact the
/// loader needs to place a global warp target; none means the SDE is incomplete
/// and the case must fail rather than fall back to a synthetic target.
fn real_warp_target(sde: &crate::sde::Sde) -> (i64, i64) {
    sde.map.stations().find_map(|station| {
        let system = station.solar_system_id.filter(|&system| system > 0 && sde.map.system(system).is_some())?;
        station.owner_id?;
        let celestial = sde.map.celestial(station.station_id)?;
        if celestial.solar_system_id != Some(system) { return None; }
        let (x, y, z) = (celestial.x?, celestial.y?, celestial.z?);
        if !x.is_finite() || !y.is_finite() || !z.is_finite() { return None; }
        if celestial.group_id != Some(GROUP_STATION) { return None; }
        let ty = sde.types.iter().find(|ty| celestial.type_id == Some(i64::from(ty.type_id)))?;
        if ty.group_id != i32::try_from(GROUP_STATION).ok() { return None; }
        let radius = f64::from(sde.celestial_radius(celestial));
        if !radius.is_finite() || radius < 0.0 { return None; }
        Some((system, station.station_id))
    }).expect("published NPC station with a resolvable warp target")
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn warp_capacitor_real_service_tcp() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let (fixture_system, target_id) = real_warp_target(sde.as_ref());
    let choose = |positive: bool| sde.types.iter().find(|ty| {
        if ty.category_id != Some(CATEGORY_SHIP) || ty.published != Some(true) { return false; }
        let fit = Fit::for_space_type(sde.clone(), ty.type_id, vec![]);
        let need = fit.value(Key::Ship, ATTR_WARP_CAPACITOR_NEED);
        let mass = fit.value(Key::Ship, ATTR_MASS);
        need.is_finite() && (if positive { need > 0.0 } else { need == 0.0 })
            && mass.is_finite() && mass > 0.0 && fitted_warp_factor(&fit).is_ok()
    }).expect("published hull matching capacitor predicate").type_id;
    let positive = choose(true);
    let zero = choose(false);
    for (name, ship_type, fraction) in [
        ("full", positive, 2.0), ("partial", positive, 0.5),
        ("short", positive, 0.05), ("tiny", positive, 0.0000005),
        ("smaller", positive, 0.0000000001), ("empty", positive, 0.0),
        ("zero", zero, 0.0), ("cancel", positive, 2.0),
    ] {
        let mut created = Vec::new();
        for _ in 0..4 {
            match create_ship(&db, ship_type, ShipLocation::Space { system: fixture_system }, &[]).await {
                Ok(row) => created.push(row),
                Err(error) => {
                    for row in created { cleanup_ship(&db, None, row).await.unwrap(); }
                    panic!("fixture creation failed: {error}");
                }
            }
        }
        let rows: [ShipFixture; 4] = created.try_into().ok().expect("four fixtures");
        let spawn_rows: [ShipFixture; 4] = rows.each_ref().map(|row| ShipFixture { account: row.account, character: row.character, ship: row.ship, items: row.items.clone() });
        let d = db.clone();
        let data = sde.clone();
        let world = space::World::new(db.clone(), sde.clone(), 1);
        let w = world.clone();
        let result = tokio::spawn(async move {
            real_service_case(&d, data, &w, name, spawn_rows, fraction, fixture_system, target_id).await;
        }).await;
        for row in &rows {
            let mut barrier = crate::live_sessions::register(row.account, true).unwrap();
            barrier.wait_for_predecessor().await;
            drop(barrier);
            world.remove_ball(fixture_system, row.ship).await;
            world.unregister_pilot_generation(row.character, None);
        }
        let flush = world.flush_positions().await;
        drop(world);
        let captured = crate::services::tests::fixture::capture_fixture_rows(&db,
            &rows.each_ref().map(|row| crate::services::tests::fixture::Fixture { account: row.account, character: row.character }),
            &rows.each_ref().map(|row| row.ship)).await.unwrap();
        for row in rows {
            crate::services::combat::forget_ship(row.ship);
            cleanup_ship(&db, None, row).await.unwrap();
        }
        flush.unwrap();
        result.unwrap();
        crate::services::tests::fixture::assert_fixture_removed(&db, &captured).await.unwrap();
    }
}

async fn real_service_case(
    db: &crate::db::Db, sde: Arc<crate::sde::Sde>, world: &Arc<space::World>,
    name: &str, rows: [ShipFixture; 4], fraction: f64, system: i64, target_id: i64,
) {
    world.system(system).await.unwrap();
    let (target_pos, target_radius) = {
        let celestial = sde.map.celestial(target_id).expect("SDE station celestial row");
        ([celestial.x.unwrap(), celestial.y.unwrap(), celestial.z.unwrap()], f64::from(sde.celestial_radius(celestial)))
    };
    world.with(system, move |s| {
        let target = s.ball(target_id).expect("loader-placed station ball");
        assert!(!target.free && target.global, "station must load as a global static ball");
        assert_eq!(target.pos, target_pos);
        assert_eq!(f64::from(target.radius), target_radius);
        assert!(s.warp_in(target_id).is_none(), "station must not project a warp-in point");
    }).await.unwrap();
    let ship0 = rows[0].ship;
    let ship_type = crate::dao::items::inventory::type_id(db, rows[0].ship).await.unwrap().unwrap();
    let base = Fit::for_space_type(sde.clone(), ship_type, vec![]);
    let need = base.value(Key::Ship, ATTR_WARP_CAPACITOR_NEED);
    let mass = base.value(Key::Ship, ATTR_MASS);
    let distance = if matches!(name, "full" | "partial" | "zero") { AU } else { 1_000_000.0 };
    let ship_radius = f64::from(balls::radius(sde.as_ref(), ship_type).unwrap());
    let stand_off = crate::space::system::warp::ordinary_warp_standoff(Some(target_radius), ship_radius, 0.0);
    let warp_distance = distance - stand_off;
    assert!(warp_distance > 0.0, "station stand-off must leave a positive nominal route");
    let capacity = need * mass * (warp_distance / AU) * fraction;
    assert!(crate::dao::items::dogma::set_override(db, rows[0].ship, ATTR_CAPACITOR_CAPACITY, capacity).await.unwrap());
    let origin = [target_pos[0] - distance, target_pos[1], target_pos[2]];
    for (index, row) in rows.iter().enumerate() {
        let pos = [origin[0], origin[1] + index as f64 * 100.0, origin[2]];
        for celestial in sde.map.celestials(system) {
            let (Some(cx), Some(cy), Some(cz)) = (celestial.x, celestial.y, celestial.z) else { continue };
            let clearance = f64::from(sde.celestial_radius(celestial)) + ship_radius;
            let delta = ((pos[0] - cx).powi(2) + (pos[1] - cy).powi(2) + (pos[2] - cz).powi(2)).sqrt();
            assert!(delta > clearance, "seat {index} start {pos:?} lies within {clearance}m of celestial {}", celestial.item_id);
        }
        let free = crate::dao::items::space_ships::FreeShip {
            item_id: row.ship, type_id: ship_type, owner_id: row.character,
            corporation_id: 1, alliance_id: -1, pos,
        };
        let mut prepared = balls::piloted(db, sde.clone(), &free, row.character, 0.0).await.unwrap();
        if index == 0 {
            prepared.ball.mode = MODE_GOTO;
            prepared.ball.goto = target_pos;
            prepared.ball.vel = [f64::from(prepared.ball.max_vel), 0.0, 0.0];
            prepared.ball.speed_fraction = 1.0;
            if name == "cancel" {
                prepared.ball.max_vel = 0.0;
                prepared.ball.vel = [0.0; 3];
            }
        }
        world.with(system, move |s| prepared.install(s, |s, ball| s.add_ship(ball))).await.unwrap();
    }
    assert!(world.after(system, 1100).await);
    let (sample_tx, mut sample_rx) = tokio::sync::mpsc::unbounded_channel();
    let characters = rows.each_ref().map(|row| row.character);
    world.with(system, move |s| {
        let stamp = s.current_time();
        capture_ticks(s, ship0, characters, stamp, sample_tx);
    }).await.unwrap();

    let cfg = Arc::new(crate::config::Config::default());
    let mut seats = Vec::new();
    let mut late = None;
    let mut navigation_skills = None;
    let mut initial_identity = None;
    for (seat_index, row) in rows.iter().enumerate() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let peer = tokio::net::TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
        let (stream, address) = listener.accept().await.unwrap();
        let client_id = row.account;
        let login = crate::handshake::Login { user_id: row.account, user_name: row.account.to_string(),
            role: 0, client_id, session_id: row.character, language_id: "EN".into() };
        let chat = crate::chat::Hub::new(&cfg.chat.domain, db.clone(), world.clone());
        let mut client = Client::new(infinity_wire::conn::Conn::new(stream, address), cfg.clone(), db.clone(), sde.clone(), chat, world.clone(), login);
        client.session.apply(vec![("charid", Value::Int(row.character)), ("shipid", Value::Int(row.ship)),
            ("solarsystemid", Value::Int(system)), ("solarsystemid2", Value::Int(system)), ("corpid", Value::Int(1))]);
        world.register_pilot(row.character, client.notify_tx.clone());
        let call = Call { service: "dogmaIM", method: "ShipGetInfo", args: &[], kwargs: &Value::None, bound: false, bind_params: None };
        let info = crate::services::dispatch(&mut client, &call).await.unwrap();
        let mut objects = BTreeMap::new();
        let primed = priming(&info, &mut objects);
        if name == "full" && seat_index == 0 {
            navigation_skills = Some(navigation_skill_priming(&mut client, &mut objects).await);
            initial_identity = Some(capture_identity(&client));
        }
        let initial_cap = capacitor_sample(world, system, row).await;
        if seat_index == 3 {
            late = Some((row, client, peer, primed, objects, initial_cap));
            continue;
        }
        // World client-map binding is not exercised here; stopped_actor_recovers_committed_boarding_from_loader_and_seeds covers World.observe binding.
        client.reset_destiny();
        let tx = client.destiny_tx.clone();
        let row_ship = row.ship;
        let (stamp, initial_ball) = tokio::time::timeout(std::time::Duration::from_secs(10), world.with(system, move |s| {
            let (completed, mut done) = tokio::sync::oneshot::channel();
            s.observe(client_id, row_ship, tx, completed);
            crate::space::system::tests::events::publish_scene_prefix(s, crate::space::collision::enabled());
            assert_eq!(done.try_recv(), Ok(true));
            (s.current_time(), s.ball(ship0).unwrap().clone())
        })).await.expect("observe job").unwrap();
        let state = initial_state(&mut client);
        assert_eq!(state.items().unwrap()[0].as_int().unwrap(), stamp);
        if name == "full" && seat_index == 0 { assert_eq!(initial_identity, Some(capture_identity(&client))); }
        seats.push((row, client, peer, initial_ball, stamp, primed, objects, initial_cap));
    }
    let prewarp = world.with(system, {
        let characters = rows.each_ref().map(|row| row.character);
        move |s| {
            let mut members = [Vec::new(), Vec::new(), Vec::new(), Vec::new()];
            for seat in 0..4 {
                let ego = s.pilot_ball(characters[seat]).expect("pre-warp pilot ego");
                members[seat] = s.balls().filter(|ball| !ball.global && s.same_bubble(ego.id, ball.id)).map(|ball| ball.id).collect();
            }
            members
        }
    }).await.expect("pre-warp scene");

    let args = [Value::str("item"), Value::Int(target_id)];
    let call = Call { service: "beyonce", method: "CmdWarpToStuff", args: &args, kwargs: &Value::None, bound: false, bind_params: None };
    let dispatch_identity = capture_identity(&seats[0].1);
    if name == "full" { assert_eq!(initial_identity, Some(dispatch_identity)); }
    let issued_at = crate::client::filetime_now();
    let result = crate::services::dispatch(&mut seats[0].1, &call).await.unwrap();
    if name == "full" { assert_eq!(capture_identity(&seats[0].1), dispatch_identity); }
    assert_eq!(result, Value::None);
    let mut request_records = vec![json!({"request_index":0, "case_id":name, "provenance":"fixture_dispatch",
        "service":call.service, "method":call.method, "args":call.args.iter().map(scalar).collect::<Vec<_>>(),
        "kwargs":null, "bound":call.bound, "bind_params":null, "character_id":rows[0].character,
        "ship_id":rows[0].ship, "system_id":system, "issued_at_filetime":issued_at,
        "outcome":{"kind":"return","value_marshaled_hex":hex(&marshal::dumps(&result))}})];
    if name == "cancel" {
        let stop = Call { service: "beyonce", method: "CmdStop", args: &[], kwargs: &Value::None, bound: false, bind_params: None };
        let at = crate::client::filetime_now();
        let outcome = crate::services::dispatch(&mut seats[0].1, &stop).await.unwrap();
        assert_eq!(outcome, Value::None);
        request_records.push(json!({"request_index":1, "case_id":name, "provenance":"fixture_dispatch",
            "service":stop.service, "method":stop.method, "args":[], "kwargs":null,
            "bound":stop.bound, "bind_params":null, "character_id":rows[0].character,
            "ship_id":rows[0].ship, "system_id":system, "issued_at_filetime":at,
            "outcome":{"kind":"return","value_marshaled_hex":hex(&marshal::dumps(&outcome))}}));
    }
    assert!(world.after(system, 2000).await);
    if name == "partial" {
        assert!(world.with(system, move |s| s.ball(ship0).is_some_and(|ball| ball.is_warping())).await.unwrap(),
            "late observer must enter during partial flight");
    }
    let (row, mut client, peer, primed, objects, initial_cap) = late.expect("late observer");
    client.reset_destiny();
    let tx = client.destiny_tx.clone();
    let (account, row_ship) = (row.account, row.ship);
    let (stamp, initial_ball) = tokio::time::timeout(std::time::Duration::from_secs(10), world.with(system, move |s| {
        let (completed, mut done) = tokio::sync::oneshot::channel();
        s.observe(account, row_ship, tx, completed);
        crate::space::system::tests::events::publish_scene_prefix(s, crate::space::collision::enabled());
        assert_eq!(done.try_recv(), Ok(true));
        (s.current_time(), s.ball(ship0).unwrap().clone())
    })).await.expect("observe job").unwrap();
    let state = initial_state(&mut client);
    assert_eq!(state.items().unwrap()[0].as_int().unwrap(), stamp);
    seats.push((row, client, peer, initial_ball, stamp, primed, objects, initial_cap));

    let mut tasks = Vec::new();
    for (row, client, mut peer, initial_ball, stamp, primed, objects, initial_cap) in seats {
        let mut registration = crate::live_sessions::register(row.account, false).unwrap();
        registration.wait_for_predecessor().await;
        let run = client.run(registration);
        let sample_world = world.clone();
        let (stop, mut stopped) = tokio::sync::watch::channel(false);
        let spawn_row = ShipFixture { account: row.account, character: row.character, ship: row.ship, items: row.items.clone() };
        let reader = tokio::spawn(async move {
            let mut frames = Vec::new();
            let mut snapshots = Vec::new();
            loop {
                // The stop signal bounds the capture; a cancelled read_frame is never reused.
                let frame = tokio::select! {
                    biased;
                    _ = stopped.changed() => break,
                    frame = infinity_wire::framing::read_frame(&mut peer) => frame,
                };
                let Ok(Some(frame)) = frame else { break };
                let packet_index = frames.len();
                let packet = Packet::from_value(&marshal::loads(&frame.payload).unwrap()).unwrap();
                let received_at = crate::client::filetime_now();
                let matching = notification(&packet).and_then(|(name, args)| {
                    if name != "OnModuleAttributeChanges" { return None; }
                    assert_eq!(args.len(), 1);
                    let Value::List(changes) = &args[0] else { panic!("attribute changes list") };
                    let mut matching = None;
                    for (index, change) in changes.iter().enumerate() {
                        let Value::Tuple(fields) = change else { panic!("attribute change row tuple") };
                        assert_eq!(fields.len(), 8);
                        assert!(matches!(fields[2], Value::Int(_)));
                        if fields[1].as_int() == Some(spawn_row.character) && fields[2].as_int() == Some(spawn_row.ship)
                            && fields[3].as_int() == Some(i64::from(infinity_core::consts::dogma::ATTR_CHARGE)) {
                            assert!(matching.replace(index).is_none(), "multiple charge changes in one notice");
                        }
                    }
                    matching
                });
                frames.push((received_at, frame.payload));
                if let Some(index) = matching {
                    let immediate = capacitor_sample(&sample_world, system, &spawn_row).await;
                    assert!(sample_world.after(system, 1000).await);
                    let later = capacitor_sample(&sample_world, system, &spawn_row).await;
                    snapshots.push((packet_index, index, immediate, later));
                }
            }
            (frames, snapshots, peer)
        });
        tasks.push((row, run, reader, stop, initial_ball, stamp, primed, objects, initial_cap));
    }
    let no_activation = matches!(name, "empty" | "cancel");
    tokio::time::timeout(std::time::Duration::from_secs(150), async {
        loop {
            assert!(world.after(system, 1000).await);
            let done = world.with(system, move |s| s.ball(ship0).is_some_and(|ball| {
                ball.mode == MODE_STOP && (ball.warp.length >= 0.0 || no_activation)
            })).await.unwrap();
            if done { break; }
        }
    }).await.expect("warp did not complete");
    assert!(world.after(system, 2000).await);
    let mut ledger = BTreeMap::new();
    while let Ok((stamp, sample)) = sample_rx.try_recv() { assert!(ledger.insert(stamp, sample).is_none()); }
    for task in &tasks {
        task.3.send_replace(true);
    }
    let mut staged = Vec::with_capacity(tasks.len());
    for (seat_index, (row, run, reader, _, initial_ball, stamp, primed, mut objects, initial_cap)) in tasks.into_iter().enumerate() {
        let (frames, cap_observations, peer) = tokio::time::timeout(std::time::Duration::from_secs(10), reader).await.unwrap().unwrap();
        let expected_notices = usize::from(seat_index == 0 && !matches!(name, "empty" | "zero" | "cancel"));
        assert_eq!(cap_observations.len(), expected_notices, "actual capacitor debit notices");
        let final_cap = if cap_observations.is_empty() { Some(capacitor_sample(world, system, row).await) } else { None };
        let last_packet = frames.len().checked_sub(1);
        let mut packets = Vec::new();
        let mut notifications = Vec::new();
        let mut events = Vec::new();
        let mut initial_state_hex = None;
        let mut initial_packet = None;
        let mut client_ids = BTreeSet::new();
        let mut removes = BTreeMap::new();
        let mut corrections = BTreeMap::new();
        for (received_at, payload) in frames {
            let packet = Packet::from_value(&marshal::loads(&payload).unwrap()).unwrap();
            let index = packets.len();
            let mut packet_json = json!({"packet_index":index, "received_at_filetime":received_at,
                "frame_payload_hex":hex(&payload), "command":packet.command});
            if let Some((event_name, args)) = notification(&packet) {
                packet_json["notification_name"] = json!(event_name);
                if event_name == "DoDestinyUpdate" {
                    let mut batch_json = Vec::new();
                    for (batch_index, value) in args[0].items().unwrap().iter().enumerate() {
                        let call = value.items().unwrap()[1].items().unwrap();
                        let op = call[0].as_text().unwrap();
                        let event_args = call[1].items().unwrap();
                        let event_stamp = value.items().unwrap()[0].as_int().unwrap();
                        if op == "SetState" {
                            let Value::Instance(_, bag) = &event_args[0] else { panic!("SetState bag") };
                            let Value::List(slims) = bag.get("slims").expect("SetState slims") else { panic!("SetState slim list") };
                            client_ids.clear();
                            client_ids.extend(slims.iter().map(|slim| {
                                let Value::Instance(_, fields) = slim else { panic!("SetState slim") };
                                fields.get("itemID").and_then(Value::as_int).expect("SetState item")
                            }));
                        } else if op == "AddBalls" {
                            let Value::List(slims) = &event_args[0].items().unwrap()[1] else { panic!("AddBalls slims") };
                            client_ids.extend(slims.iter().map(|slim| {
                                let Value::Instance(_, fields) = slim else { panic!("AddBalls slim") };
                                fields.get("itemID").and_then(Value::as_int).expect("AddBalls item")
                            }));
                        } else if op == "RemoveBalls" {
                            let sample = ledger.get(&event_stamp).expect("exact actor sample");
                            for id in event_args[0].items().unwrap().iter().map(|id| id.as_int().expect("removed id")) {
                                assert_ne!(id, row.ship, "own ego removed");
                                assert!(prewarp[seat_index].contains(&id), "removed id outside pre-warp scene");
                                assert!(!sample.local[seat_index].contains(&id), "removed id still local");
                                let (_, pos) = sample.positions.iter().find(|(ball, _)| *ball == id).expect("removed position");
                                assert!(pos.iter().all(|v| v.is_finite()), "removed position");
                                assert!(client_ids.remove(&id), "removed id absent before Remove");
                                assert!(!client_ids.contains(&id), "removed id present after Remove");
                                *removes.entry(id).or_insert(0) += 1;
                            }
                        } else if op == "CloakBall" || op == "UncloakBall" {
                            panic!("cloak member event {op} case={name} seat={seat_index} stamp={event_stamp}");
                        }
                        let (event, rendered) = destiny_event(value, index, batch_index, &mut objects);
                        let op = event["op"].as_str().unwrap();
                        if op == "SetState" {
                            let fields = value.items().unwrap()[1].items().unwrap()[1].items().unwrap();
                            let Value::Instance(_, bag) = &fields[0] else { panic!("SetState bag") };
                            assert_eq!(event["stamp"].as_i64(), Some(stamp));
                            assert!(initial_state_hex.replace(hex(bag.get("state").unwrap().as_bytes().unwrap())).is_none());
                            assert!(initial_packet.replace(index).is_none());
                        } else {
                            if op == "AddBalls" {
                                let bytes = value.items().unwrap()[1].items().unwrap()[1].items().unwrap()[0].items().unwrap()[0].as_bytes().unwrap();
                                let id = i64::from_le_bytes(bytes[5..13].try_into().unwrap());
                                if id == rows[0].ship { corrections.insert(event["stamp"].as_i64().unwrap(), events.len()); }
                            }
                            events.push(event);
                        }
                        batch_json.push(rendered);
                    }
                    notifications.push(json!({"packet_index":index, "name":event_name, "args":[batch_json, scalar(&args[1])]}));
                } else {
                    notifications.push(json!({"packet_index":index, "name":event_name, "args":args.iter().map(|value| notification_arg(value, &mut objects)).collect::<Vec<_>>() }));
                }
            }
            packets.push(packet_json);
        }
        let warp_commands = events.iter().filter(|event| event["op"] == "WarpTo"
            && event["args"][0].as_i64() == Some(rows[0].ship)).count();
        assert_eq!(warp_commands, usize::from(seat_index != 3), "one native WarpTo for pre-existing observers");
        let (&final_stamp, final_sample) = ledger.last_key_value().expect("final actor sample");
        let final_visible: BTreeSet<i64> = final_sample.visible[seat_index].iter().copied().collect();
        assert_eq!(client_ids, final_visible, "client ballpark differs from final actor visibility");
        if seat_index < 3 {
            for &id in &prewarp[seat_index] {
                let gone = !final_visible.contains(&id);
                assert_eq!(removes.get(&id).copied().unwrap_or(0), usize::from(gone),
                    "pre-warp member cardinality case={name} seat={seat_index} id={id} stamp={} local={:?} visible={:?}",
                    final_stamp, final_sample.local[seat_index], final_sample.visible[seat_index]);
            }
        }
        let expected_corrections = usize::from(seat_index != 3 && matches!(name, "partial" | "short" | "tiny" | "smaller"));
        assert_eq!(corrections.len(), expected_corrections);
        let mut expected = vec![checkpoint(&initial_ball, stamp, "initial", None)];
        let mut last = stamp;
        for (&time, sample) in ledger.range((stamp + 1)..) {
            assert_eq!(time, last + 1, "missing completed actor step");
            let index = corrections.get(&time).copied();
            expected.push(checkpoint(&sample.tracked, time, if index.is_some() { "event" } else { "completed" }, index));
            last = time;
        }
        assert!(last > stamp);
        let case_id = format!("{name}-seat-{seat_index}");
        let mut samples = vec![sample_record(&case_id, "after-priming", "after_priming", row, system, None, None, None, initial_cap)];
        if let Some(cap) = final_cap {
            samples.push(sample_record(&case_id, "later", "later", row, system,
                (seat_index == 0).then_some(if name == "cancel" { 1 } else { 0 }), last_packet, None, cap));
        }
        for (packet_index, row_index, immediate, later) in cap_observations {
            let request_index = (seat_index == 0).then_some(0);
            let reference = Some((packet_index, row_index));
            samples.push(sample_record(&case_id, "after-notice", "after_notice", row, system, request_index, Some(packet_index), reference, immediate));
            samples.push(sample_record(&case_id, "later", "later", row, system, request_index, Some(packet_index), reference, later));
        }
        let requests = if seat_index == 0 {
            request_records.iter().cloned().map(|mut actual| {
                actual["case_id"] = json!(case_id);
                actual
            }).collect()
        } else { Vec::new() };
        let mut capture = json!({
            "name":case_id, "lane":"real-service-world-tcp",
            "initial_stamp":stamp, "ticks":last-stamp, "tracked_ids":[rows[0].ship],
            "initial_state_hex":initial_state_hex.expect("TCP SetState"), "initial_state_packet_index":initial_packet.unwrap(),
            "events":events, "expected":expected, "packets":packets, "notifications":notifications,
            "object_values":objects, "priming":primed,
            "session":{"charid":row.character,"shipid":row.ship,"solarsystemid":system,"corpid":1},
            "requests":requests, "capacitor_samples":samples
        });
        if seat_index == 0 {
            if let Some(mut skills) = navigation_skills.take() {
                let identity = initial_identity.unwrap();
                assert_eq!(identity, dispatch_identity);
                assert_eq!(skills["session_id"], json!(identity.0));
                assert_eq!((skills["character_id"].as_i64(), skills["ship_id"].as_i64(), skills["system_id"].as_i64()),
                    (Some(identity.1), Some(identity.2), Some(identity.3)));
                let packet = capture["initial_state_packet_index"].as_u64().unwrap();
                let requests = capture["requests"].as_array_mut().unwrap();
                let selected: Vec<_> = requests.iter_mut().filter(|request| request["request_index"].as_i64() == Some(0)).collect();
                assert_eq!(selected.len(), 1);
                selected.into_iter().next().unwrap()["session_id"] = json!(dispatch_identity.0);
                skills["case_id"] = json!(case_id);
                skills["request_index"] = json!(0);
                skills["initial_state_packet_index"] = json!(packet);
                capture["session"]["sid"] = json!(identity.0);
                capture["navigation_priming"] = json!({"skills":skills});
            }
        }
        println!("WARP_CAPACITOR_CAPTURE {}", capture);
        staged.push((row, run, peer));
    }
    for (row, run, mut peer) in staged {
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            tokio::io::AsyncWriteExt::shutdown(&mut peer).await?;
            tokio::io::copy(&mut peer, &mut tokio::io::sink()).await
        }).await.expect("peer graceful EOF timeout").expect("peer graceful EOF failed");
        drop(peer);
        let account = row.account;
        tokio::time::timeout(std::time::Duration::from_secs(10), world.with(system, move |s| s.unobserve(account))).await.expect("unobserve job").unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(10), run).await.expect("client session cleanup timeout").expect("client session cleanup failed");
    }
}
