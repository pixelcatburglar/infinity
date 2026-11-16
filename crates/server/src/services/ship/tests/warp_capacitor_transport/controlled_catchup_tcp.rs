use super::*;
use std::sync::Arc;
use infinity_core::consts::{AU, dogma::{ATTR_CAPACITOR_CAPACITY, ATTR_WARP_CAPACITOR_NEED}};
use crate::services::fit::Key;

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
        let mut client = Client::new(infinity_wire::conn::Conn::new(stream, address), cfg.clone(), db.clone(), sde.clone(), chat, world.clone(), login);
        client.session.apply(vec![("charid", Value::Int(first)), ("shipid", Value::Int(first)),
            ("solarsystemid", Value::Int(-1)), ("corpid", Value::Int(1))]);
        let (completed_1, mut completion_1) = tokio::sync::oneshot::channel();
        assert!(measured.observe(account, first, client.destiny_tx.clone(), completed_1));
        crate::space::system::tests::events::publish_scene_prefix(&mut measured, false);
        assert_eq!(completion_1.try_recv(), Ok(true));
        initial_state(&mut client);
        let revision = measured.projection_revision(first);
        assert_eq!(commit_warp(&mut measured, first, revision, false, 3000, WarpDestination::Exact([AU, 0.0, 0.0]), None), WarpCommit::Queued);
        let mut registration = crate::live_sessions::register(account, false).unwrap();
        registration.wait_for_predecessor().await;
        let run = client.run(registration);
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
            let frame = tokio::time::timeout(std::time::Duration::from_secs(10), infinity_wire::framing::read_frame(&mut peer)).await.unwrap().unwrap().unwrap();
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
        drop(peer);
        // The detached system never entered `World`, so the ordinary leave can
        // fail to find the actor; the session's real result is only observed.
        let _ = run.await;
    }
}
