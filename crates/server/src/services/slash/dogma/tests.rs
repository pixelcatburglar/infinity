use super::*;

#[test]
fn resolved_kinematic_overrides_follow_ball_validation() {
    for attr in [ATTR_AGILITY, ATTR_MASS, ATTR_MAX_VELOCITY] {
        let attr = attr as i32;
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -1.0] {
            assert!(!valid_kinematic_action(attr, Action::Set("name", value)));
        }
        assert!(valid_kinematic_action(attr, Action::Set("name", 1.0)));
        assert!(valid_kinematic_action(attr, Action::Query("name")));
        assert!(valid_kinematic_action(attr, Action::Reset("name")));
    }
    assert!(!valid_kinematic_action(ATTR_AGILITY as i32, Action::Set("id", 0.0)));
    assert!(!valid_kinematic_action(ATTR_AGILITY as i32, Action::Set("id", f64::MIN_POSITIVE)));
    assert!(!valid_kinematic_action(ATTR_AGILITY as i32, Action::Set("id", f64::MAX)));
    assert!(!valid_kinematic_action(ATTR_MASS as i32, Action::Set("id", 0.0)));
    assert!(valid_kinematic_action(ATTR_MASS as i32, Action::Set("id", f64::MAX)));
    assert!(valid_kinematic_action(ATTR_MAX_VELOCITY as i32, Action::Set("id", 0.0)));
    assert!(!valid_kinematic_action(ATTR_MAX_VELOCITY as i32, Action::Set("id", f64::MAX)));
    assert!(valid_kinematic_action(-1, Action::Set("other", -1.0)));
}

#[test]
fn drone_queries_select_only_the_session_owners_drone() {
    let mut item = Item {
        item_id: 42, type_id: 1, owner_id: 7, location_id: 1, flag_id: 1,
        quantity: 1, singleton: true, group_id: 1,
        category_id: i64::from(CATEGORY_DRONE), custom_info: None,
    };
    assert_eq!(drone_query_key(&item, Action::Query("speed"), Some(7)), Some(Key::Drone(42)));
    assert_eq!(drone_query_key(&item, Action::Query("speed"), Some(8)), None);
    assert_eq!(drone_query_key(&item, Action::Query("speed"), None), None);
    assert_eq!(drone_query_key(&item, Action::Set("speed", 2.0), Some(7)), None);
    assert_eq!(drone_query_key(&item, Action::Reset("speed"), Some(7)), None);
    assert_eq!(drone_query_key(&item, Action::List, Some(7)), None);
    item.category_id = i64::from(CATEGORY_MODULE);
    assert_eq!(drone_query_key(&item, Action::Query("speed"), Some(7)), None);
}

#[test]
fn live_drone_candidate_uses_item_location_and_rejects_bay_or_non_singletons() {
    let mut item = Item {
        item_id: 42, type_id: 1, owner_id: 7, location_id: 123, flag_id: 0,
        quantity: 1, singleton: true, group_id: 1,
        category_id: i64::from(CATEGORY_DRONE), custom_info: None,
    };
    assert_eq!(drone_location(&item), Some(123));
    item.location_id = 456;
    assert_eq!(drone_location(&item), Some(456));
    item.flag_id = 1;
    assert_eq!(drone_location(&item), None);
    item.flag_id = 0;
    item.singleton = false;
    assert_eq!(drone_location(&item), None);
    item.singleton = true;
    item.category_id = i64::from(CATEGORY_MODULE);
    assert_eq!(drone_location(&item), None);
}

