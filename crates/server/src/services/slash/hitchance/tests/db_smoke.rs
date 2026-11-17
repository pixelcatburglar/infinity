//! Database-backed slash oracle against sampled actor state and real SDE types.
use std::sync::Arc;
use infinity_core::{
    consts::{dogma::ATTR_FALLOFF, dogma::ATTR_MAX_RANGE, dogma::ATTR_OPTIMAL_SIG_RADIUS, dogma::ATTR_TRACKING_SPEED, CATEGORY_MODULE, CATEGORY_SHIP, FLAG_FITTING_FIRST, GM_ROLES},
};
use crate::{
    config::Config,
    db::Db,
    handshake::Login,
    sde::Sde,
    services::effects::{registry, EffectKind},
    space::{Ball, WarpState, World, MODE_STOP},
};
use infinity_wire::conn::Conn;
use marshal::Value;

use crate::services::tests::fixture::{cleanup_ship, create_ship, ShipItemSpec, ShipLocation};

fn ball(id: i64, type_id: i32, owner: i64, pos: [f64; 3], vel: [f64; 3], radius: f32) -> Ball {
    Ball { id, type_id, owner_id: owner, corp_id: 1, alliance_id: -1, char_id: None, signature_radius: Some(40.0),
        mode: MODE_STOP, radius, mass: 1e6, max_vel: 0.0, agility: 1.0, speed_fraction: 0.0,
        pos, vel, old_pos: pos, old_vel: vel, segment_acceleration: None,
        goto: pos, follow_id: 0, follow_range: 0.0, free: false, global: false, massive: false, interactive: false,
        warp: WarpState::default(), last_ms: 0, interpolation_anchor_ms: 0, effect_stamp: 0,
        cloak: 0, cloak_until: 0, health: [1.0; 3], launcher: 0, slim: Vec::new() }
}


#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn hit_chance_command_db_smoke() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let turret = sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_MODULE)
        && t.attribute(ATTR_OPTIMAL_SIG_RADIUS).is_some_and(|value| value > 0.0)
        && t.effects().iter().any(|e| registry::kind(&sde, e.effect_id) == Some(EffectKind::Turret)))
        .expect("turret").type_id;
    let ship_type = sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_SHIP)).expect("ship").type_id;
    let f = create_ship(&db, ship_type, ShipLocation::Docked { station: 1 }, &[
        ShipItemSpec { type_id: turret, flag_id: FLAG_FITTING_FIRST, quantity: 1, singleton: true, online: true, attributes: &[] },
    ]).await.unwrap();
    let module = f.module();
    let world = World::new(db.clone(), sde.clone(), 1);
    let test_world = world.clone();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        for (attr, value) in [(ATTR_TRACKING_SPEED, 0.01), (ATTR_OPTIMAL_SIG_RADIUS, 400.0), (ATTR_MAX_RANGE, 20_000.0), (ATTR_FALLOFF, 10_000.0)] {
            assert!(crate::dao::items::dogma::set_override(&test_db, module, attr, value).await.unwrap());
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let _peer = tokio::net::TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
        let (stream, addr) = listener.accept().await.unwrap();
        let login = Login { user_id: f.account, user_name: "fixture".into(), role: GM_ROLES, client_id: f.character, session_id: f.character, language_id: "EN".into() };
        let chat = crate::chat::Hub::new("localhost", test_db.clone(), test_world.clone());
        let mut client = crate::client::Client::new(Conn::new(stream, addr), Arc::new(Config::default()), test_db, sde, chat, test_world.clone(), login);
        client.session.apply(vec![("charid", Value::Int(f.character)), ("shipid", Value::Int(f.ship)), ("solarsystemid", Value::Int(1))]);
        let target = f.ship + 1_000_000;
        test_world.add_ship(1, ball(f.ship, ship_type, f.character, [0.0; 3], [0.0; 3], 100.0)).await.unwrap();
        for (distance, velocity, expected) in [(10_000.0, 0.0, 1.0), (10_000.0, 500.0, 0.0), (10_000.0, 10.0, 0.5), (30_110.0, 30.11, 0.25)] {
            test_world.add_ship(1, ball(target, ship_type, f.character, [distance, 0.0, 0.0], [0.0, velocity, 0.0], 10.0)).await.unwrap();
            let row = super::super::command(&mut client, &[target.to_string()]).await.ok().expect("hitchance command");
            assert!(row.contains(&module.to_string()));
            assert!(row.contains(&turret.to_string()));
            let values: Vec<f64> = row.split('=').skip(1).map(|part| {
                part.split(|c: char| !c.is_ascii_digit() && !matches!(c, 'e' | 'E' | '+' | '-' | '.'))
                    .next().expect("field").parse().expect("number")
            }).collect();
            assert_eq!(values.len(), 11);
            let surface = values[1];
            let angular_squared = values[4];
            let tracking = values[5];
            let opt_sig = values[6];
            let sig = values[7];
            let optimal = values[8];
            let falloff = values[9];
            let chance = values[10];
            let hand = 0.5_f64.powf(angular_squared * (opt_sig / (tracking * sig)).powi(2) + ((surface - optimal).max(0.0) / falloff).powi(2));
            assert!((chance - hand).abs() <= 1e-9 * hand.abs().max(f64::MIN_POSITIVE), "{row}");
            assert!((chance - expected).abs() < 1e-9, "{row}");
            test_world.remove_ball(1, target).await;
        }
    }).await;
    world.remove_ball(1, f.ship).await;
    world.remove_ball(1, f.ship + 1_000_000).await;
    let flush = world.flush_positions().await;
    drop(world);
    let rows = crate::services::tests::fixture::capture_fixture_rows(&db, &[crate::services::tests::fixture::Fixture { account: f.account, character: f.character }], &[f.ship, module]).await.unwrap();
    cleanup_ship(&db, None, f).await.unwrap();
    crate::services::tests::fixture::assert_fixture_removed(&db, &rows).await.unwrap();
    flush.unwrap();
    result.unwrap();
}
