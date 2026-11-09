//! Real framed client transport capture for ordinary warp replay.
use super::*;
use std::collections::BTreeMap;
use serde_json::{json, Value as Json};
use std::sync::Arc;
use infinity_core::consts::{AU, CATEGORY_SHIP, dogma::{ATTR_CAPACITOR_CAPACITY, ATTR_MASS, ATTR_WARP_CAPACITOR_NEED}};
use crate::services::fit::{Fit, Key};

#[path = "../../combat/groups_health/tests/fixture.rs"]
mod fixture;
use crate::macho::{Address, Packet};

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut result = String::with_capacity(bytes.len() * 2);
    for byte in bytes { write!(&mut result, "{byte:02x}").unwrap(); }
    result
}

fn object_view(value: &Value) -> Json {
    match value {
        Value::None => Json::Null,
        Value::Bool(v) => json!(v),
        Value::Int(v) => json!(v),
        Value::Float(v) if v.is_finite() => json!(v),
        Value::Bytes(v) => json!({"t":"bytes", "hex":hex(v)}),
        Value::Unicode(v) => json!({"t":"unicode", "text":v}),
        Value::Tuple(v) => json!({"t":"tuple", "items":v.iter().map(object_view).collect::<Vec<_>>() }),
        Value::List(v) => json!({"t":"list", "items":v.iter().map(object_view).collect::<Vec<_>>() }),
        Value::Dict(v) => json!({"t":"dict", "items":v.iter().map(|(k, v)| [object_view(k), object_view(v)]).collect::<Vec<_>>() }),
        Value::Global(v) => json!({"t":"global", "name":v}),
        Value::Instance(class, state) => json!({"t":"instance", "class":object_view(class), "state":object_view(state)}),
        Value::Reduce { data, list, dict } => json!({"t":"reduce", "data":object_view(data),
            "list":list.iter().map(object_view).collect::<Vec<_>>(),
            "dict":dict.iter().map(|(k,v)| [object_view(k), object_view(v)]).collect::<Vec<_>>() }),
        Value::DbRow { descriptor, .. } => {
            let descriptor = marshal::DbRowDescriptor::from_value(descriptor).expect("DBRow descriptor");
            let columns: Vec<_> = descriptor.columns.iter().map(|(name, ty)| json!([name, *ty as i32])).collect();
            let values: Vec<_> = descriptor.columns.iter().map(|(name, _)| object_view(&descriptor.get(value, name).expect("DBRow column"))).collect();
            json!({"t":"dbrow", "columns":columns, "values":values, "virtual_columns":descriptor.virtual_columns})
        }
        other => panic!("unsupported captured object: {other:?}"),
    }
}

fn opaque(value: &Value, objects: &mut BTreeMap<String, Json>) -> Json {
    let encoded = hex(&marshal::dumps(value));
    let view = object_view(value);
    if let Some(previous) = objects.insert(encoded.clone(), view.clone()) { assert_eq!(previous, view); }
    json!({"marshal_hex":encoded})
}

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

fn scalar(value: &Value) -> Json {
    match value {
        Value::None => Json::Null,
        Value::Bool(v) => json!(v),
        Value::Int(v) => json!(v),
        Value::Float(v) if v.is_finite() => json!(v),
        Value::Bytes(v) => json!(std::str::from_utf8(v).expect("protocol text")),
        Value::Unicode(v) => json!(v),
        Value::Tuple(v) | Value::List(v) => Json::Array(v.iter().map(scalar).collect()),
        other => panic!("unsupported scalar notification: {other:?}"),
    }
}

fn notification_arg(value: &Value, objects: &mut BTreeMap<String, Json>) -> Json {
    match value {
        Value::Tuple(values) | Value::List(values) =>
            Json::Array(values.iter().map(|value| notification_arg(value, objects)).collect()),
        Value::None | Value::Bool(_) | Value::Int(_) | Value::Float(_) | Value::Bytes(_) | Value::Unicode(_) => scalar(value),
        _ => opaque(value, objects),
    }
}