#[test]
fn late_gm_snapshot_requests_refresh_without_overwriting_newer_projection() {
    crate::tests::init_config();
    use crate::space::{Ball, System, WarpState, MODE_STOP};
    use std::sync::Arc;

    let mut system = System::new(0, Arc::default(), 12_345);
    system.insert(Ball {
        id: 1, type_id: 1, owner_id: 1, corp_id: 1, alliance_id: -1,
        char_id: None, signature_radius: None, mode: MODE_STOP, radius: 1.0,
        mass: 1.0, max_vel: 1.0, agility: 1.0, speed_fraction: 0.0,
        pos: [0.0; 3], vel: [0.0; 3], goto: [0.0; 3], follow_id: 0,
        old_pos: [0.0; 3], old_vel: [0.0; 3], segment_acceleration: None,
        follow_range: 0.0, free: true, global: false, massive: false, interactive: false,
        warp: WarpState::default(), warp_factor: 0, last_ms: 0, interpolation_anchor_ms: 0,
        effect_stamp: 0, cloak: 0, cloak_until: 0, health: [1.0; 3], launcher: 0, slim: Vec::new(),
    });
    let old = system.projection_revision(1);
    system.refresh_projection_target(1);
    let newer = system.projection_revision(1);
    assert!(current_projection(&mut system, 1, newer));
    system.command(1, |ball, _, _| { ball.set_max_speed(10.0); Vec::new() });
    system.tick(13_000);
    assert_eq!(system.ball(1).unwrap().max_vel, 10.0);
    if current_projection(&mut system, 1, old) {
        system.command(1, |ball, _, _| { ball.set_max_speed(100.0); Vec::new() });
    }
    assert!(system.projection_revision(1) > newer);
    system.tick(14_000);
    assert_eq!(system.ball(1).unwrap().max_vel, 10.0);
}

