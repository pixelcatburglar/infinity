use super::*;
use crate::db::outbox::Outbox;

#[path = "../services/combat/groups_health/tests/fixture.rs"]
mod fixture;

#[test]
fn unix_milliseconds_convert_to_filetime() {
    assert_eq!(filetime_from_unix_ms(0), FILETIME_UNIX_EPOCH);
    assert_eq!(filetime_from_unix_ms(1), FILETIME_UNIX_EPOCH + 10_000);
    assert_eq!(filetime_from_unix_ms(-1), FILETIME_UNIX_EPOCH - 10_000);
}

#[test]
fn signed_filetime_seconds_conversions() {
    assert_eq!(unix_seconds_from_filetime(FILETIME_UNIX_EPOCH + FILETIME_SECOND + 1), 1);
    assert_eq!(unix_seconds_from_filetime(FILETIME_UNIX_EPOCH - FILETIME_SECOND - 1), -1);
    assert_eq!(unix_seconds_from_filetime(FILETIME_UNIX_EPOCH - 1), 0);
    assert_eq!(filetime_ticks_from_seconds(2), 2 * FILETIME_SECOND);
    assert_eq!(filetime_ticks_from_seconds(-2), -2 * FILETIME_SECOND);
    assert_eq!(filetime_ticks_from_seconds(0), 0);
}

#[test]
fn filetime_to_unix_nanoseconds_preserves_ticks_and_checks_overflow() {
    assert_eq!(unix_ns_from_filetime(FILETIME_UNIX_EPOCH), Some(0));
    assert_eq!(unix_ns_from_filetime(FILETIME_UNIX_EPOCH + 1), Some(100));
    assert_eq!(unix_ns_from_filetime(FILETIME_UNIX_EPOCH - 1), Some(-100));
    assert_eq!(unix_ns_from_filetime(infinity_core::consts::FAR_FUTURE), Some(4_132_224_000_000_000_000));
    assert_eq!(unix_ns_from_filetime(i64::MIN), None);
    assert_eq!(unix_ns_from_filetime(i64::MAX), None);
}

