use super::*;
use infinity_core::consts::CATEGORY_ASTEROID;
use crate::dao::asteroids::NewAsteroid;
use crate::services::mining::tests::publish_fixture;
use crate::space::{Ball, WarpState, MODE_STOP};

fn ship_ball(id: i64, type_id: i32) -> Ball {
    Ball { id, type_id, owner_id: 1, corp_id: 1, alliance_id: -1, char_id: None,
        signature_radius: None, mode: MODE_STOP, radius: 1.0, mass: 1.0, max_vel: 0.0,
        agility: 1.0, speed_fraction: 0.0, pos: [0.0; 3], vel: [0.0; 3],
        old_pos: [0.0; 3], old_vel: [0.0; 3], segment_acceleration: None, goto: [0.0; 3],
        follow_id: 0, follow_range: 0.0, free: false, global: false, massive: false,
        interactive: false, warp: WarpState::default(), last_ms: 0, interpolation_anchor_ms: 0,
        effect_stamp: 0, cloak: 0, cloak_until: 0, health: [1.0; 3], launcher: 0, slim: vec![] }
}

#[test]
fn survey_range_uses_largest_valid_fitted_scanner_or_integrated_range() {
    assert_eq!(scanner_range(42.0, [].into_iter()), 42.0);
    assert_eq!(scanner_range(42.0, [0.0, f64::NAN, 12.0, 23.0].into_iter()), 23.0);
    assert_eq!(scanner_range(42.0, [f64::INFINITY, -1.0].into_iter()), 42.0);
}

async fn fingerprint(db: &crate::db::Db) -> Vec<Vec<String>> {
    let mut result = Vec::new();
    for table in ["accounts", "characters", "items", "item_attributes", "asteroids"] {
        result.push(crate::services::drones::tests::fixture::table_fingerprint(db, table).await.unwrap());
    }
    result
}

