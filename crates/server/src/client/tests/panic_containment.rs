//! A panic inside one request ends that session through ordinary leave
//! cleanup; the caller observes the supervisor's join error, never a fake result.
use super::*;

/// The one `CALL_REQ` source call ID that panics inside `Client::handle`;
/// every other request is answered normally.
const PANIC_CALL_ID: i64 = -9_000_001;

pub(crate) fn inject(p: &Packet) {
    if p.command != cmd::CALL_REQ { return; }
    if let Address::Client { call_id: Value::Int(id), .. } = &p.source {
        if *id == PANIC_CALL_ID { panic!("injected request panic"); }
    }
}

pub(super) fn call_req(source: Address, account: i64, service: &str, method: &str, args: Vec<Value>) -> Packet {
    let payload = Value::tuple([Value::Int(1), Value::str(method), Value::Tuple(args), Value::Dict(vec![])]);
    Packet::new(cmd::CALL_REQ, source, Address::Any { service: Some(service.into()), call_id: Value::None },
        Value::Int(account), vec![Value::tuple([Value::Int(0), Value::str(marshal::dumps(&payload))])])
}

pub(super) async fn response(peer: &mut Conn, source: &Address) -> Packet {
    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        loop {
            let value = peer.recv().await.unwrap().expect("connection closed before the call response");
            let packet = Packet::from_value(&value).unwrap();
            if packet.destination == *source && matches!(packet.command, cmd::CALL_RSP | cmd::ERRORRESPONSE) { break packet; }
        }
    }).await.expect("call response timed out")
}
pub(super) async fn select(peer: &mut Conn, account: i64, character: i64, call_id: i64) {
    let source = Address::Client { client_id: account, call_id: Value::Int(call_id), service: None };
    peer.send(&call_req(source.clone(), account, "charUnboundMgr", "SelectCharacterID", vec![Value::Int(character)]).to_value()).await.unwrap();
    let packet = response(peer, &source).await;
    assert!(packet.command == cmd::CALL_RSP,
        "SelectCharacterID refused for account {account} on client_id {account} call_id {call_id}: {}",
        describe(&packet));
}

/// Render a refused response for diagnosis: the decoded exception class and
/// client message when it is a wrapped error packet, the packet otherwise.
fn describe(packet: &Packet) -> String {
    let wrapped = packet.body.get(2).and_then(|body| body.items())
        .and_then(|items| items.first()).and_then(|value| value.as_bytes());
    if packet.command == cmd::ERRORRESPONSE {
        if let Some(Ok(exception)) = wrapped.map(marshal::loads) {
            return format!("command {} decoded exception {exception:?}", packet.command);
        }
    }
    format!("{packet:?}")
}

pub(super) async fn session(db: &Db, sde: &Arc<crate::sde::Sde>, world: &Arc<crate::space::World>, cfg: &Arc<Config>, account: i64)
    -> (Conn, crate::client::lifecycle::SessionRun)
{
    let login = Login { user_id: account, user_name: "panic".into(), role: 0, client_id: account, session_id: account, language_id: "EN".into() };
    let (_listener, peer, client) = crate::services::tests::connected_client(db.clone(), sde.clone(), world.clone(), cfg.clone(), login).await;
    let peer_address = peer.peer_addr().unwrap();
    let mut registration = crate::live_sessions::register(account, false).unwrap();
    registration.wait_for_predecessor().await;
    (infinity_wire::conn::Conn::new(peer, peer_address), client.run(registration))
}