#[test]
fn filetime_to_unix_parts_normalizes_and_preserves_ticks() {
    let cases = [
        (FILETIME_UNIX_EPOCH, (0, 0)),
        (FILETIME_UNIX_EPOCH + 1, (0, 100)),
        (FILETIME_UNIX_EPOCH - 1, (-1, 999_999_900)),
        (FILETIME_UNIX_EPOCH + FILETIME_SECOND - 1, (0, 999_999_900)),
        (FILETIME_UNIX_EPOCH + FILETIME_SECOND + 1_234_567, (1, 123_456_700)),
        (0, (-11_644_473_600, 0)),
    ];
    for (ticks, expected) in cases {
        assert_eq!(unix_parts_from_filetime(ticks), expected);
    }

    for ticks in cases.map(|(ticks, _)| ticks).into_iter().chain([i64::MIN, i64::MAX]) {
        let (seconds, nanos) = unix_parts_from_filetime(ticks);
        assert!((0..1_000_000_000).contains(&nanos));
        assert_eq!(nanos % FILETIME_TICK_NS as i32, 0);
        assert_eq!(
            i128::from(seconds) * i128::from(FILETIME_SECOND)
                + i128::from(nanos) / i128::from(FILETIME_TICK_NS),
            i128::from(ticks) - i128::from(FILETIME_UNIX_EPOCH)
        );
    }
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn cancelled_run_detaches_account_sender_even_with_clones() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let world = crate::space::World::new(db.clone(), sde.clone(), 1);
    let cfg = Arc::new(Config::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let _peer = tokio::net::TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
    let (stream, address) = listener.accept().await.unwrap();
    let account = -81103;
    let login = Login { user_id: account, user_name: "test".into(), role: 0, client_id: 1, session_id: 1, language_id: "EN".into() };
    let chat = crate::chat::Hub::new(&cfg.chat.domain, db.clone(), world.clone());
    let client = Client::new(Conn::new(stream, address), cfg, db, sde, chat, world.clone(), login);
    let mut registration = crate::live_sessions::register(account, true).unwrap();
    registration.wait_for_predecessor().await;
    let retained_sender;
    {
        let run = client.run(&mut registration);
        tokio::pin!(run);
        tokio::select! {
            result = &mut run => panic!("client exited unexpectedly: {result:?}"),
            _ = tokio::time::sleep(std::time::Duration::from_millis(20)) => {}
        }
        retained_sender = crate::live_sessions::sender(account).unwrap();
    }
    assert!(crate::live_sessions::sender(account).is_none());
    assert!(retained_sender.is_closed());
    let mut outbox = Outbox::default();
    outbox.notify_sender(retained_sender, "event", vec![]);
    assert!(crate::db::outbox::dispatch_world((), outbox, &world).is_ok());
    assert!(crate::live_sessions::sender(account).is_none());
}

#[test]
fn filetime_timestamp_milliseconds_round_up() {
    assert_eq!(unix_ms_from_filetime_ceil(FILETIME_UNIX_EPOCH), 0);
    assert_eq!(unix_ms_from_filetime_ceil(FILETIME_UNIX_EPOCH + FILETIME_TICKS_PER_MS), 1);
    assert_eq!(unix_ms_from_filetime_ceil(FILETIME_UNIX_EPOCH + FILETIME_TICKS_PER_MS + 1), 2);
    assert_eq!(unix_ms_from_filetime_ceil(FILETIME_UNIX_EPOCH - FILETIME_TICKS_PER_MS), -1);
    assert_eq!(unix_ms_from_filetime_ceil(FILETIME_UNIX_EPOCH - FILETIME_TICKS_PER_MS + 1), 0);
}

#[test]
fn call_kwargs_strip_only_macho_version() {
    let mut kwargs = Value::Dict(vec![
        (Value::str("machoVersion"), Value::Int(496)),
        (Value::str("keep"), Value::Int(7)),
        (Value::Int(1), Value::str("non-text key")),
    ]);
    strip_macho_version(&mut kwargs);
    assert_eq!(
        kwargs,
        Value::Dict(vec![
            (Value::str("keep"), Value::Int(7)),
            (Value::Int(1), Value::str("non-text key")),
        ])
    );
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn wait_flushes_queued_notifications_and_forwards_later_destiny() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let world = crate::space::World::new(db.clone(), sde.clone(), 1);
    let system = -1;
    world.system(system).await.unwrap();
    let cfg = Arc::new(Config::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut peer = tokio::net::TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
    let (stream, address) = listener.accept().await.unwrap();
    let login = Login { user_id: 1, user_name: "test".into(), role: 0, client_id: 1, session_id: 1, language_id: "EN".into() };
    let chat = crate::chat::Hub::new(&cfg.chat.domain, db.clone(), world.clone());
    let mut client = Client::new(Conn::new(stream, address), cfg, db, sde, chat, world, login);
    client.notify("OnDockingAccepted", vec![Value::Int(42)]);
    let tx = client.destiny_tx.clone();
    let wait = client.wait_pumping(system, 2000);
    tokio::pin!(wait);
    let first = {
        let read_name = async {
            let frame = crate::framing::read_frame(&mut peer).await.unwrap().unwrap();
            let packet = Packet::from_value(&marshal::loads(&frame.payload).unwrap()).unwrap();
            match packet.destination {
                Address::Broadcast { broadcast_id, .. } => broadcast_id.as_text().unwrap().into_owned(),
                _ => panic!("expected broadcast"),
            }
        };
        tokio::pin!(read_name);
        tokio::select! {
            _ = &mut wait => panic!("timer completed before queued notification"),
            name = &mut read_name => name,
        }
    };
    assert_eq!(first, "OnDockingAccepted");
    tx.send(vec![Value::Int(99)]).unwrap();
    let second = tokio::select! {
        _ = &mut wait => panic!("timer completed before delayed destiny update"),
        frame = crate::framing::read_frame(&mut peer) => frame.unwrap().unwrap(),
    };
    let packet = Packet::from_value(&marshal::loads(&second.payload).unwrap()).unwrap();
    assert!(matches!(packet.destination, Address::Broadcast { broadcast_id, .. } if broadcast_id.as_text().as_deref() == Some("DoDestinyUpdate")));
    assert!(wait.await.unwrap());
}

#[test]
fn dogma_days_convert_to_whole_second_filetime() {
    assert_eq!(filetime_from_dogma_days(0.0), Some(FILETIME_UNIX_EPOCH));
    assert_eq!(filetime_from_dogma_days(0.5), Some(FILETIME_UNIX_EPOCH + FILETIME_DAY / 2));
    let seconds_per_day = FILETIME_DAY as f64 / FILETIME_SECOND as f64;
    assert_eq!(filetime_from_dogma_days(0.49 / seconds_per_day), Some(FILETIME_UNIX_EPOCH));
    assert_eq!(filetime_from_dogma_days(0.51 / seconds_per_day), Some(FILETIME_UNIX_EPOCH + FILETIME_SECOND));
    for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, f64::MAX, -f64::MAX] {
        assert_eq!(filetime_from_dogma_days(value), None);
    }
    assert_eq!(filetime_from_dogma_days(i64::MAX as f64 / FILETIME_DAY as f64), None);
}

#[test]
fn hours_convert_to_checked_filetime_ticks() {
    assert_eq!(filetime_ticks_from_hours(0.0), Some(0));
    assert_eq!(filetime_ticks_from_hours(1.5), Some(FILETIME_HOUR + FILETIME_HOUR / 2));
    assert_eq!(filetime_ticks_from_hours(-1.0), Some(-FILETIME_HOUR));
    for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, f64::MAX, -f64::MAX] {
        assert_eq!(filetime_ticks_from_hours(value), None);
    }
    assert_eq!(filetime_ticks_from_hours(i64::MAX as f64 / FILETIME_HOUR as f64), None);
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn leave_does_not_remove_or_save_stale_unpiloted_hull() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let ship_type = sde.types.iter().find(|t| t.category_id == Some(infinity_core::consts::CATEGORY_SHIP)).unwrap().type_id;
    let before = fixture::fingerprint(&db).await.unwrap();
    let fixture = fixture::create(&db, ship_type, 1).await.unwrap();
    let world = crate::space::World::new(db.clone(), sde.clone(), 1);
    let test_world = world.clone();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let row = crate::dao::items::space_ships::FreeShip {
            item_id: fixture.ship, type_id: ship_type, owner_id: fixture.character,
            corporation_id: 1, alliance_id: 0, pos: [1.0, 2.0, 3.0],
        };
        let prepared = services::ship::balls::unpiloted(&test_db, sde.clone(), &row).await.ok().unwrap();
        test_world.system(1).await.unwrap();
        test_world.with(1, move |s| prepared.install(s, |s, ball| s.add_ship(ball))).await.unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let _peer = tokio::net::TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
        let (stream, address) = listener.accept().await.unwrap();
        let cfg = Arc::new(Config::default());
        let login = Login { user_id: fixture.account, user_name: "fixture".into(), role: 0, client_id: 1, session_id: 1, language_id: "EN".into() };
        let mut client = Client::new(Conn::new(stream, address), cfg.clone(), test_db.clone(), sde, crate::chat::Hub::new(&cfg.chat.domain, test_db.clone(), test_world.clone()), test_world.clone(), login);
        client.change_session(vec![("userid", Value::Int(fixture.account)), ("charid", Value::Int(fixture.character)), ("shipid", Value::Int(fixture.ship)), ("solarsystemid", Value::Int(1))]);
        client.leave_world().await.unwrap();
        let events = crate::db::outbox::tests::events(&test_db, "CharacterLoggedOff", fixture.character).await;
        assert!(matches!(events.as_slice(), [crate::lua::events::GameEvent::CharacterLoggedOff { user_id, char_id }] if *user_id == fixture.account && *char_id == fixture.character));
        assert!(test_world.ball(1, fixture.ship).await.is_some());
        let count: i64 = test_db.query_one("SELECT count(*) AS count FROM game.item_attributes WHERE item_id = $1", &[&fixture.ship]).await.unwrap().get("count");
        assert_eq!(count, 0);
    }).await;
    world.remove_ball(1, fixture.ship).await;
    drop(world);
    db.execute("DELETE FROM game.progress_deliveries WHERE event_id IN (SELECT event_id FROM game.progress_events WHERE kind = 'CharacterLoggedOff' AND subject_character_id = $1)", &[&fixture.character]).await.unwrap();
    db.execute("DELETE FROM game.progress_events WHERE kind = 'CharacterLoggedOff' AND subject_character_id = $1", &[&fixture.character]).await.unwrap();
    fixture::cleanup(&db, fixture).await.unwrap();
    assert_eq!(fixture::fingerprint(&db).await.unwrap(), before);
    result.unwrap();
}

