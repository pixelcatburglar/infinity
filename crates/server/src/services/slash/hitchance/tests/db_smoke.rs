//! Database-backed slash oracle against sampled actor state and real SDE types.
use std::sync::Arc;
use infinity_core::{
    consts::{dogma::ATTR_FALLOFF, dogma::ATTR_MAX_RANGE, dogma::ATTR_OPTIMAL_SIG_RADIUS, dogma::ATTR_TRACKING_SPEED, CATEGORY_MODULE, CATEGORY_SHIP, FLAG_FITTING_FIRST, GM_ROLES},
};
use crate::{
    config::Config,
    conn::Conn,
    db::Db,
    handshake::Login,
    sde::Sde,
    services::effects::{registry, EffectKind},
    space::{Ball, WarpState, World, MODE_STOP},
};
use marshal::Value;

#[derive(Clone, Copy)]
struct Fixture { account: i64, character: i64, ship: i64, module: i64 }

async fn fingerprint(db: &Db) -> Vec<String> {
    let mut result = Vec::new();
    for sql in [
        "SELECT md5(COALESCE(string_agg(row_to_json(t)::text, '' ORDER BY user_id), '')) AS fingerprint FROM game.accounts t",
        "SELECT md5(COALESCE(string_agg(row_to_json(t)::text, '' ORDER BY character_id), '')) AS fingerprint FROM game.characters t",
        "SELECT md5(COALESCE(string_agg(row_to_json(t)::text, '' ORDER BY item_id), '')) AS fingerprint FROM game.items t",
        "SELECT md5(COALESCE(string_agg(row_to_json(t)::text, '' ORDER BY item_id, attribute_id), '')) AS fingerprint FROM game.item_attributes t",
    ] { result.push(db.query_one(sql, &[]).await.unwrap().get("fingerprint")); }
    result
}

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
    let before = fingerprint(&db).await;
    let f = db.transaction(|tx, _: &mut ()| Box::pin(async move {
        let account = tx.query_one("INSERT INTO game.accounts (user_name, password_hash, role) VALUES ('hitchance-fixture-' || nextval('game.accounts_user_id_seq')::text, $1, 0) RETURNING user_id", &[&&b"fixture"[..]]).await?.get("user_id");
        let character = tx.query_one("INSERT INTO game.characters (user_id, name, race_id, bloodline_id, gender_id, corporation_id) VALUES ($1, 'hitchance-fixture-' || nextval('game.characters_character_id_seq')::text, 1, 1, 1, 1) RETURNING character_id", &[&account]).await?.get("character_id");
        let ship = tx.query_one("INSERT INTO game.items (type_id, owner_id, location_id, flag_id, quantity, singleton, x, y, z) VALUES ($1, $2, $3, 0, 1, true, 0, 0, 0) RETURNING item_id", &[&ship_type, &character, &1_i64]).await?.get("item_id");
        let module = tx.query_one("INSERT INTO game.items (type_id, owner_id, location_id, flag_id, quantity, singleton, online) VALUES ($1, $2, $3, $4, 1, true, true) RETURNING item_id", &[&turret, &character, &ship, &FLAG_FITTING_FIRST]).await?.get("item_id");
        Ok::<Fixture, crate::db::Error>(Fixture { account, character, ship, module })
    }), |f, ()| async move { Ok(f) }).await.unwrap();
    let world = World::new(db.clone(), sde.clone(), 1);
    let test_world = world.clone();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        for (attr, value) in [(ATTR_TRACKING_SPEED, 0.01), (ATTR_OPTIMAL_SIG_RADIUS, 400.0), (ATTR_MAX_RANGE, 20_000.0), (ATTR_FALLOFF, 10_000.0)] {
            assert!(crate::dao::items::dogma::set_override(&test_db, f.module, attr, value).await.unwrap());
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
            assert!(row.contains(&f.module.to_string()));
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
    db.transaction(|tx, _: &mut ()| Box::pin(async move {
        tx.execute("DELETE FROM game.item_attributes WHERE item_id = $1", &[&f.module]).await?;
        tx.execute("DELETE FROM game.items WHERE item_id = ANY($1)", &[&&[f.module, f.ship][..]]).await?;
        tx.execute("DELETE FROM game.characters WHERE character_id = $1 AND user_id = $2", &[&f.character, &f.account]).await?;
        tx.execute("DELETE FROM game.accounts WHERE user_id = $1", &[&f.account]).await?;
        Ok::<(), crate::db::Error>(())
    }), |(), ()| async move { Ok(()) }).await.unwrap();
    assert_eq!(fingerprint(&db).await, before);
    flush.unwrap();
    result.unwrap();
}
