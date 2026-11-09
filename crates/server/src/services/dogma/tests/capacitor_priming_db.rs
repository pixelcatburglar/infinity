//! Opt-in database regression for the ShipGetInfo capacitor priming path.
use super::*;
use std::sync::Arc;
use infinity_core::{consts::{dogma::ATTR_RECHARGE_RATE, CATEGORY_SHIP, FILETIME_SECOND}};
use crate::{client::filetime_now, space::{Ball, WarpState, MODE_STOP, World}};

fn ball(id: i64, type_id: i32, pilot: i64) -> Ball {
    Ball {
        id, type_id, owner_id: pilot, corp_id: 1, alliance_id: -1,
        char_id: Some(pilot), signature_radius: None, mode: MODE_STOP, radius: 1.0,
        mass: 1e6, max_vel: 0.0, agility: 1.0, speed_fraction: 0.0,
        pos: [0.0; 3], vel: [0.0; 3], old_pos: [0.0; 3], old_vel: [0.0; 3],
        segment_acceleration: None, goto: [0.0; 3], follow_id: 0, follow_range: 0.0,
        free: false, global: false, massive: false, interactive: false,
        warp: WarpState::default(), last_ms: 0, interpolation_anchor_ms: 0,
        effect_stamp: 0, cloak: 0, cloak_until: 0, health: [1.0; 3], launcher: 0, slim: Vec::new(),
    }
}

fn charge(info: &[(Value, Value)], ship: i64) -> f64 {
    let (_, Value::Instance(_, fields)) = info.iter().find(|(key, _)| *key == Value::Int(ship)).expect("ship info") else { panic!("expected ship KeyVal") };
    let Some(Value::Dict(attributes)) = fields.get("attributes") else { panic!("expected attributes") };
    let (_, Value::Float(value)) = attributes.iter().find(|(key, _)| *key == Value::Int(ATTR_CHARGE as i64)).expect("charge") else { panic!("expected float charge") };
    *value
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn ship_get_info_priming_does_not_refill_capacitor() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let ship_type = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SHIP)
        && t.attribute(ATTR_CAPACITOR_CAPACITY).is_some_and(|v| v.is_finite() && v > 0.0)
        && t.attribute(ATTR_RECHARGE_RATE).is_some_and(|v| v.is_finite() && v >= 60_000.0))
        .expect("SDE must contain a ship with capacitor and recharge time at least one minute").type_id;
    let system = 1;
    let before = fixture::fingerprint(&db).await.unwrap();
    let fixture = fixture::create(&db, ship_type, system).await.unwrap();
    let world = World::new(db.clone(), sde.clone(), 1);
    let test_world = world.clone();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        test_world.add_ship(system, ball(fixture.ship, ship_type, fixture.character)).await.unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let _peer = tokio::net::TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
        let (stream, address) = listener.accept().await.unwrap();
        let cfg = Arc::new(crate::config::Config::default());
        let login = crate::handshake::Login { user_id: fixture.account, user_name: "fixture".into(), role: 0, client_id: 1, session_id: 1, language_id: "EN".into() };
        let mut client = Client::new(crate::conn::Conn::new(stream, address), cfg.clone(), test_db.clone(), sde, crate::chat::Hub::new(&cfg.chat.domain, test_db.clone(), test_world.clone()), test_world.clone(), login);
        client.session.apply(vec![("charid", Value::Int(fixture.character)), ("shipid", Value::Int(fixture.ship)), ("solarsystemid", Value::Int(system))]);
        let fit = fit::Fit::load_in_space(&test_db, &test_world, system, fixture.character, fixture.ship).await.ok().expect("fit");
        let capacity = fit.value(fit::Key::Ship, ATTR_CAPACITOR_CAPACITY);
        let tau = fit.value(fit::Key::Ship, ATTR_RECHARGE_RATE);
        assert!(capacity.is_finite() && capacity > 0.0 && tau.is_finite() && tau > 0.0);
        let condition = combat::prepare_condition_with_fit(&test_db, fixture.ship, &fit).await.expect("prepare ship condition");
        test_world.with(system, move |s| {
            s.install_condition(fixture.ship, condition.into_condition(), fit);
            assert_eq!(s.pilot_ball(fixture.character).map(|ball| ball.id), Some(fixture.ship));
            assert!(s.ship_condition(fixture.ship).is_some());
        }).await.expect("ship system");
        let start = filetime_now();
        let drained = combat::groups_health::tests::drain_exact(&test_db, &test_world, system, fixture.ship, capacity * 2.0).await.ok().expect("drain");
        assert_eq!(drained, capacity);
        for _ in 0..2 {
            let primed = ship_info(&client).await.expect("ShipGetInfo priming");
            let reported = charge(&primed, fixture.ship);
            let snapshot = combat::groups_health::capacitor_snapshot(&test_db, &test_world, system, fixture.ship).await.ok().expect("snapshot").expect("ship present");
            let elapsed = filetime_now().saturating_sub(start).max(0) as f64 / FILETIME_SECOND as f64;
            let bound = capacity * (1.0 - (-5.0 * elapsed / (tau / 1000.0)).exp()).powi(2);
            let tolerance = (capacity * 1e-12).max(1e-12);
            assert!(bound + tolerance < capacity, "test ran too long for recharge bound");
            assert!(reported >= 0.0 && reported <= bound + tolerance, "priming refilled reported charge");
            assert!(snapshot.level >= reported - tolerance && snapshot.level <= bound + tolerance, "priming refilled authoritative state");
        }
    }).await;
    world.remove_ball(system, fixture.ship).await;
    world.unregister_pilot_generation(fixture.character, None);
    let flush = world.flush_positions().await;
    combat::forget_ship(fixture.ship);
    drop(world);
    fixture::cleanup(&db, fixture).await.unwrap();
    assert_eq!(fixture::fingerprint(&db).await.unwrap(), before);
    flush.unwrap();
    result.unwrap();
}