fn publication_ball(id: i64) -> crate::space::Ball {
    use crate::space::{Ball, MODE_STOP};
    Ball {
        id, type_id: 0, owner_id: 0, corp_id: -1, alliance_id: -1,
        char_id: None, mode: MODE_STOP, radius: 10.0, signature_radius: None,
        mass: 1_000_000.0, max_vel: 200.0, agility: 1.0, speed_fraction: 1.0,
        pos: [0.0; 3], vel: [100.0, 0.0, 0.0], goto: [0.0; 3], follow_id: 0,
        old_pos: [0.0; 3], old_vel: [100.0, 0.0, 0.0], segment_acceleration: None,
        follow_range: 0.0, free: true, global: false, massive: true, interactive: true,
        warp: Default::default(), last_ms: 0, interpolation_anchor_ms: 0, effect_stamp: 0,
        cloak: 0, cloak_until: 0, health: [1.0; infinity_core::consts::DAMAGE_LAYER_COUNT],
        launcher: 0, slim: vec![],
    }
}

fn publication_system() -> crate::space::System {
    let mut system = crate::space::System::new(1, Arc::default(), 0);
    system.insert(publication_ball(1));
    system.insert(publication_ball(2));
    system
}

fn publication_tick(system: &mut crate::space::System, ms: i64) {
    crate::space::system::tests::events::tick_with_collisions(system, ms, false);
}