async fn smoke_fingerprint(db: &crate::db::Db) -> Vec<String> {
    let mut result = Vec::new();
    for sql in [
        "SELECT md5(COALESCE(string_agg(row_to_json(t)::text, '' ORDER BY user_id), '')) AS fingerprint FROM game.accounts t",
        "SELECT md5(COALESCE(string_agg(row_to_json(t)::text, '' ORDER BY character_id), '')) AS fingerprint FROM game.characters t",
        "SELECT md5(COALESCE(string_agg(row_to_json(t)::text, '' ORDER BY item_id), '')) AS fingerprint FROM game.items t",
        "SELECT md5(COALESCE(string_agg(row_to_json(t)::text, '' ORDER BY item_id, attribute_id), '')) AS fingerprint FROM game.item_attributes t",
        "SELECT md5(COALESCE(string_agg(row_to_json(t)::text, '' ORDER BY ship_id, flag_id), '')) AS fingerprint FROM game.module_charges t",
        "SELECT md5(COALESCE(string_agg(row_to_json(t)::text, '' ORDER BY character_id, type_id), '')) AS fingerprint FROM game.skills t",
    ] {
        result.push(db.query_one(sql, &[]).await.unwrap().get("fingerprint"));
    }
    result
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn rejected_kinematic_commands_do_not_persist_notify_or_move() {
    use std::sync::Arc;
    use crate::space::{Ball, WarpState, World, MODE_STOP};
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let before = smoke_fingerprint(&db).await;
    let (account, character, ship) = db.transaction(|tx, _: &mut ()| Box::pin(async move {
        let account: i64 = tx.query_one("INSERT INTO game.accounts (user_name,password_hash,role) VALUES ('gm-refusal-' || nextval('game.accounts_user_id_seq')::text,$1,0) RETURNING user_id", &[&&b"fixture"[..]]).await?.get("user_id");
        let character: i64 = tx.query_one("INSERT INTO game.characters (user_id,name,race_id,bloodline_id,gender_id,corporation_id) VALUES ($1,'gm-refusal-' || nextval('game.characters_character_id_seq')::text,1,1,1,1) RETURNING character_id", &[&account]).await?.get("character_id");
        let ship: i64 = tx.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton,x,y,z) VALUES (587,$1,1,0,1,true,0,0,0) RETURNING item_id", &[&character]).await?.get("item_id");
        Ok::<_, crate::db::Error>((account, character, ship))
    }), |fixture, ()| async move { Ok(fixture) }).await.unwrap();
    let world = World::new(db.clone(), sde.clone(), 1);
    let w = world.clone();
    let d = db.clone();
    let result = tokio::spawn(async move {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let _peer = tokio::net::TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
        let (stream, address) = listener.accept().await.unwrap();
        let cfg = Arc::new(crate::config::Config::default());
        let login = crate::handshake::Login { user_id: account, user_name: "fixture".into(), role: 0, client_id: 1, session_id: 1, language_id: "EN".into() };
        let mut client = Client::new(crate::conn::Conn::new(stream, address), cfg.clone(), d.clone(), sde, crate::chat::Hub::new(&cfg.chat.domain, d.clone(), w.clone()), w.clone(), login);
        client.session.apply(vec![("charid", Value::Int(character)), ("shipid", Value::Int(ship)), ("solarsystemid", Value::Int(1))]);
        let ball = Ball { id: ship, type_id: 587, owner_id: character, corp_id: 1, alliance_id: -1,
            char_id: Some(character), signature_radius: None, mode: MODE_STOP, radius: 1.0, mass: 1e6,
            max_vel: 1.0, agility: 1.0, speed_fraction: 0.0, pos: [0.0; 3], vel: [0.0; 3],
            old_pos: [0.0; 3], old_vel: [0.0; 3], segment_acceleration: None, goto: [0.0; 3],
            follow_id: 0, follow_range: 0.0, free: false, global: false, massive: false, interactive: false,
            warp: WarpState::default(), warp_factor: 0, last_ms: 0, interpolation_anchor_ms: 0,
            effect_stamp: 0, cloak: 0, cloak_until: 0, health: [1.0; 3], launcher: 0, slim: Vec::new() };
        w.add_ship(1, ball).await.unwrap();
        assert!(w.after(1, 1200).await);
        let (destiny, mut events) = tokio::sync::mpsc::unbounded_channel();
        w.observe(1, character, ship, destiny).await.unwrap();
        let (notices, mut received) = tokio::sync::mpsc::unbounded_channel();
        client.notify_tx = notices.clone();
        w.register_pilot(character, notices);
        let initial = w.ball(1, ship).await.unwrap();
        for (name, id, values) in [
            ("agility", ATTR_AGILITY, &["0", "-1", "1e-300", "1e300"][..]),
            ("mass", ATTR_MASS, &["0", "-1"][..]),
            ("maxVelocity", ATTR_MAX_VELOCITY, &["-1", "1e300"][..]),
        ] {
            for spelling in [name.to_owned(), id.to_string()] {
                for value in values {
                    assert!(command(&mut client, &["ship".into(), spelling.clone(), (*value).into()]).await.is_err(), "{spelling} {value}");
                    println!("refused {spelling}={value}");
                    assert!(dao::overrides(&d, ship).await.unwrap().is_empty());
                }
            }
        }
        assert!(w.after(1, 1200).await);
        let after = w.ball(1, ship).await.unwrap();
        assert_eq!((after.mass, after.agility, after.max_vel, after.pos, after.vel), (initial.mass, initial.agility, initial.max_vel, initial.pos, initial.vel));
        assert!(events.try_recv().is_err());
        assert!(received.try_recv().is_err());
    }).await;
    world.unobserve(character);
    world.remove_ball(1, ship).await;
    world.unregister_pilot(character);
    let flush = world.flush_positions().await;
    crate::services::combat::forget_ship(ship);
    db.execute("DELETE FROM game.item_attributes WHERE item_id = $1", &[&ship]).await.unwrap();
    db.execute("DELETE FROM game.items WHERE item_id = $1 AND owner_id = $2", &[&ship, &character]).await.unwrap();
    db.execute("DELETE FROM game.characters WHERE character_id = $1 AND user_id = $2", &[&character, &account]).await.unwrap();
    db.execute("DELETE FROM game.accounts WHERE user_id = $1", &[&account]).await.unwrap();
    assert_eq!(smoke_fingerprint(&db).await, before);
    flush.unwrap();
    result.expect("GM refusal regression failed");
}
