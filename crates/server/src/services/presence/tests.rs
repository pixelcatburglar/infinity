use crate::client::Client;
use crate::conn::Conn;
use crate::handshake::Login;
use marshal::Value;
use std::sync::Arc;

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated test database with initialized SDE"]
async fn session_changes_keep_channel_and_complete_chat_metadata() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let world = crate::space::World::new(db.clone(), sde.clone(), 1);
    let cfg = Arc::new(crate::config::Config::default());
    let chat = crate::chat::Hub::new(&cfg.chat.domain, db.clone(), world.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let _peer = tokio::net::TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
    let (stream, address) = listener.accept().await.unwrap();
    let login = Login { user_id: 1, user_name: "test".into(), role: 0, client_id: 1, session_id: 1, language_id: "EN".into() };
    let mut client = Client::new(Conn::new(stream, address), cfg, db, sde, chat, world.clone(), login);
    let (original_tx, mut original_rx) = tokio::sync::mpsc::unbounded_channel();
    client.notify_tx = original_tx;
    client.change_session(vec![
        ("charid", Value::Int(1)), ("corpid", Value::Int(2)),
        ("solarsystemid2", Value::Int(3)), ("allianceid", Value::Int(4)),
        ("warfactionid", Value::Int(5)), ("fleetid", Value::Int(6)),
        ("role", Value::Int(7)), ("corprole", Value::Int(8)),
    ]);
    let snapshot = world.chat_session(1).unwrap();
    assert_eq!((snapshot.system_id, snapshot.corporation_id, snapshot.alliance_id), (Some(3), Some(2), Some(4)));
    assert_eq!((snapshot.faction_id, snapshot.fleet_id, snapshot.role, snapshot.corporation_role), (Some(5), Some(6), 7, 8));
    let (replacement_tx, mut replacement_rx) = tokio::sync::mpsc::unbounded_channel();
    client.notify_tx = replacement_tx;
    client.change_session(vec![("solarsystemid2", Value::Int(9))]);
    let updated = world.chat_session(1).unwrap();
    assert_eq!(updated.system_id, Some(9));
    assert_eq!((updated.alliance_id, updated.faction_id, updated.fleet_id, updated.role, updated.corporation_role), (Some(4), Some(5), Some(6), 7, 8));
    world.notify_pilot(1, "test", vec![]);
    assert_eq!(original_rx.try_recv().unwrap().0, "test");
    assert!(replacement_rx.try_recv().is_err());
    client.change_session(vec![("charid", Value::Int(10))]);
    assert!(!world.is_online(1));
    assert!(world.chat_session(1).is_none());
    assert_eq!(world.chat_session(10).unwrap().character_id, 10);
    world.notify_pilot(10, "replacement", vec![]);
    assert_eq!(replacement_rx.try_recv().unwrap().0, "replacement");
    client.change_session(vec![("charid", Value::None)]);
    assert!(!world.is_online(10));
    assert!(world.chat_session(10).is_none());
}
