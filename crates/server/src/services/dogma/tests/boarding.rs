//! Ship state assembly under the lifecycle lock.
use super::*;
use std::sync::Arc;

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn reconciled_ship_state_is_a_four_tuple_under_outer_lock() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let ship_type = sde.types.iter().find(|t| t.category_id == Some(infinity_core::consts::CATEGORY_SHIP)).unwrap().type_id;
    let before = fixture::fingerprint(&db).await.unwrap();
    let fixture = fixture::create(&db, ship_type, 1).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let _peer = tokio::net::TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
        let (stream, address) = listener.accept().await.unwrap();
        let cfg = Arc::new(crate::config::Config::default());
        let world = crate::space::World::new(test_db.clone(), sde.clone(), 1);
        let login = crate::handshake::Login { user_id: fixture.account, user_name: "fixture".into(), role: 0, client_id: 1, session_id: 1, language_id: "EN".into() };
        let chat = crate::chat::Hub::new(&cfg.chat.domain, test_db.clone(), world.clone());
        let mut client = Client::new(crate::conn::Conn::new(stream, address), cfg.clone(), test_db, sde, chat, world, login);
        assert!(ship_state_core(&client, fixture.ship).await.is_err());
        client.session.apply(vec![("charid", Value::Int(fixture.character)), ("shipid", Value::Int(fixture.ship))]);
        let _lock = crate::services::market::lock().await;
        let state = ship_state_core(&client, fixture.ship).await.ok().unwrap();
        let Value::Tuple(fields) = state else { panic!("expected ship state tuple") };
        assert_eq!(fields.len(), 4);
    }).await;
    fixture::cleanup(&db, fixture).await.unwrap();
    assert_eq!(fixture::fingerprint(&db).await.unwrap(), before);
    result.unwrap();
}
