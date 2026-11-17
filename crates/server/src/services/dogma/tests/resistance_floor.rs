//! Resonance-floor boundary at the godma wire: ShipGetInfo/GetAllInfo publish
//! the floored effective value while ItemGetInfo and the persisted BASE row
//! keep the raw override.
use super::*;
use std::sync::Arc;
use infinity_core::consts::{CATEGORY_SHIP, dogma::{ATTR_ARMOR_EM_DAMAGE_RESONANCE, ATTR_SHIELD_EM_DAMAGE_RESONANCE}};
use crate::services::tests::fixture::{cleanup_ship, create_ship, ShipLocation};


/// Effective float of `attr` in one item's godma info entry.
fn dogma_attribute(info: &Value, attr: i32) -> f64 {
    let Value::Instance(_, state) = info else { panic!("expected item info instance") };
    let Some(Value::Dict(attributes)) = state.get("attributes") else { panic!("expected attributes dict") };
    attributes.iter().find_map(|(key, value)| match (key, value) {
        (Value::Int(id), Value::Float(value)) if *id == i64::from(attr) => Some(*value),
        _ => None,
    }).expect("resonance attribute present")
}

/// The ship's info entry in a ShipGetInfo-style `{itemID: KeyVal}` dict.
fn ship_entry(info: &Value, ship: i64) -> &Value {
    let Value::Dict(entries) = info else { panic!("expected ship info dict") };
    &entries.iter().find(|(key, _)| *key == Value::Int(ship)).expect("ship entry present").1
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn ship_info_reports_floored_effective_and_unchanged_base() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let ship_type = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SHIP)
        && t.attribute(ATTR_SHIELD_EM_DAMAGE_RESONANCE).is_some_and(|v| v > 0.05)).unwrap().type_id;
    let native = sde.types.get(ship_type).unwrap().attribute(ATTR_SHIELD_EM_DAMAGE_RESONANCE).unwrap();
    let fixture = create_ship(&db, ship_type, ShipLocation::Space { system: 1 }, &[]).await.unwrap();
    assert!(crate::dao::items::dogma::set_override(&db, fixture.ship, ATTR_ARMOR_EM_DAMAGE_RESONANCE, 0.01).await.unwrap());
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let _peer = tokio::net::TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
        let (stream, address) = listener.accept().await.unwrap();
        let cfg = Arc::new(crate::config::Config::default());
        let world = crate::space::World::new(test_db.clone(), sde.clone(), 1);
        let login = crate::handshake::Login { user_id: fixture.account, user_name: "fixture".into(), role: 0, client_id: 1, session_id: 1, language_id: "EN".into() };
        let chat = crate::chat::Hub::new(&cfg.chat.domain, test_db.clone(), world.clone());
        let mut client = Client::new(infinity_wire::conn::Conn::new(stream, address), cfg.clone(), test_db.clone(), sde, chat, world, login);
        client.session.apply(vec![("charid", Value::Int(fixture.character)), ("shipid", Value::Int(fixture.ship))]);
        let floor = crate::config::game().combat.resonance_floor;
        assert_eq!(floor, 0.05);
        // ShipGetInfo publishes the floored effective resonance.
        let call = Call { service: "dogmaIM", method: "ShipGetInfo", args: &[],
            kwargs: &Value::None, bound: false, bind_params: None };
        let info = dispatch(&mut client, &call).await.unwrap();
        assert_eq!(dogma_attribute(ship_entry(&info, fixture.ship), ATTR_ARMOR_EM_DAMAGE_RESONANCE), floor);
        // A channel left at its type base reports the unchanged native value.
        assert_eq!(dogma_attribute(ship_entry(&info, fixture.ship), ATTR_SHIELD_EM_DAMAGE_RESONANCE), native);
        // GetAllInfo's embedded shipInfo dict reports the same effective value.
        let args = [Value::None, Value::Int(1), Value::None];
        let call = Call { service: "dogmaIM", method: "GetAllInfo", args: &args,
            kwargs: &Value::None, bound: false, bind_params: None };
        let all = dispatch(&mut client, &call).await.unwrap();
        let Value::Instance(_, state) = all else { panic!("expected GetAllInfo KeyVal") };
        let ship_info = state.get("shipInfo").expect("shipInfo key present");
        assert_eq!(dogma_attribute(ship_entry(ship_info, fixture.ship), ATTR_ARMOR_EM_DAMAGE_RESONANCE), floor);
        // ItemGetInfo keeps BASE: the same attribute reports the raw override.
        let args = [Value::Int(fixture.ship)];
        let call = Call { service: "dogmaIM", method: "ItemGetInfo", args: &args,
            kwargs: &Value::None, bound: false, bind_params: None };
        let item = dispatch(&mut client, &call).await.unwrap();
        assert_eq!(dogma_attribute(&item, ATTR_ARMOR_EM_DAMAGE_RESONANCE), 0.01);
    }).await;
    // The persisted override row was not rewritten by the info reads.
    let stored = crate::dao::items::dogma::overrides(&db, fixture.ship).await.unwrap();
    assert_eq!(stored.iter().find(|row| row.attribute_id == ATTR_ARMOR_EM_DAMAGE_RESONANCE)
        .expect("persisted resonance override").value, 0.01);
    let rows = crate::services::tests::fixture::capture_fixture_rows(&db, &[crate::services::tests::fixture::Fixture { account: fixture.account, character: fixture.character }], &[fixture.ship]).await.unwrap();
    cleanup_ship(&db, None, fixture).await.unwrap();
    crate::services::tests::fixture::assert_fixture_removed(&db, &rows).await.unwrap();
    result.unwrap();
}