fn publication_setter(system: &mut crate::space::System) {
    system.command(2, |ball, stamp, _| {
        ball.set_speed_fraction(0.5);
        vec![crate::space::event(stamp, "SetSpeedFraction", vec![Value::Int(ball.id), Value::Float(0.5)])]
    });
}

fn publication_warp(system: &mut crate::space::System) {
    system.command(1, |ball, _, _| {
        ball.warp_to([1_000_000.0, 0.0, 0.0], 0.0, 3000);
        Vec::new()
    });
}

fn publication_hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut result = String::with_capacity(bytes.len() * 2);
    for byte in bytes { write!(&mut result, "{byte:02x}").unwrap(); }
    result
}

fn publication_json(value: &Value) -> serde_json::Value {
    match value {
        Value::None => serde_json::Value::Null,
        Value::Bool(v) => serde_json::json!(v),
        Value::Int(v) => serde_json::json!(v),
        Value::Float(v) => serde_json::json!(v),
        Value::Bytes(v) => serde_json::json!(std::str::from_utf8(v).unwrap()),
        Value::Unicode(v) => serde_json::json!(v),
        Value::Tuple(v) | Value::List(v) => serde_json::Value::Array(v.iter().map(publication_json).collect()),
        other => panic!("unexpected destiny value: {other:?}"),
    }
}

struct PublicationFixture {
    client: Client,
    peer: tokio::net::TcpStream,
    trap: tokio::net::TcpListener,
    db: Db,
    packets: Vec<serde_json::Value>,
    notifications: Vec<serde_json::Value>,
    batches: Vec<Vec<Value>>,
}

impl PublicationFixture {
    async fn new() -> Self {
        crate::tests::init_config();
        let trap = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let db = Db::unconnected_publication_fixture(trap.local_addr().unwrap());
        let sde = Arc::new(crate::sde::Sde::default());
        let world = crate::space::World::new(db.clone(), sde.clone(), 1);
        let cfg = Arc::new(Config::default());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let peer = tokio::net::TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
        let (stream, address) = listener.accept().await.unwrap();
        let login = Login { user_id: 1, user_name: "test".into(), role: 0, client_id: 1, session_id: 1, language_id: "EN".into() };
        let chat = crate::chat::Hub::new(&cfg.chat.domain, db.clone(), world.clone());
        let client = Client::new(Conn::new(stream, address), cfg, db.clone(), sde, chat, world, login);
        Self { client, peer, trap, db, packets: Vec::new(), notifications: Vec::new(), batches: Vec::new() }
    }

