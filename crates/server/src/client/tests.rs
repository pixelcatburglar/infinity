use super::*;
use crate::dao::items::creation;

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
    assert_eq!(unix_ns_from_filetime(crate::consts::FAR_FUTURE), Some(4_132_224_000_000_000_000));
    assert_eq!(unix_ns_from_filetime(i64::MIN), None);
    assert_eq!(unix_ns_from_filetime(i64::MAX), None);
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
    let ship_type = sde.types.iter().find(|t| t.category_id == Some(crate::consts::CATEGORY_SHIP)).unwrap().type_id;
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
        test_world.add_ship(1, prepared.install()).await.unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let _peer = tokio::net::TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
        let (stream, address) = listener.accept().await.unwrap();
        let cfg = Arc::new(Config::default());
        let login = Login { user_id: fixture.account, user_name: "fixture".into(), role: 0, client_id: 1, session_id: 1, language_id: "EN".into() };
        let mut client = Client::new(Conn::new(stream, address), cfg.clone(), test_db.clone(), sde, crate::chat::Hub::new(&cfg.chat.domain, test_db.clone(), test_world.clone()), test_world.clone(), login);
        client.session.apply(vec![("charid", Value::Int(fixture.character)), ("shipid", Value::Int(fixture.ship)), ("solarsystemid", Value::Int(1))]);
        client.leave_world().await.unwrap();
        assert!(test_world.ball(1, fixture.ship).await.is_some());
        let count: i64 = test_db.query_one("SELECT count(*) AS count FROM game.item_attributes WHERE item_id = $1", &[&fixture.ship]).await.unwrap().get("count");
        assert_eq!(count, 0);
    }).await;
    world.remove_ball(1, fixture.ship).await;
    services::combat::invalidate_condition(fixture.ship);
    drop(world);
    fixture::cleanup(&db, fixture).await.unwrap();
    assert_eq!(fixture::fingerprint(&db).await.unwrap(), before);
    result.unwrap();
}