#[tokio::test]
#[ignore = "requires a dedicated INFINITY_TEST_DATABASE_URL with loaded SDE; run serially"]
async fn perform_scan_uses_online_survey_range_then_integrated_range() {
    use infinity_core::{consts::{CATEGORY_MODULE, CATEGORY_SHIP, FLAG_HI_SLOT_FIRST}};
    use crate::{dao::items::{combat, dogma}, services::effects::{registry, EffectKind}};
    crate::tests::init_config();
    let mut config = crate::config::tests::test_database();
    config.schema_on_start = false;
    let db = Db::connect(&config).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let hull = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SHIP)).unwrap().type_id;
    let scanner = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_MODULE)
        && t.effects().iter().any(|e| registry::kind(&sde, e.effect_id) == Some(EffectKind::SurveyScan)))
        .expect("SDE survey scanner").type_id;
    let ore = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_ASTEROID)).expect("SDE asteroid").type_id;
    let configured = crate::config::game().mining.scanner_range_m;
    assert!(configured.is_finite() && configured > 0.0);
    let before = fingerprint(&db).await;
    let (account, character, ship, module): (i64, i64, i64, i64) = db.transaction(
        move |tx, _: &mut ()| Box::pin(async move {
            let account = tx.query_one("INSERT INTO game.accounts (user_name,password_hash,role) VALUES ('survey-fixture-' || nextval('game.accounts_user_id_seq')::text,$1,0) RETURNING user_id", &[&&b"fixture"[..]]).await?.get("user_id");
            let character = tx.query_one("INSERT INTO game.characters (user_id,name,race_id,bloodline_id,gender_id,corporation_id) VALUES ($1,'survey-fixture-' || nextval('game.characters_character_id_seq')::text,1,1,1,1) RETURNING character_id", &[&account]).await?.get("character_id");
            let ship = tx.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton,x,y,z) VALUES ($1,$2,1,0,1,true,0,0,0) RETURNING item_id", &[&hull, &character]).await?.get("item_id");
            let module = tx.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton,online) VALUES ($1,$2,$3,$4,1,true,true) RETURNING item_id", &[&scanner, &character, &ship, &FLAG_HI_SLOT_FIRST]).await?.get("item_id");
            Ok::<_, crate::db::Error>((account, character, ship, module))
        }), |fixture, ()| async move { Ok(fixture) }).await.unwrap();
    let system = -character;
    let inner = crate::dao::asteroids::insert_batch(&db, system, &[NewAsteroid { field: system, type_id: ore, quantity: 17, radius: 1.0, pos: [configured * 0.5, 0.0, 0.0] }]).await.unwrap()[0].id;
    let outer = crate::dao::asteroids::insert_batch(&db, system, &[NewAsteroid { field: system, type_id: ore, quantity: 29, radius: 1.0, pos: [configured * 0.9, 0.0, 0.0] }]).await.unwrap()[0].id;
    let world = World::new(db.clone(), sde.clone(), 1);
    let test_world = world.clone();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        assert!(dogma::set_override(&test_db, module, ATTR_SURVEY_SCAN_RANGE, configured * 0.75).await.unwrap());
        test_world.system(system).await.unwrap();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        test_world.register_pilot(character, tx);
        test_world.with(system, move |s| s.insert(ship_ball(ship, hull))).await.unwrap();
        publish_fixture(&test_db, &test_world, system, system).await;
        let cfg = Arc::new(crate::config::Config::default());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let _peer = tokio::net::TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
        let (stream, address) = listener.accept().await.unwrap();
        let login = crate::handshake::Login { user_id: account, user_name: "test".into(), role: 0, client_id: 1, session_id: 1, language_id: "EN".into() };
        let chat = crate::chat::Hub::new(&cfg.chat.domain, test_db.clone(), test_world.clone());
        let mut client = Client::new(crate::conn::Conn::new(stream, address), cfg, test_db.clone(), sde, chat, test_world, login);
        client.session.apply(vec![("charid", Value::Int(character)), ("shipid", Value::Int(ship)), ("solarsystemid", Value::Int(system))]);
        let expected_inner = Value::tuple([Value::Int(inner), Value::Int(ore as i64), Value::Int(17)]);
        let expected_outer = Value::tuple([Value::Int(outer), Value::Int(ore as i64), Value::Int(29)]);
        assert_eq!(perform_scan(&client).await, Value::List(vec![expected_inner.clone()]));
        assert_eq!(combat::set_online(&test_db, module, ship, character, FLAG_HI_SLOT_FIRST, scanner, hull, 1, 0, true, false).await.unwrap(), 1);
        let Value::List(rows) = perform_scan(&client).await else { panic!("expected scan results") };
        assert_eq!(rows.len(), 2);
        assert!(rows.contains(&expected_inner) && rows.contains(&expected_outer));
    }).await;
    let actor = world.asteroid_actor(system);
    for rock in [inner, outer] {
        db.execute("DELETE FROM game.asteroids WHERE asteroid_id=$1 AND system_id=$2 AND field_id=$3", &[&rock, &system, &system]).await.unwrap();
        if let Some(actor) = &actor {
            if actor.rock_field(rock).await == Some(system) {
                assert!(crate::space::asteroids::publish_depletion(&world, system, actor, rock, system, None, None).await);
            }
        }
    }
    world.remove_ball(system, ship).await;
    world.unregister_pilot_generation(character, None);
    let flush = world.flush_positions().await;
    drop(world);
    db.transaction(move |tx, _: &mut ()| Box::pin(async move {
        tx.execute("DELETE FROM game.item_attributes WHERE item_id IN (SELECT item_id FROM game.items WHERE owner_id=$1)", &[&character]).await?;
        tx.execute("DELETE FROM game.items WHERE owner_id=$1", &[&character]).await?;
        tx.execute("DELETE FROM game.characters WHERE character_id=$1 AND user_id=$2", &[&character, &account]).await?;
        tx.execute("DELETE FROM game.accounts WHERE user_id=$1", &[&account]).await?;
        Ok::<_, crate::db::Error>(())
    }), |(), ()| async move { Ok(()) }).await.unwrap();
    assert_eq!(fingerprint(&db).await, before);
    flush.unwrap();
    result.unwrap();
}