fn notification(packet: &Packet) -> Option<(String, Vec<Value>)> {
    let Address::Broadcast { broadcast_id, .. } = &packet.destination else { return None };
    let name = broadcast_id.as_text()?.into_owned();
    let envelope = packet.body.first()?.items()?;
    let bytes = envelope.get(1)?.as_bytes()?;
    let payload = marshal::loads(bytes).expect("notification payload");
    let tuple = payload.items().expect("notification tuple");
    assert_eq!(tuple.first().and_then(Value::as_int), Some(1));
    Some((name, tuple.get(1).and_then(Value::items).expect("notification args").to_vec()))
}

fn destiny_event(value: &Value, packet_index: usize, batch_event_index: usize, objects: &mut BTreeMap<String, Json>) -> (Json, Json) {
    let fields = value.items().expect("destiny event tuple");
    assert_eq!(fields.len(), 2);
    let stamp = fields[0].as_int().expect("destiny stamp");
    let call = fields[1].items().expect("destiny call tuple");
    assert_eq!(call.len(), 2);
    let op = call[0].as_text().expect("destiny operation").into_owned();
    let args = call[1].items().expect("destiny args");
    let (args_json, stream) = match op.as_str() {
        "AddBalls" => {
            assert_eq!(args.len(), 1);
            let chunk = args[0].items().expect("AddBalls chunk");
            assert_eq!(chunk.len(), 3);
            let stream = hex(chunk[0].as_bytes().expect("AddBalls state bytes"));
            let args = json!([[{"bytes_hex":stream}, opaque(&chunk[1], objects), opaque(&chunk[2], objects)]]);
            (args, Some(stream))
        }
        "SetState" => {
            assert_eq!(args.len(), 1);
            (json!([opaque(&args[0], objects)]), None)
        }
        "OnSpecialFX" => {
            assert_eq!(args.len(), 14);
            let mut rendered: Vec<_> = args[..13].iter().map(scalar).collect();
            rendered.push(if args[13] == Value::None { Json::Null } else { opaque(&args[13], objects) });
            (Json::Array(rendered), None)
        }
        _ => (Json::Array(args.iter().map(scalar).collect()), None),
    };
    let batch = json!([stamp, [op, args_json]]);
    let mut event = json!({"stamp":stamp, "op":op, "args":args_json,
        "packet_index":packet_index, "batch_event_index":batch_event_index, "source":"tcp_event"});
    if let Some(stream) = stream { event["stream_hex"] = json!(stream); }
    (event, batch)
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

fn checkpoint(ball: &Ball, stamp: i64, phase: &str, event_index: Option<usize>) -> Json {
    let samples: Vec<_> = crate::space::system::tests::events::completed_segment_samples(ball)
        .into_iter().map(|(fraction, pos, vel)| {
            assert!(pos.iter().chain(vel.iter()).all(|value| value.is_finite()), "nonfinite server interpolation sample");
            json!({"fraction":fraction, "pos":pos, "vel":vel})
        }).collect();
    let mut value = json!({"stamp":stamp, "phase":phase, "id":ball.id, "mode":ball.mode,
        "pos":ball.pos, "vel":ball.vel, "samples":samples, "source":"server_expectation"});
    if matches!(ball.mode, space::MODE_WARP | space::MODE_MISSILE) {
        value["effect_stamp"] = json!(ball.effect_stamp);
    }
    if matches!(ball.mode, space::MODE_GOTO | space::MODE_WARP | space::MODE_MISSILE) {
        value["goto"] = json!(ball.goto);
    }
    if ball.mode == space::MODE_WARP {
        value["length"] = json!(ball.warp.length);
        value["min_range"] = json!(ball.warp.min_range);
        value["factor"] = json!(ball.warp.factor);
    }
    if let Some(index) = event_index { value["event_index"] = json!(index); }
    value
}

fn capture_ticks(s: &mut space::System, ship: i64, last: i64, tx: tokio::sync::mpsc::UnboundedSender<(i64, Ball)>) {
    s.schedule(100, move |s| {
        let stamp = s.current_time();
        if stamp != last {
            let Some(ball) = s.ball(ship).cloned() else { return };
            if tx.send((stamp, ball)).is_err() { return; }
        }
        capture_ticks(s, ship, stamp, tx);
    });
}

async fn capacitor_sample(world: &space::World, system: i64, row: fixture::Fixture) -> (u64, f64, f64, i64, f64) {
    world.with(system, move |actor| {
        assert_eq!(actor.pilot_ball(row.character).map(|ball| ball.id), Some(row.ship));
        let (generation, capacity, tau) = crate::space::system::tests::events::fitted_capacitor_curve_for_capture(actor, row.ship)
            .expect("live matching fitted ship");
        let (at, level) = crate::services::combat::groups_health::tests::capacitor_snapshot_for_capture(row.ship, capacity, tau);
        (generation, capacity, tau, at, level)
    }).await.expect("system actor")
}

fn sample_record(case_id: &str, sample_id: &str, kind: &str, row: fixture::Fixture,
    after_request: Option<usize>, after_packet: Option<usize>, reference: Option<(usize, usize)>,
    (generation, capacity, tau, at, level): (u64, f64, f64, i64, f64)) -> Json {
    json!({"sample_id":sample_id, "case_id":case_id, "sample_kind":kind,
        "after_request_index":after_request, "after_packet_index":after_packet,
        "reference_notice":reference.map(|(packet_index, row_index)| json!({"packet_index":packet_index,"row_index":row_index})),
        "character_id":row.character, "ship_id":row.ship, "system_id":-1,
        "membership_generation":generation, "sampled_filetime":at, "capacity":capacity,
        "recharge_tau_ms":tau, "level":level, "provenance":"production_capacitor_snapshot"})
}


struct AbortTask<T>(Option<tokio::task::JoinHandle<T>>);
impl<T> AbortTask<T> {
    fn new(task: tokio::task::JoinHandle<T>) -> Self { Self(Some(task)) }
    fn abort(&self) { if let Some(task) = &self.0 { task.abort(); } }
    async fn finish(mut self) -> Result<T, tokio::task::JoinError> {
        let result = self.0.as_mut().expect("owned task").await;
        drop(self.0.take());
        result
    }
}
impl<T> Drop for AbortTask<T> {
    fn drop(&mut self) { self.abort(); }
}
struct SeededCap {
    ship: i64,
    previous: Option<(f64, i64)>,
}
impl SeededCap {
    fn new(ship: i64, level: f64) -> Self {
        let previous = crate::services::combat::groups_health::tests::replace_capacitor_test_entry(ship, Some((level, i64::MAX)));
        Self { ship, previous }
    }
}
impl Drop for SeededCap {
    fn drop(&mut self) {
        crate::services::combat::groups_health::tests::replace_capacitor_test_entry(self.ship, self.previous);
    }
}

fn controlled_system(first: i64, second: i64, same_boundary: bool) -> space::System {
    use crate::services::combat::groups_health::{Condition, ShipStats};
    use crate::services::fit::attributes::tests::ship_fit;
    let mut system = space::System::new(-1, Arc::default(), 0);
    let install = |system: &mut space::System, id, max_vel, velocity| {
        let mut ball = ship();
        ball.id = id;
        ball.char_id = None;
        ball.mode = MODE_STOP;
        ball.pos = [0.0; 3];
        ball.old_pos = ball.pos;
        ball.vel = [velocity, 0.0, 0.0];
        ball.old_vel = ball.vel;
        ball.mass = 1_000_000.0;
        ball.agility = 1.0;
        ball.max_vel = max_vel;
        system.insert(ball);
        let fit = ship_fit(&[
            (ATTR_WARP_CAPACITOR_NEED, 0.000001),
            (ATTR_CAPACITOR_CAPACITY, 1.0),
            (infinity_core::consts::dogma::ATTR_RECHARGE_RATE, 1000.0),
        ]);
        let stats = ShipStats::from_fit(&fit, Key::Ship);
        system.install_condition(id, Condition { now: stats.max, stats, shield_at: crate::client::filetime_now() }, fit);
    };
    install(&mut system, second, 0.0, 0.0);
    let revision = system.projection_revision(second);
    assert_eq!(commit_warp(&mut system, second, revision, false, 3000, WarpDestination::Exact([AU, 0.0, 0.0]), None), WarpCommit::Queued);
    for tick in 1..=(179 + i64::from(same_boundary)) { system.tick(tick * 1000); }
    assert_eq!(system.ball(second).unwrap().effect_stamp, if same_boundary { -181 } else { -180 });
    install(&mut system, first, 100.0, 100.0);
    if same_boundary {
        let mut unrelated = ship();
        unrelated.id = -931_003;
        unrelated.mode = MODE_STOP;
        unrelated.pos = [0.0, 10000.0, 0.0];
        unrelated.vel = [0.0; 3];
        system.insert(unrelated);
    }
    system
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn warp_capacitor_controlled_catchup_tcp() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let world = space::World::new(db.clone(), sde.clone(), 1);
    let cfg = Arc::new(crate::config::Config::default());
    for (steps, same_boundary) in [(2_i64, false), (5, false), (6, false), (2, true)] {
        let base = 179 + i64::from(same_boundary);
        let ids = if same_boundary { vec![-931_001, -931_002, -931_003] } else { vec![-931_001, -931_002] };
        let logical_steps = if steps > 5 { 1 } else { steps };
        let first = -931_001;
        let second = -931_002;
        let _first_seed = SeededCap::new(first, 0.25);
        let _second_seed = SeededCap::new(second, 0.5);
        let mut reference = controlled_system(first, second, same_boundary);
        let revision = reference.projection_revision(first);
        assert_eq!(commit_warp(&mut reference, first, revision, false, 3000, WarpDestination::Exact([AU, 0.0, 0.0]), None), WarpCommit::Queued);
        let initial_stamp = reference.current_time();
        let initial_balls: Vec<_> = ids.iter().map(|id| reference.ball(*id).unwrap().clone()).collect();
        let mut ledger = BTreeMap::new();
        let reference_steps: Vec<_> = if steps > 5 { vec![(base + steps) * 1000] }
            else { (1..=steps).map(|step| (base + step) * 1000).collect() };
        for &wall_ms in &reference_steps {
            crate::space::system::tests::events::tick_with_collisions(&mut reference, wall_ms, false);
            for &id in &ids {
                ledger.insert((reference.current_time(), id), reference.ball(id).unwrap().clone());
            }
        }
        crate::services::combat::groups_health::tests::replace_capacitor_test_entry(first, Some((0.25, i64::MAX)));
        crate::services::combat::groups_health::tests::replace_capacitor_test_entry(second, Some((0.5, i64::MAX)));
        let mut measured = controlled_system(first, second, same_boundary);
        let revision = measured.projection_revision(first);
        assert_eq!(commit_warp(&mut measured, first, revision, false, 3000, WarpDestination::Exact([AU, 0.0, 0.0]), None), WarpCommit::Queued);
        assert_eq!(measured.current_time(), initial_stamp);
        for ball in &initial_balls {
            assert_eq!(crate::space::system::tests::events::encoded_ball_for_test(ball, false),
                crate::space::system::tests::events::encoded_ball_for_test(measured.ball(ball.id).unwrap(), false));
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut peer = tokio::net::TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
        let (stream, address) = listener.accept().await.unwrap();
        let account = -931_100 - steps;
        let login = crate::handshake::Login { user_id: account, user_name: account.to_string(), role: 0, client_id: account, session_id: account, language_id: "EN".into() };
        let chat = crate::chat::Hub::new(&cfg.chat.domain, db.clone(), world.clone());
        let mut client = Client::new(crate::conn::Conn::new(stream, address), cfg.clone(), db.clone(), sde.clone(), chat, world.clone(), login);
        client.session.apply(vec![("charid", Value::Int(first)), ("shipid", Value::Int(first)),
            ("solarsystemid", Value::Int(-1)), ("corpid", Value::Int(1))]);
        let state = measured.observe(account, first, client.destiny_tx.clone());
        client.notify("DoDestinyUpdate", vec![Value::list([state]), Value::Bool(false)]);
        let mut registration = crate::live_sessions::register(account, false).unwrap();
        registration.wait_for_predecessor().await;
        let task = AbortTask::new(tokio::spawn(async move { client.run(&mut registration).await }));
        crate::space::system::tests::events::tick_with_collisions(&mut measured, (base + steps) * 1000, false);
        for &id in &ids {
            assert_eq!(crate::space::system::tests::events::encoded_ball_for_test(measured.ball(id).unwrap(), false),
                crate::space::system::tests::events::encoded_ball_for_test(&ledger[&(initial_stamp + logical_steps, id)], false));
        }
        let mut packets = Vec::new();
        let mut notifications = Vec::new();
        let mut events = Vec::new();
        let mut objects = BTreeMap::new();
        let mut initial_state_hex = None;
        let mut initial_packet = None;
        let mut corrections = BTreeMap::new();
        while corrections.len() < logical_steps.min(2) as usize || initial_state_hex.is_none() {
            let frame = tokio::time::timeout(std::time::Duration::from_secs(10), crate::framing::read_frame(&mut peer)).await.unwrap().unwrap().unwrap();
            let received_at = crate::client::filetime_now();
            let packet = Packet::from_value(&marshal::loads(&frame.payload).unwrap()).unwrap();
            let index = packets.len();
            let decoded = notification(&packet);
            let mut packet_json = json!({"packet_index":index, "received_at_filetime":received_at,
                "frame_payload_hex":hex(&frame.payload), "command":packet.command});
            if let Some((name, args)) = decoded {
                packet_json["notification_name"] = json!(name);
                if name == "DoDestinyUpdate" {
                    let batch = args[0].items().expect("destiny update batch");
                    let mut batch_json = Vec::new();
                    for (batch_index, value) in batch.iter().enumerate() {
                        let (event, rendered) = destiny_event(value, index, batch_index, &mut objects);
                        let op = event["op"].as_str().unwrap();
                        if op == "SetState" {
                            let fields = value.items().unwrap()[1].items().unwrap()[1].items().unwrap();
                            let Value::Instance(_, bag) = &fields[0] else { panic!("SetState bag") };
                            initial_state_hex = Some(hex(bag.get("state").unwrap().as_bytes().unwrap()));
                            initial_packet = Some(index);
                        } else {
                            if op == "AddBalls" {
                                let bytes = value.items().unwrap()[1].items().unwrap()[1].items().unwrap()[0].items().unwrap()[0].as_bytes().unwrap();
                                let id = i64::from_le_bytes(bytes[5..13].try_into().unwrap());
                                let stamp = event["stamp"].as_i64().unwrap();
                                assert!(corrections.insert((stamp, id), events.len()).is_none());
                            }
                            events.push(event);
                        }
                        batch_json.push(rendered);
                    }
                    notifications.push(json!({"packet_index":index, "name":name, "args":[batch_json, scalar(&args[1])]}));
                } else {
                    notifications.push(json!({"packet_index":index, "name":name, "args":args.iter().map(scalar).collect::<Vec<_>>() }));
                }
            }
            packets.push(packet_json);
        }
        assert_eq!(corrections.len(), logical_steps.min(2) as usize);
        if steps <= 5 {
            assert_eq!(corrections.keys().next().unwrap().0 == corrections.keys().next_back().unwrap().0, same_boundary);
        }
        let mut expected: Vec<_> = initial_balls.iter().map(|ball| checkpoint(ball, initial_stamp, "initial", None)).collect();
        for (&(stamp, id), ball) in &ledger {
            let index = corrections.get(&(stamp, id)).copied();
            expected.push(checkpoint(ball, stamp, if index.is_some() { "event" } else { "completed" }, index));
        }
        println!("WARP_CAPACITOR_CAPTURE {}", json!({
            "name":if same_boundary { "controlled-same-boundary".to_string() } else { format!("controlled-catchup-{steps}") }, "lane":"controlled-production-tick-over-tcp",
            "initial_stamp":initial_stamp, "ticks":logical_steps, "tracked_ids":ids,
            "initial_state_hex":initial_state_hex.unwrap(), "initial_state_packet_index":initial_packet.unwrap(),
            "events":events, "expected":expected, "packets":packets, "notifications":notifications,
            "object_values":objects, "session":{"charid":first,"shipid":first,"solarsystemid":-1,"corpid":1},
            "seed_clock":"future FILETIME, no recharge before activation", "collisions":false,
            "reference_step_ms":reference_steps,
            "catchup_ms":(base + steps) * 1000
        }));
        task.abort();
        let _ = task.finish().await;
    }
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn warp_capacitor_real_service_tcp() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
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
    let baseline = fixture::fingerprint(&db).await.unwrap();
    for (name, ship_type, fraction) in [
        ("full", positive, 2.0), ("partial", positive, 0.5),
        ("short", positive, 0.05), ("tiny", positive, 0.0000005),
        ("smaller", positive, 0.0000000001), ("empty", positive, 0.0),
        ("zero", zero, 0.0), ("cancel", positive, 2.0),
    ] {
        let mut created = Vec::new();
        for _ in 0..4 {
            match fixture::create(&db, ship_type, -1).await {
                Ok(row) => created.push(row),
                Err(error) => {
                    for row in created { fixture::cleanup(&db, row).await.unwrap(); }
                    panic!("fixture creation failed: {error}");
                }
            }
        }
        let rows: [fixture::Fixture; 4] = created.try_into().ok().expect("four fixtures");
        let d = db.clone();
        let data = sde.clone();
        let world = space::World::new(db.clone(), sde.clone(), 1);
        let w = world.clone();
        let result = tokio::spawn(async move {
            real_service_case(&d, data, &w, name, rows, fraction).await;
        }).await;
        for row in rows {
            world.remove_ball(-1, row.ship).await;
            world.unregister_pilot_generation(row.character, None);
        }
        let flush = world.flush_positions().await;
        drop(world);
        for row in rows {
            crate::services::combat::forget_ship(row.ship);
            fixture::cleanup(&db, row).await.unwrap();
        }
        flush.unwrap();
        result.unwrap();
    }
    assert_eq!(fixture::fingerprint(&db).await.unwrap(), baseline);
}

async fn real_service_case(
    db: &crate::db::Db, sde: Arc<crate::sde::Sde>, world: &Arc<space::World>,
    name: &str, rows: [fixture::Fixture; 4], fraction: f64,
) {
    let system = -1;
    world.system(system).await.unwrap();
    let ship_type = crate::dao::items::inventory::type_id(db, rows[0].ship).await.unwrap().unwrap();
    let base = Fit::for_space_type(sde.clone(), ship_type, vec![]);
    let need = base.value(Key::Ship, ATTR_WARP_CAPACITOR_NEED);
    let mass = base.value(Key::Ship, ATTR_MASS);
    let distance = if matches!(name, "full" | "partial" | "zero") { AU } else { 1_000_000.0 };
    let capacity = need * mass * (distance / AU) * fraction;
    assert!(crate::dao::items::dogma::set_override(db, rows[0].ship, ATTR_CAPACITOR_CAPACITY, capacity).await.unwrap());
    for (index, row) in rows.iter().enumerate() {
        let free = crate::dao::items::space_ships::FreeShip {
            item_id: row.ship, type_id: ship_type, owner_id: row.character,
            corporation_id: 1, alliance_id: -1, pos: [0.0, index as f64 * 100.0, 0.0],
        };
        let mut prepared = balls::piloted(db, sde.clone(), &free, row.character, 0.0).await.unwrap();
        if index == 0 {
            prepared.ball.mode = MODE_GOTO;
            prepared.ball.goto = [distance, 0.0, 0.0];
            prepared.ball.vel = [f64::from(prepared.ball.max_vel), 0.0, 0.0];
            prepared.ball.speed_fraction = 1.0;
            if name == "cancel" {
                prepared.ball.max_vel = 0.0;
                prepared.ball.vel = [0.0; 3];
            }
        }
        world.with(system, move |s| prepared.install(s, |s, ball| s.add_ship(ball))).await.unwrap();
    }
    let target_id = -932_001;
    let mut target = ship();
    target.id = target_id;
    target.type_id = ship_type;
    target.char_id = None;
    target.mode = MODE_STOP;
    target.pos = [distance, 0.0, 0.0];
    target.old_pos = target.pos;
    target.vel = [0.0; 3];
    target.old_vel = target.vel;
    target.free = false;
    world.with(system, move |s| s.insert(target)).await.unwrap();
    assert!(world.after(system, 1100).await);
    let (sample_tx, mut sample_rx) = tokio::sync::mpsc::unbounded_channel();
    world.with(system, move |s| {
        let stamp = s.current_time();
        capture_ticks(s, rows[0].ship, stamp, sample_tx);
    }).await.unwrap();

    let cfg = Arc::new(crate::config::Config::default());
    let mut seats = Vec::new();
    let mut late = None;
    let mut navigation_skills = None;
    let mut initial_identity = None;
    for (seat_index, row) in rows.into_iter().enumerate() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let peer = tokio::net::TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
        let (stream, address) = listener.accept().await.unwrap();
        let client_id = row.account;
        let login = crate::handshake::Login { user_id: row.account, user_name: row.account.to_string(),
            role: 0, client_id, session_id: row.character, language_id: "EN".into() };
        let chat = crate::chat::Hub::new(&cfg.chat.domain, db.clone(), world.clone());
        let mut client = Client::new(crate::conn::Conn::new(stream, address), cfg.clone(), db.clone(), sde.clone(), chat, world.clone(), login);
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
        let (state, initial_ball, stamp) = loop {
            let state = world.observe(system, client_id, row.ship, client.destiny_tx.clone()).await.unwrap();
            let stamp = state.items().unwrap()[0].as_int().unwrap();
            let current = world.with(system, move |s| (s.current_time(), s.ball(rows[0].ship).unwrap().clone())).await.unwrap();
            if current.0 == stamp { break (state, current.1, stamp); }
        };
        if name == "full" && seat_index == 0 { assert_eq!(initial_identity, Some(capture_identity(&client))); }
        client.notify("DoDestinyUpdate", vec![Value::list([state]), Value::Bool(false)]);
        seats.push((row, client, peer, initial_ball, stamp, primed, objects, initial_cap));
    }
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
        assert!(world.with(system, move |s| s.ball(rows[0].ship).is_some_and(|ball| ball.is_warping())).await.unwrap(),
            "late observer must enter during partial flight");
    }
    let (row, mut client, peer, primed, objects, initial_cap) = late.expect("late observer");
    let (state, initial_ball, stamp) = loop {
        let state = world.observe(system, row.account, row.ship, client.destiny_tx.clone()).await.unwrap();
        let stamp = state.items().unwrap()[0].as_int().unwrap();
        let current = world.with(system, move |s| (s.current_time(), s.ball(rows[0].ship).unwrap().clone())).await.unwrap();
        if current.0 == stamp { break (state, current.1, stamp); }
    };
    client.notify("DoDestinyUpdate", vec![Value::list([state]), Value::Bool(false)]);
    seats.push((row, client, peer, initial_ball, stamp, primed, objects, initial_cap));

    let mut tasks = Vec::new();
    for (row, client, mut peer, initial_ball, stamp, primed, objects, initial_cap) in seats {
        let mut registration = crate::live_sessions::register(row.account, false).unwrap();
        registration.wait_for_predecessor().await;
        let client_task = AbortTask::new(tokio::spawn(async move { client.run(&mut registration).await }));
        let sample_world = world.clone();
        let reader = AbortTask::new(tokio::spawn(async move {
            let mut frames = Vec::new();
            let mut snapshots = Vec::new();
            while let Ok(Some(frame)) = crate::framing::read_frame(&mut peer).await {
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
                        if fields[1].as_int() == Some(row.character) && fields[2].as_int() == Some(row.ship)
                            && fields[3].as_int() == Some(i64::from(infinity_core::consts::dogma::ATTR_CHARGE)) {
                            assert!(matching.replace(index).is_none(), "multiple charge changes in one notice");
                        }
                    }
                    matching
                });
                frames.push((received_at, frame.payload));
                if let Some(index) = matching {
                    let immediate = capacitor_sample(&sample_world, system, row).await;
                    assert!(sample_world.after(system, 1000).await);
                    let later = capacitor_sample(&sample_world, system, row).await;
                    snapshots.push((packet_index, index, immediate, later));
                }
            }
            (frames, snapshots)
        }));
        tasks.push((row, client_task, reader, initial_ball, stamp, primed, objects, initial_cap));
    }
    let no_activation = matches!(name, "empty" | "cancel");
    tokio::time::timeout(std::time::Duration::from_secs(150), async {
        loop {
            assert!(world.after(system, 1000).await);
            let done = world.with(system, move |s| s.ball(rows[0].ship).is_some_and(|ball| {
                ball.mode == MODE_STOP && (ball.warp.length >= 0.0 || no_activation)
            })).await.unwrap();
            if done { break; }
        }
    }).await.expect("warp did not complete");
    assert!(world.after(system, 2000).await);
    let mut ledger = BTreeMap::new();
    while let Ok((stamp, ball)) = sample_rx.try_recv() { assert!(ledger.insert(stamp, ball).is_none()); }
    for (seat_index, (row, client, reader, initial_ball, stamp, primed, mut objects, initial_cap)) in tasks.into_iter().enumerate() {
        client.abort();
        let _ = client.finish().await;
        let (frames, cap_observations) = tokio::time::timeout(std::time::Duration::from_secs(10), reader.finish()).await.unwrap().unwrap();
        let expected_notices = usize::from(seat_index == 0 && !matches!(name, "empty" | "zero" | "cancel"));
        assert_eq!(cap_observations.len(), expected_notices, "actual capacitor debit notices");
        let final_cap = if cap_observations.is_empty() { Some(capacitor_sample(world, system, row).await) } else { None };
        let last_packet = frames.len().checked_sub(1);
        let mut packets = Vec::new();
        let mut notifications = Vec::new();
        let mut events = Vec::new();
        let mut initial_state_hex = None;
        let mut initial_packet = None;
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
        assert!(!events.iter().any(|event| event["op"] == "RemoveBalls"), "correction must not remove the ship");
        let expected_corrections = usize::from(seat_index != 3 && matches!(name, "partial" | "short" | "tiny" | "smaller"));
        assert_eq!(corrections.len(), expected_corrections);
        let mut expected = vec![checkpoint(&initial_ball, stamp, "initial", None)];
        let mut last = stamp;
        for (&time, ball) in ledger.range((stamp + 1)..) {
            assert_eq!(time, last + 1, "missing completed actor step");
            let index = corrections.get(&time).copied();
            expected.push(checkpoint(ball, time, if index.is_some() { "event" } else { "completed" }, index));
            last = time;
        }
        assert!(last > stamp);
        let case_id = format!("{name}-seat-{seat_index}");
        let mut samples = vec![sample_record(&case_id, "after-priming", "after_priming", row, None, None, None, initial_cap)];
        if let Some(cap) = final_cap {
            samples.push(sample_record(&case_id, "later", "later", row,
                (seat_index == 0).then_some(if name == "cancel" { 1 } else { 0 }), last_packet, None, cap));
        }
        for (packet_index, row_index, immediate, later) in cap_observations {
            let request_index = (seat_index == 0).then_some(0);
            let reference = Some((packet_index, row_index));
            samples.push(sample_record(&case_id, "after-notice", "after_notice", row, request_index, Some(packet_index), reference, immediate));
            samples.push(sample_record(&case_id, "later", "later", row, request_index, Some(packet_index), reference, later));
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
    }
}