    async fn drain(&mut self) {
        while let Ok(events) = self.client.destiny_rx.try_recv() {
            self.client.notify("DoDestinyUpdate", vec![Value::List(events.clone()), Value::Bool(false)]);
            let read = async {
                let mut received = Vec::new();
                let mut frames = Vec::new();
                while received.len() < events.len() {
                    let frame = crate::framing::read_frame(&mut self.peer).await.unwrap().unwrap();
                    let packet = Packet::from_value(&marshal::loads(&frame.payload).unwrap()).unwrap();
                    assert_eq!(packet.command, crate::macho::cmd::NOTIFICATION);
                    let Address::Broadcast { broadcast_id, .. } = &packet.destination else { panic!("broadcast expected") };
                    let name = broadcast_id.as_text().unwrap().into_owned();
                    assert_eq!(name, "DoDestinyUpdate");
                    let body = packet.body[0].items().unwrap();
                    assert_eq!(body[0].as_int(), Some(0));
                    let Value::Bytes(encoded) = &body[1] else { panic!("notification body") };
                    let payload = marshal::loads(encoded).unwrap();
                    let envelope = payload.items().unwrap();
                    assert_eq!(envelope[0].as_int(), Some(1));
                    let args = envelope[1].items().unwrap();
                    assert_eq!(args.len(), 2);
                    assert_eq!(args[1], Value::Bool(false));
                    let Value::List(batch) = &args[0] else { panic!("destiny batch expected") };
                    assert!(!batch.is_empty());
                    received.extend(batch.iter().cloned());
                    frames.push((frame.payload, packet.command, name, args.to_vec(), batch.clone()));
                }
                assert_eq!(received, events);
                frames
            };
            let (sent, frames) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
                tokio::join!(self.client.flush(), read)
            }).await.expect("publication flush or capture timed out");
            sent.unwrap();
            for (payload, command, name, args, batch) in frames {
                let index = self.packets.len();
                self.packets.push(serde_json::json!({
                    "packet_index": index, "received_at_filetime": filetime_now(),
                    "command": command, "notification_name": name,
                    "frame_payload_hex": publication_hex(&payload)
                }));
                self.notifications.push(serde_json::json!({
                    "packet_index": index, "name": name,
                    "args": args.iter().map(publication_json).collect::<Vec<_>>()
                }));
                self.batches.push(batch);
            }
        }
    }

    async fn finish(mut self, name: &str, initial: &Value, provenance: serde_json::Value) -> Vec<Vec<Value>> {
        self.client.conn.close().await.unwrap();
        self.db.assert_publication_fixture_unused();
        assert!(tokio::time::timeout(std::time::Duration::from_millis(50), self.trap.accept()).await.is_err(), "database trap contacted");
        assert!(!self.packets.is_empty());
        println!("WARP_CAPACITOR_CAPTURE {}", serde_json::json!({
            "name": name, "provenance": "raw-native-warp-publisher-regression",
            "scenario": provenance, "initial_set_state_marshal_hex": publication_hex(&marshal::dumps(initial)),
            "packets": self.packets, "notifications": self.notifications
        }));
        self.batches
    }
}

fn publication_single_stamp(batches: &[Vec<Value>]) -> bool {
    batches.iter().all(|batch| {
        let mut stamps = batch.iter().map(|event| event.items().unwrap()[0].as_int().unwrap());
        stamps.next().is_none_or(|first| stamps.all(|stamp| stamp == first))
    })
}

#[tokio::test]
async fn destiny_publication_single_stamp_tcp_control() {
    let mut fixture = PublicationFixture::new().await;
    let mut system = publication_system();
    let initial = system.observe(1, 1, fixture.client.destiny_tx.clone());
    publication_setter(&mut system);
    publication_setter(&mut system);
    publication_tick(&mut system, 1000);
    fixture.drain().await;
    let batches = fixture.finish("single-stamp-control", &initial, serde_json::json!({"kind": "raw-scalar-setters"})).await;
    assert_eq!(batches.len(), 1);
    assert_eq!(batches[0].len(), 2);
    assert!(publication_single_stamp(&batches));
}

