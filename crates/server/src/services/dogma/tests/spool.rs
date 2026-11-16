//! Cold ship_info ramp-attribute prime, zeroed and then from the actor readout.
use super::*;
use std::sync::{Arc, atomic::AtomicBool};
use infinity_core::consts::{CATEGORY_MODULE, CATEGORY_SHIP, FILETIME_SECOND, FLAG_HI_SLOT_FIRST, dogma::{
    ATTR_CAPACITOR_CAPACITY, ATTR_DAMAGE_MULTIPLIER_BONUS_CURRENT, ATTR_DAMAGE_MULTIPLIER_BONUS_MAX_TIMESTAMP, EFFECT_TARGET_DISINTEGRATOR_ATTACK}};
use crate::services::combat::tests::remote_repair_db::ball;
use crate::services::tests::fixture::{cleanup_ship, create_ship, ShipItemSpec, ShipLocation};
use crate::space::World;

fn attribute(info: &[(Value, Value)], item: i64, id: i32) -> f64 {
    let (_, Value::Instance(_, fields)) = info.iter().find(|(key, _)| *key == Value::Int(item)).expect("item") else { panic!("KeyVal") };
    let Some(Value::Dict(attributes)) = fields.get("attributes") else { panic!("attributes") };
    let (_, Value::Float(value)) = attributes.iter().find(|(key, _)| *key == Value::Int(i64::from(id))).expect("attribute") else { panic!("float") };
    *value
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn ship_info_primes_ramp_attributes_from_the_actor_readout() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let find = |pred: fn(&crate::sde::types::TypeData) -> bool| sde.types.iter().find(|ty| pred(ty)).expect("SDE type").type_id;
    let ship_type = find(|ty| ty.category_id == Some(CATEGORY_SHIP) && ty.attribute(ATTR_CAPACITOR_CAPACITY).is_some_and(|v| v.is_finite() && v > 0.0));
    let module_type = find(|ty| ty.category_id == Some(CATEGORY_MODULE) && ty.published == Some(true)
        && ty.effects().iter().any(|effect| effect.effect_id == EFFECT_TARGET_DISINTEGRATOR_ATTACK));
    let fixture = create_ship(&db, ship_type, ShipLocation::Space { system: 1 }, &[ShipItemSpec {
        type_id: module_type, flag_id: FLAG_HI_SLOT_FIRST, quantity: 1, singleton: true, online: true, attributes: &[] }]).await.unwrap();
    let (module, world) = (fixture.items[0], World::new(db.clone(), sde.clone(), 1));
    let (test_world, test_db) = (world.clone(), db.clone());
    let result = tokio::spawn(async move {
        test_world.add_ship(1, ball(fixture.ship, ship_type, fixture.character)).await.unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let _peer = tokio::net::TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
        let (stream, address) = listener.accept().await.unwrap();
        let cfg = Arc::new(crate::config::Config::default());
        let login = crate::handshake::Login { user_id: fixture.account, user_name: "fixture".into(), role: 0, client_id: 1, session_id: 1, language_id: "EN".into() };
        let mut client = Client::new(infinity_wire::conn::Conn::new(stream, address), cfg.clone(), test_db.clone(), sde,
            crate::chat::Hub::new(&cfg.chat.domain, test_db.clone(), test_world.clone()), test_world.clone(), login);
        client.session.apply(vec![("charid", Value::Int(fixture.character)), ("shipid", Value::Int(fixture.ship)), ("solarsystemid", Value::Int(1))]);
        test_world.register_pilot(fixture.character, client.notify_tx.clone());
        let rows = |info: &[(Value, Value)]| (attribute(info, module, ATTR_DAMAGE_MULTIPLIER_BONUS_CURRENT), attribute(info, module, ATTR_DAMAGE_MULTIPLIER_BONUS_MAX_TIMESTAMP));
        let fit = fit::Fit::load_in_space(&test_db, &test_world, 1, fixture.character, fixture.ship).await.ok().expect("fit");
        let condition = combat::prepare_condition_with_fit(&test_db, fixture.ship, &fit).await.ok().expect("condition");
        test_world.with(1, move |system| {
            system.install_condition(fixture.ship, condition.into_condition(), fit);
        }).await.expect("system");
        assert_eq!(rows(&ship_info(&client).await.expect("cold")), (0.0, 0.0));
        let (stop, max_at) = (Arc::new(AtomicBool::new(false)), crate::client::filetime_now() + 90 * FILETIME_SECOND);
        test_world.with(1, move |system| {
            system.spool_readout_begin(fixture.ship, module, module, system.combat_membership_generation(fixture.ship).unwrap(), &stop, max_at, fixture.character);
            system.spool_readout_publish(fixture.ship, module, &stop, 0.21, max_at, fixture.character);
        }).await.expect("system");
        assert_eq!(rows(&ship_info(&client).await.expect("primed")), (0.21, max_at as f64));
    }).await;
    world.remove_ball(1, fixture.ship).await;
    world.unregister_pilot_generation(fixture.character, None);
    let flush = world.flush_positions().await;
    combat::forget_ship(fixture.ship);
    drop(world);
    let captured = crate::services::tests::fixture::capture_fixture_rows(&db,
        &[crate::services::tests::fixture::Fixture { account: fixture.account, character: fixture.character }], &[fixture.ship]).await.unwrap();
    cleanup_ship(&db, None, fixture).await.unwrap();
    crate::services::tests::fixture::assert_fixture_removed(&db, &captured).await.unwrap();
    flush.unwrap();
    result.unwrap();
}
