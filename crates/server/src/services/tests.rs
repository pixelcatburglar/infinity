use super::*;
use std::sync::Arc;

pub(super) mod fixture;

pub(super) fn database_message(error: &anyhow::Error) -> Option<&str> {
    match error.downcast_ref::<crate::db::Error>()? {
        crate::db::Error::Postgres(error) => error.as_db_error().map(|error| error.message()),
        _ => None,
    }
}

pub(super) async fn client(db: crate::db::Db, character: i64) -> (Client, tokio::net::TcpStream) {
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let world = crate::space::World::new(db.clone(), sde.clone(), 1);
    let cfg = Arc::new(crate::config::Config::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let peer = tokio::net::TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
    let (stream, address) = listener.accept().await.unwrap();
    let conn = crate::conn::Conn::new(stream, address);
    let login = crate::handshake::Login { user_id: 1, user_name: "test".into(), role: 0, client_id: 1, session_id: 1, language_id: "EN".into() };
    let chat = crate::chat::Hub::new(&cfg.chat.domain, db.clone(), world.clone());
    let mut client = Client::new(conn, cfg, db, sde, chat, world, login);
    client.session.apply(vec![("charid", Value::Int(character))]);
    (client, peer)
}

#[test]
fn unit_from_bits_covers_unit_interval() {
    assert_eq!(unit_from_bits(0), 0.0);
    let max = unit_from_bits(u64::MAX);
    assert!(max < 1.0 && max > 0.999);
    assert_eq!(unit_from_bits(1u64 << 63), 0.5);
    assert_eq!(unit_from_bits(0x4000_0000_0000_0000), 0.25);
}

#[test]
fn surface_offset_includes_radius() {
    let center = [1.0e9, -2.0e9, 3.0e9];
    for radius in [0.0, 6_000_000.0, 1.0e12] {
        let clearance = 1_000_000.0;
        let actual = crate::space::dist(center, off_surface(center, radius, clearance));
        let expected = radius + clearance;
        assert!((actual - expected).abs() < expected * 1e-12, "{actual} != {expected}");
    }
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn item_locking_dispatch_returns_empty_collections() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let (mut client, _peer) = client(db, 1).await;
    let kwargs = Value::Dict(vec![]);
    let location_args = [Value::Int(1)];
    for (method, args, expected) in [
        ("GetItemsByLocation", &location_args[..], Value::Dict(vec![])),
        ("GetLockedItemLocations", &[][..], Value::reduce("__builtin__.set", vec![Value::list([])], None)),
    ] {
        let call = Call { service: "itemLocking", method, args, kwargs: &kwargs, bound: false, bind_params: None };
        let reply = dispatch(&mut client, &call).await.unwrap();
        assert_eq!(reply, expected);
        assert_eq!(marshal::loads(&marshal::dumps(&reply)).unwrap(), expected);
    }
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn item_fixed_dispatch_roundtrips_alpha_and_decode_responses() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let (mut client, _peer) = client(db, 1).await;
    let kwargs = Value::Dict(vec![]);

    let alpha = Call { service: "alphaInjectorMgr", method: "GetNextAvailableInjection", args: &[], kwargs: &kwargs, bound: false, bind_params: None };
    let alpha_reply = dispatch(&mut client, &alpha).await.unwrap();
    assert_eq!(alpha_reply, Value::Int(0));
    assert_eq!(marshal::loads(&marshal::dumps(&alpha_reply)).unwrap(), Value::Int(0));

    let item_id = [Value::Int(1)];
    let decode = Call { service: "encodedItems", method: "DecodeItem", args: &item_id, kwargs: &kwargs, bound: false, bind_params: None };
    let decode_error = dispatch(&mut client, &decode).await.unwrap_err();
    let CallError::User { msg, dict } = decode_error else { panic!("expected CustomNotify refusal") };
    assert_eq!(msg, crate::messages::MSG_CUSTOM_NOTIFY);
    assert_eq!(dict, vec![("notify".into(), Value::unicode(crate::messages::encoded_items::DECODE_UNAVAILABLE))]);
    let wire_error = crate::client::user_error(&msg, dict);
    assert_eq!(marshal::loads(&marshal::dumps(&wire_error)).unwrap(), wire_error);
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn character_selection_fixed_dispatch_without_selected_character() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let (mut client, _peer) = client(db, 0).await;
    client.session.apply(vec![("charid", Value::None)]);
    assert_eq!(client.session.int("charid"), None);
    let kwargs = Value::Dict(vec![]);
    for method in ["GetQAStarterSystemIDs", "GetCharacterInfo"] {
        let call = Call { service: "charUnboundMgr", method, args: &[], kwargs: &kwargs, bound: false, bind_params: None };
        let reply = dispatch(&mut client, &call).await.unwrap();
        assert_eq!(reply, Value::list([]));
        assert_eq!(marshal::loads(&marshal::dumps(&reply)).unwrap(), Value::list([]));
    }
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn corporate_mining_ledger_fixed_dispatch_returns_empty_lists() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let (mut client, _peer) = client(db, 1).await;
    let kwargs = Value::Dict(vec![]);
    let stale_structure_args = [Value::Int(1)];
    for (method, args) in [
        ("GetObserversWithMiningEvents", &[][..]),
        ("GetObserverLedger", &stale_structure_args[..]),
    ] {
        let call = Call { service: "corpMiningLedger", method, args, kwargs: &kwargs, bound: false, bind_params: None };
        let reply = dispatch(&mut client, &call).await.unwrap();
        assert_eq!(reply, Value::list([]));
        assert_eq!(marshal::loads(&marshal::dumps(&reply)).unwrap(), Value::list([]));
    }
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn mobile_binding_callback_dispatch_returns_none() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let (mut client, _peer) = client(db, 1).await;
    let kwargs = Value::Dict(vec![]);
    for aeid in ["fictional-aeid-one", "fictional-aeid-two"] {
        let args = [Value::str(aeid)];
        let call = Call { service: "mobileCollectMgr", method: "MobileBindSucceed", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
        let reply = dispatch(&mut client, &call).await.unwrap();
        assert_eq!(reply, Value::None);
    }
}