#[tokio::test]
async fn destiny_publication_raw_exit_catchup_tcp_capture() {
    crate::tests::init_config();
    let mut discovery = publication_system();
    publication_warp(&mut discovery);
    let mut activation = None;
    let mut exit = None;
    for step in 1..=100 {
        publication_tick(&mut discovery, step * 1000);
        let ball = discovery.ball(1).unwrap();
        if ball.is_warping() && activation.is_none() { activation = Some(step); }
        if activation.is_some() && ball.mode == crate::space::MODE_STOP { exit = Some(step); break; }
    }
    let exit = exit.expect("native warp must activate and exit");
    let activation = activation.expect("native warp must activate");
    assert!(exit >= 5);
    let mut all = Vec::new();
    for (name, due) in [("exit", 1), ("due2", 2), ("due5", 5)] {
        let mut fixture = PublicationFixture::new().await;
        let mut system = publication_system();
        let initial = system.observe(1, 1, fixture.client.destiny_tx.clone());
        publication_warp(&mut system);
        let mut active = false;
        for step in 1..=(exit - due) {
            publication_setter(&mut system);
            publication_tick(&mut system, step * 1000);
            active |= system.ball(1).unwrap().is_warping();
            fixture.drain().await;
        }
        publication_setter(&mut system);
        let before = system.current_time();
        publication_tick(&mut system, exit * 1000);
        assert_eq!(system.current_time(), before + due);
        assert!(active || activation > exit - due);
        assert_eq!(system.ball(1).unwrap().mode, crate::space::MODE_STOP);
        fixture.drain().await;
        let batches = fixture.finish(name, &initial, serde_json::json!({
            "kind": "controlled-raw-native-warp", "activation_step": activation, "exit_step": exit, "due_steps": due,
            "clock_before": before, "clock_after": before + due
        })).await;
        assert!(batches.iter().flatten().any(|event| {
            event.items().unwrap()[1].items().unwrap()[0].as_text().as_deref() == Some("SetBallMassive")
        }), "missing native exit reset");
        all.extend(batches);
    }
    assert!(publication_single_stamp(&all), "destiny notification contains mixed stamps");
}

#[tokio::test]
async fn destiny_publication_groups_stamps_stably_and_preserves_other_payloads() {
    let mut fixture = PublicationFixture::new().await;
    let event = |stamp, marker| crate::space::event(stamp, "SetSpeedFraction", vec![Value::Int(marker)]);
    let state = publication_system().observe(1, 1, fixture.client.destiny_tx.clone());
    let cases = [
        (vec![state.clone()], false, vec![vec![state]]),
        (vec![event(1, 10), event(1, 11)], true, vec![vec![event(1, 10), event(1, 11)]]),
        (vec![event(2, 20), event(1, 10), event(2, 21), event(1, 11)], false,
            vec![vec![event(1, 10), event(1, 11)], vec![event(2, 20), event(2, 21)]]),
        (vec![], false, vec![vec![]]),
        (vec![Value::Int(99)], false, vec![vec![Value::Int(99)]]),
        (vec![event(2, 20), Value::Int(99), event(1, 10)], false,
            vec![vec![event(2, 20), Value::Int(99), event(1, 10)]]),
        (vec![Value::tuple([Value::Bool(true), Value::tuple([Value::str("Stop"), Value::Tuple(vec![])])]), event(2, 20)],
            false, vec![vec![Value::tuple([Value::Bool(true), Value::tuple([Value::str("Stop"), Value::Tuple(vec![])])]), event(2, 20)]]),
    ];
    for (events, wait, expected) in cases {
        fixture.client.notify("DoDestinyUpdate", vec![Value::List(events), Value::Bool(wait)]);
        let packets = std::mem::take(&mut fixture.client.outbox);
        assert_eq!(packets.len(), expected.len());
        for (packet, group) in packets.iter().zip(expected) {
            let encoded = packet.body[0].items().unwrap()[1].as_bytes().unwrap();
            let payload = marshal::loads(encoded).unwrap();
            let args = payload.items().unwrap()[1].items().unwrap();
            assert_eq!(args, &[Value::List(group), Value::Bool(wait)]);
        }
    }
    fixture.client.notify("Other", vec![Value::Int(42)]);
    let packet = fixture.client.outbox.pop().unwrap();
    let payload = marshal::loads(packet.body[0].items().unwrap()[1].as_bytes().unwrap()).unwrap();
    assert_eq!(payload.items().unwrap()[1].items().unwrap(), &[Value::Int(42)]);
    fixture.db.assert_publication_fixture_unused();
}
