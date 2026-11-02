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

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn prepare_restores_docked_and_space_pilots_without_a_connection() {
    use crate::{config::deployment::DatabaseConfig, consts::{FLAG_HANGAR, TYPE_CAPSULE}, dao::{characters, items}, db::Db, sde::Sde, space::{World, CLOAK_RESTORE}};
    crate::tests::init_config();
    let db = Db::connect(&DatabaseConfig { schema_on_start: false, ..crate::config::tests::test_database() }).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let station = sde.map.stations().next().unwrap().station_id;
    let system_row = sde.map.systems().find(|s| s.constellation_id.is_some() && s.region_id.is_some()).unwrap();
    let (system, constellation, region) = (system_row.solar_system_id, system_row.constellation_id.unwrap(), system_row.region_id.unwrap());
    let account: i64 = db.query_one("INSERT INTO game.accounts (user_name,password_hash,role) VALUES (nextval('game.accounts_user_id_seq')::text,''::bytea,0) RETURNING user_id", &[]).await.unwrap().get("user_id");
    let docked: i64 = db.query_one("INSERT INTO game.characters (user_id,name,race_id,bloodline_id,gender_id,corporation_id,station_id,solar_system_id,constellation_id,region_id) VALUES ($1,nextval('game.characters_character_id_seq')::text,1,1,1,1,$2,$3,$4,$5) RETURNING character_id", &[&account, &station, &system, &constellation, &region]).await.unwrap().get("character_id");
    let space: i64 = db.query_one("INSERT INTO game.characters (user_id,name,race_id,bloodline_id,gender_id,corporation_id,station_id,solar_system_id,constellation_id,region_id) VALUES ($1,nextval('game.characters_character_id_seq')::text,1,1,1,1,NULL,$2,$3,$4) RETURNING character_id", &[&account, &system, &constellation, &region]).await.unwrap().get("character_id");
    let world = World::new(db.clone(), sde.clone(), 1);
    let test_db = db.clone();
    let test_world = world.clone();
    let result = tokio::spawn(async move {
        let pos = [1234.0, 5678.0, 9012.0];
        let ship = items::creation::positioned(&test_db, TYPE_CAPSULE, space, system, pos).await.unwrap();
        characters::selection::set_ship(&test_db, space, ship).await.unwrap();
        let before = test_db.query("SELECT source, source_id FROM game.map_events WHERE solar_system_id=$1 ORDER BY source, source_id", &[&system]).await.unwrap()
            .iter().map(|r| (r.get::<_, i16>("source"), r.get::<_, i64>("source_id"))).collect::<Vec<_>>();
        let selected = characters::selection::selected(&test_db, docked, account).await.unwrap().unwrap();
        let prepared = super::prepare(&test_db, &test_world, &sde, docked, &selected, |n| n.dispatch(&test_world, None)).await.unwrap();
        assert_eq!(prepared.cloak_until, None);
        let hull = test_db.query_one("SELECT type_id, owner_id, location_id, flag_id FROM game.items WHERE item_id=$1", &[&prepared.ship_id]).await.unwrap();
        assert_eq!((hull.get::<_, i32>("type_id"), hull.get::<_, i64>("owner_id"), hull.get::<_, i64>("location_id"), hull.get::<_, i32>("flag_id")), (601, docked, station, FLAG_HANGAR));
        let mut fit = test_db.query("SELECT type_id, flag_id, quantity FROM game.items WHERE location_id=$1 AND owner_id=$2", &[&prepared.ship_id, &docked]).await.unwrap()
            .iter().map(|r| (r.get::<_, i32>("type_id"), r.get::<_, i32>("flag_id"), r.get::<_, i32>("quantity"))).collect::<Vec<_>>();
        fit.sort_unstable();
        let mut expected_fit = vec![(3638,27,1),(3651,28,1),(10039,19,1),(21857,20,1),(30839,11,1),(32465,87,1)];
        expected_fit.sort_unstable();
        assert_eq!(fit, expected_fit);
        let mut skills = test_db.query("SELECT type_id, level FROM game.skills WHERE character_id=$1", &[&docked]).await.unwrap()
            .iter().map(|r| (r.get::<_, i32>("type_id"), r.get::<_, i32>("level"))).collect::<Vec<_>>();
        let mut expected_skills = sde.characters.race(1).unwrap().skills.iter().map(|s| (s.type_id, s.level)).collect::<Vec<_>>();
        skills.sort_unstable();
        expected_skills.sort_unstable();
        assert_eq!(skills, expected_skills);
        let selected = characters::selection::selected(&test_db, space, account).await.unwrap().unwrap();
        let start = crate::client::filetime_now();
        let prepared = super::prepare(&test_db, &test_world, &sde, space, &selected, |n| n.dispatch(&test_world, None)).await.unwrap();
        assert_eq!(prepared.ship_id, ship);
        let ball = test_world.with(system, move |s| s.pilot_ball(space).cloned()).await.unwrap().unwrap();
        assert_eq!(ball.pos, pos);
        assert_eq!(ball.cloak, CLOAK_RESTORE);
        assert_eq!(prepared.cloak_until, Some(ball.cloak_until));
        assert!(ball.cloak_until >= start + 60 * crate::consts::FILETIME_SECOND);
        for character in [docked, space] {
            assert!(test_db.query("SELECT 1 FROM game.map_visits WHERE character_id=$1", &[&character]).await.unwrap().is_empty());
        }
        let after = test_db.query("SELECT source, source_id FROM game.map_events WHERE solar_system_id=$1 ORDER BY source, source_id", &[&system]).await.unwrap()
            .iter().map(|r| (r.get::<_, i16>("source"), r.get::<_, i64>("source_id"))).collect::<Vec<_>>();
        assert_eq!(after, before);
    }).await;
    for character in [docked, space] {
        let owned = db.query("SELECT item_id FROM game.items WHERE owner_id=$1", &[&character]).await.unwrap();
        for row in &owned {
            let id: i64 = row.get("item_id");
            world.remove_ball(system, id).await;
            db.execute("DELETE FROM game.item_attributes WHERE item_id=$1", &[&id]).await.unwrap();
        }
        db.execute("UPDATE game.characters SET ship_id=NULL WHERE character_id=$1", &[&character]).await.unwrap();
        for row in &owned {
            let id: i64 = row.get("item_id");
            db.execute("DELETE FROM game.items WHERE location_id=$1 AND owner_id=$2", &[&id, &character]).await.unwrap();
        }
        db.execute("DELETE FROM game.items WHERE owner_id=$1", &[&character]).await.unwrap();
        db.execute("DELETE FROM game.skills WHERE character_id=$1", &[&character]).await.unwrap();
        db.execute("DELETE FROM game.map_visits WHERE character_id=$1", &[&character]).await.unwrap();
        db.execute("DELETE FROM game.characters WHERE character_id=$1", &[&character]).await.unwrap();
    }
    db.execute("DELETE FROM game.accounts WHERE user_id=$1", &[&account]).await.unwrap();
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn headless_leave_saves_owned_ship_before_unobserving_and_clears_presence() {
    use crate::{config::deployment::DatabaseConfig, consts::{TYPE_CAPSULE, dogma::{ATTR_ARMOR_DAMAGE, ATTR_HULL_DAMAGE, ATTR_SHIELD_CHARGE}}, dao::{characters, items}, db::Db, sde::Sde, services::{combat, identity::Identity, ship}, session::ChatSession, space::World};
    crate::tests::init_config();
    let db = Db::connect(&DatabaseConfig { schema_on_start: false, ..crate::config::tests::test_database() }).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let system_row = sde.map.systems().find(|s| s.constellation_id.is_some() && s.region_id.is_some()).unwrap();
    let (system, constellation, region) = (system_row.solar_system_id, system_row.constellation_id.unwrap(), system_row.region_id.unwrap());
    let account: i64 = db.query_one("INSERT INTO game.accounts (user_name,password_hash,role) VALUES (nextval('game.accounts_user_id_seq')::text,''::bytea,0) RETURNING user_id", &[]).await.unwrap().get("user_id");
    let character: i64 = db.query_one("INSERT INTO game.characters (user_id,name,race_id,bloodline_id,gender_id,corporation_id,station_id,solar_system_id,constellation_id,region_id) VALUES ($1,nextval('game.characters_character_id_seq')::text,1,1,1,1,NULL,$2,$3,$4) RETURNING character_id", &[&account, &system, &constellation, &region]).await.unwrap().get("character_id");
    let ship_id = items::creation::positioned(&db, TYPE_CAPSULE, character, system, [1.0, 2.0, 3.0]).await.unwrap();
    characters::selection::set_ship(&db, character, ship_id).await.unwrap();
    let world = World::new(db.clone(), sde.clone(), 1);
    let cfg = crate::config::Config::default();
    let chat = crate::chat::Hub::new(&cfg.chat.domain, db.clone(), world.clone());
    let test_db = db.clone();
    let test_world = world.clone();
    let result = tokio::spawn(async move {
        let ids = [ATTR_SHIELD_CHARGE, ATTR_ARMOR_DAMAGE, ATTR_HULL_DAMAGE];
        items::dogma::save_condition(&test_db, ship_id, &ids, &[0.0, 1.0, 0.0], ATTR_SHIELD_CHARGE, 0).await.unwrap();
        let row = items::space_ships::FreeShip { item_id: ship_id, type_id: TYPE_CAPSULE, owner_id: character, corporation_id: 1, alliance_id: 0, pos: [1.0e15, 2.0e15, 3.0e15] };
        let ball = ship::balls::piloted(&test_db, sde.clone(), &row, character, 0.0).await.ok().unwrap().install();
        assert!((combat::ship_condition(ship_id).unwrap().1 - 1.0).abs() < 1e-9);
        test_world.add_ship(system, ball).await.unwrap();
        items::dogma::save_condition(&test_db, ship_id, &ids, &[0.0, 0.0, 0.0], ATTR_SHIELD_CHARGE, 0).await.unwrap();
        let identity = Identity { char_id: character, ship_id: Some(ship_id), corp_id: 1, station_id: None, system_id: Some(system) };
        let (notify_tx, _notify_rx) = tokio::sync::mpsc::unbounded_channel();
        super::online(&identity, &test_world, &chat, notify_tx, None, ChatSession {
            character_id: character, system_id: Some(system), corporation_id: Some(1),
            alliance_id: None, faction_id: None, fleet_id: None, role: 0, corporation_role: 0,
        });
        let position = test_world.with(system, move |s| s.pilot_ball(character).unwrap().pos).await.unwrap();
        assert_ne!(position, [1.0, 2.0, 3.0]);
        let (boundary_tx, boundary_rx) = tokio::sync::oneshot::channel();
        let callback_world = test_world.clone();
        super::leave(&identity, &test_db, &test_world, &chat, &sde, Some(system), move || {
            assert!(callback_world.is_online(character));
            assert!(callback_world.chat_session(character).is_some());
            callback_world.post(system, move |s| {
                let _ = boundary_tx.send(s.pilot_ball(character).is_none() && s.ball(ship_id).is_none());
            });
        }).await.unwrap();
        assert!(boundary_rx.await.unwrap());
        assert!(!test_world.is_online(character));
        assert!(test_world.chat_session(character).is_none());
        let saved = test_db.query_one("SELECT x,y,z FROM game.items WHERE item_id=$1", &[&ship_id]).await.unwrap();
        assert_eq!([saved.get::<_, f64>("x"), saved.get("y"), saved.get("z")], position);
        let rows = items::dogma::overrides(&test_db, ship_id).await.unwrap();
        for id in ids { assert!(rows.iter().any(|row| row.attribute_id == id)); }
        assert!((rows.iter().find(|row| row.attribute_id == ATTR_ARMOR_DAMAGE).unwrap().value - 1.0).abs() < 1e-9);
    }).await;
    world.remove_ball(system, ship_id).await;
    combat::invalidate_condition(ship_id);
    db.execute("DELETE FROM game.item_attributes WHERE item_id=$1", &[&ship_id]).await.unwrap();
    db.execute("UPDATE game.characters SET ship_id=NULL WHERE character_id=$1", &[&character]).await.unwrap();
    db.execute("DELETE FROM game.items WHERE item_id=$1", &[&ship_id]).await.unwrap();
    db.execute("DELETE FROM game.characters WHERE character_id=$1", &[&character]).await.unwrap();
    db.execute("DELETE FROM game.accounts WHERE user_id=$1", &[&account]).await.unwrap();
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn absent_identity_leaves_zero_registry_untouched_and_calls_observer() {
    use crate::{config::deployment::DatabaseConfig, db::Db, sde::Sde, services::identity::Identity, session::ChatSession, space::World};
    crate::tests::init_config();
    let db = Db::connect(&DatabaseConfig { schema_on_start: false, ..crate::config::tests::test_database() }).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let world = World::new(db.clone(), sde.clone(), 1);
    let cfg = crate::config::Config::default();
    let chat = crate::chat::Hub::new(&cfg.chat.domain, db.clone(), world.clone());
    let (notify_tx, mut notify_rx) = tokio::sync::mpsc::unbounded_channel();
    world.register_pilot(0, notify_tx);
    world.set_chat_session(0, Some(ChatSession { character_id: 0, system_id: None, corporation_id: None, alliance_id: None, faction_id: None, fleet_id: None, role: 0, corporation_role: 0 }));
    let identity = Identity { char_id: 0, ship_id: None, corp_id: 0, station_id: None, system_id: None };
    let mut called = 0;
    super::leave(&identity, &db, &world, &chat, &sde, None, || called += 1).await.unwrap();
    assert_eq!(called, 1);
    assert!(world.is_online(0));
    assert!(world.chat_session(0).is_some());
    world.notify_pilot(0, "test", vec![]);
    assert_eq!(notify_rx.try_recv().unwrap().0, "test");
    world.unregister_pilot(0);
}