async fn scenario(db: Db, sde: Arc<crate::sde::Sde>, world: Arc<crate::space::World>, cfg: Arc<Config>,
    fixtures: [crate::services::tests::fixture::Fixture; 2], ship_type: i32)
{
    let online_before = db.online_count();
    let mut systems = [0; 2];
    let mut ships = [0; 2];
    for (index, fixture) in fixtures.iter().enumerate() {
        systems[index] = db.query_one("SELECT solar_system_id FROM game.characters WHERE character_id=$1", &[&fixture.character]).await.unwrap().get("solar_system_id");
        ships[index] = crate::dao::items::creation::positioned(&db, ship_type, fixture.character, systems[index], [0.0; 3]).await.unwrap();
        crate::dao::characters::selection::set_ship(&db, fixture.character, ships[index]).await.unwrap();
    }
    let [a, b] = fixtures;
    let (mut peer_a, run_a) = session(&db, &sde, &world, &cfg, a.account).await;
    let (mut peer_b, run_b) = session(&db, &sde, &world, &cfg, b.account).await;
    select(&mut peer_a, a.account, a.character, 1).await;
    select(&mut peer_b, b.account, b.character, 1).await;
    assert_eq!(db.online_count(), online_before + 2);
    assert!(world.online_session(a.character).is_some());
    assert!(world.online_session(b.character).is_some());
    peer_a.send(&call_req(Address::Client { client_id: a.account, call_id: Value::Int(PANIC_CALL_ID), service: None },
        a.account, "charUnboundMgr", "GetCohortsForUser", vec![]).to_value()).await.unwrap();
    while let Some(value) = peer_a.recv().await.expect("peer transport failed before ordinary EOF") {
        assert!(value != infinity_wire::conn::usurped_close(), "a request panic sent the usurp close");
    }
    let error = run_a.await.unwrap_err();
    assert!(error.chain().any(|cause| cause.downcast_ref::<tokio::task::JoinError>().is_some_and(|join| join.is_panic())),
        "request panic did not reach the caller as a JoinError: {error:#}");
    assert_eq!(db.online_count(), online_before + 1);
    assert!(crate::live_sessions::sender(a.account).is_none());
    assert!(world.online_session(a.character).is_none());
    assert_eq!(world.with(systems[0], move |s| s.ball(ships[0]).is_some()).await, Some(false), "the panic left the ship in the actor");
    let logoffs = crate::db::outbox::tests::events(&db, "CharacterLoggedOff", a.character).await;
    assert!(matches!(logoffs.as_slice(), [crate::lua::events::GameEvent::CharacterLoggedOff { user_id, char_id }]
        if (*user_id, *char_id) == (a.account, a.character)), "expected exactly one logoff for the panicked session");
    // B is already selected: a normal RPC answers without reinstalling its ship.
    let cohorts = Address::Client { client_id: b.account, call_id: Value::Int(2), service: None };
    peer_b.send(&call_req(cohorts.clone(), b.account, "charUnboundMgr", "GetCohortsForUser", vec![]).to_value()).await.unwrap();
    assert_eq!(response(&mut peer_b, &cohorts).await.command, cmd::CALL_RSP);
    assert!(world.online_session(b.character).is_some());
    assert_eq!(world.with(systems[1], move |s| s.ball(ships[1]).is_some()).await, Some(true));
    let (mut peer_new, run_new) = session(&db, &sde, &world, &cfg, a.account).await;
    select(&mut peer_new, a.account, a.character, 3).await;
    assert!(world.online_session(a.character).is_some());
    assert_eq!(world.with(systems[0], move |s| s.ball(ships[0]).is_some()).await, Some(true));
    drop(peer_a);
    drop(peer_b);
    drop(peer_new);
    run_b.await.expect("B's session did not finish cleanly");
    run_new.await.expect("the new session did not finish cleanly");
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn request_panic_disconnects_and_releases_account() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let world = crate::space::World::new(db.clone(), sde.clone(), 1);
    let cfg = Arc::new(Config::default());
    let ship_type = sde.types.iter().find(|ty| ty.category_id == Some(infinity_core::consts::CATEGORY_SHIP)).unwrap().type_id;
    let fixtures = [crate::services::tests::fixture::create(&db).await.unwrap(), crate::services::tests::fixture::create(&db).await.unwrap()];
    let result = tokio::spawn(scenario(db.clone(), sde, world, cfg, fixtures, ship_type)).await;
    // Sessions dropped by a failed scenario clean up before fixture rows delete.
    for fixture in fixtures {
        let mut barrier = crate::live_sessions::register(fixture.account, true).unwrap();
        barrier.wait_for_predecessor().await;
        drop(barrier);
        crate::services::tests::fixture::cleanup(&db, fixture).await.unwrap();
    }
    result.unwrap();
}
