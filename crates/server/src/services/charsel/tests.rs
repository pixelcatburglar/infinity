use super::*;
use crate::consts::{FLAG_FITTING_FIRST, FLAG_HI_SLOT_FIRST, FLAG_MED_SLOT_FIRST};

#[test]
fn name_shape_checks_configured_boundaries_and_spacing() {
    crate::tests::init_config();
    let policy = &crate::config::game().characters;
    assert_eq!(name_shape_code(&"a".repeat((policy.name_min_len - 1) as usize)), NAME_TOO_SHORT);
    assert_eq!(name_shape_code(&"a".repeat(policy.name_min_len as usize)), NAME_VALID);
    assert_eq!(name_shape_code(&"a".repeat((policy.name_max_len + 1) as usize)), NAME_TOO_LONG);
    assert_eq!(name_shape_code("A  B"), NAME_CONSECUTIVE_SPACES);
    assert_eq!(name_shape_code(&format!("A{}", " B".repeat((policy.name_max_spaces + 1) as usize))), NAME_TOO_MANY_SPACES);
}

/// SDE dgmAttributeTypes: lowSlots, medSlots and hiSlots.
const SLOT_ATTRIBUTES: [(i32, i32); 3] = [(FLAG_FITTING_FIRST, 12), (FLAG_MED_SLOT_FIRST, 13), (FLAG_HI_SLOT_FIRST, 14)];

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn rookie_grants_are_persistent_and_atomic() {
    use std::{collections::HashSet, sync::Arc};
    use crate::{config::{Config, deployment::DatabaseConfig}, conn::Conn, db::Db, handshake::Login, sde::Sde, space::World};
    crate::tests::init_config();
    let db = Db::connect(&DatabaseConfig { schema_on_start: false, ..crate::config::tests::test_database() }).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let station = sde.map.stations().next().unwrap().station_id;
    let account: i64 = db.query_one("INSERT INTO game.accounts (user_name, password_hash, role) VALUES (nextval('game.accounts_user_id_seq')::text, ''::bytea, 0) RETURNING user_id", &[]).await.unwrap().get("user_id");
    let world = World::new(db.clone(), sde.clone(), 1);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let _peer = tokio::net::TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
    let (stream, addr) = listener.accept().await.unwrap();
    let login = Login { user_id: account, user_name: account.to_string(), role: 0, client_id: account, session_id: account, language_id: "EN".into() };
    let c = Client::new(Conn::new(stream, addr), Arc::new(Config::default()), db.clone(), sde.clone(), crate::chat::Hub::new("localhost", db.clone(), world.clone()), world, login);
    let cases: &[(i32, i32, &[(i32, i32, i32)])] = &[
        (1, 601, &[(3638,27,1),(3651,28,1),(10039,19,1),(21857,20,1),(30839,11,1),(32465,87,1)]),
        (2, 588, &[(3636,27,1),(3651,28,1),(10039,19,1),(21857,20,1),(30839,11,1),(32465,87,1)]),
        (4, 596, &[(3634,27,1),(3651,28,1),(21857,19,1),(21853,11,1),(30839,12,1),(32465,87,1)]),
        (8, 606, &[(3640,27,1),(3651,28,1),(21857,19,1),(21853,11,1),(30839,12,1),(32465,87,1)]),
        (0, 606, &[(3640,27,1),(3651,28,1),(21857,19,1),(21853,11,1),(30839,12,1),(32465,87,1)]),
    ];
    for &(race, hull, expected) in cases {
        let owner: i64 = db.query_one("INSERT INTO game.characters (user_id, name, race_id, bloodline_id, gender_id, corporation_id, station_id, balance) VALUES ($1, nextval('game.characters_character_id_seq')::text, $2, 1, 1, 1, $3, 0) RETURNING character_id", &[&account, &race, &station]).await.unwrap().get("character_id");
        let (first, second) = tokio::join!(ensure_ship(&c, owner, race, station), ensure_ship(&c, owner, race, station));
        let ship = first.ok().unwrap();
        assert_eq!(second.ok().unwrap(), ship);
        assert_eq!(db.query_one("SELECT count(*) AS count FROM game.items WHERE owner_id = $1 AND location_id = $2 AND flag_id = $3", &[&owner, &station, &FLAG_HANGAR]).await.unwrap().get::<_, i64>("count"), 1);
        let row = db.query_one("SELECT type_id, owner_id, location_id, flag_id, quantity, singleton FROM game.items WHERE item_id = $1", &[&ship]).await.unwrap();
        assert_eq!((row.get::<_, i32>("type_id"), row.get::<_, i64>("owner_id"), row.get::<_, i64>("location_id"), row.get::<_, i32>("flag_id"), row.get::<_, i32>("quantity"), row.get::<_, bool>("singleton")), (hull, owner, station, FLAG_HANGAR, 1, true));
        let rows = db.query("SELECT type_id, owner_id, location_id, flag_id, quantity, singleton, online FROM game.items WHERE location_id = $1", &[&ship]).await.unwrap();
        assert_eq!(rows.len(), expected.len());
        let mut slots = HashSet::new();
        let hull_data = sde.types.get(hull).unwrap();
        for row in &rows {
            let ty: i32 = row.get("type_id");
            let flag: i32 = row.get("flag_id");
            let quantity: i32 = row.get("quantity");
            assert!(expected.contains(&(ty, flag, quantity)));
            assert_eq!(row.get::<_, i64>("owner_id"), owner);
            assert_eq!(row.get::<_, i64>("location_id"), ship);
            let module = flag != crate::consts::FLAG_DRONE_BAY;
            assert_eq!(row.get::<_, bool>("singleton"), module);
            assert_eq!(row.get::<_, bool>("online"), module);
            if module {
                assert!(slots.insert(flag));
                let &(start, attribute) = SLOT_ATTRIBUTES.iter().find(|&&(start, _)| (start..start + 8).contains(&flag)).unwrap();
                assert!((flag - start) < hull_data.attribute(attribute).unwrap() as i32);
            }
        }
        assert!(db.query("SELECT 1 FROM game.module_charges WHERE ship_id = $1", &[&ship]).await.unwrap().is_empty());
        db.execute("DELETE FROM game.items WHERE location_id = $1", &[&ship]).await.unwrap();
        assert_eq!(ensure_ship(&c, owner, race, station).await.ok().unwrap(), ship);
        assert!(db.query("SELECT 1 FROM game.items WHERE location_id = $1", &[&ship]).await.unwrap().is_empty());
        db.execute("UPDATE game.characters SET ship_id = NULL WHERE character_id = $1", &[&owner]).await.unwrap();
        db.execute("DELETE FROM game.items WHERE item_id = $1", &[&ship]).await.unwrap();
        let result: Result<(), crate::db::Error> = db.transaction(|tx, _: &mut Outbox| Box::pin(async move {
            let ship = items::creation::rookie_ship(tx, hull, owner, station, FLAG_HANGAR).await?;
            items::creation::fitted_items(tx, ship, owner, expected).await?;
            characters::selection::set_ship(tx, owner, ship).await?;
            tx.execute("SELECT * FROM game.rookie_fit_forced_failure", &[]).await?;
            Ok(())
        }), |value, _| async move { Ok(value) }).await;
        assert!(result.is_err());
        assert!(db.query("SELECT 1 FROM game.items WHERE owner_id = $1", &[&owner]).await.unwrap().is_empty());
        assert_eq!(db.query_one("SELECT ship_id FROM game.characters WHERE character_id = $1", &[&owner]).await.unwrap().get::<_, Option<i64>>("ship_id"), None);
        db.execute("DELETE FROM game.characters WHERE character_id = $1", &[&owner]).await.unwrap();
    }
    assert!(rookie_fit::for_ship(-1).is_none());
    db.execute("DELETE FROM game.accounts WHERE user_id = $1", &[&account]).await.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn space_selection_restores_cloak_before_session_and_without_map_telemetry() {
    use std::sync::Arc;
    use crate::{config::{Config, deployment::DatabaseConfig}, conn::Conn, db::Db, handshake::Login, macho::{self, Address}, sde::Sde, space::{World, CLOAK_RESTORE}};
    crate::tests::init_config();
    let db = Db::connect(&DatabaseConfig { schema_on_start: false, ..crate::config::tests::test_database() }).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let row = sde.map.systems().find(|s| s.constellation_id.is_some() && s.region_id.is_some()).unwrap();
    let (system, constellation, region) = (row.solar_system_id, row.constellation_id.unwrap(), row.region_id.unwrap());
    let account: i64 = db.query_one("INSERT INTO game.accounts (user_name,password_hash,role) VALUES (nextval('game.accounts_user_id_seq')::text,''::bytea,0) RETURNING user_id", &[]).await.unwrap().get("user_id");
    let character: i64 = db.query_one("INSERT INTO game.characters (user_id,name,race_id,bloodline_id,gender_id,corporation_id,station_id,solar_system_id,constellation_id,region_id) VALUES ($1,nextval('game.characters_character_id_seq')::text,1,1,1,1,NULL,$2,$3,$4) RETURNING character_id", &[&account, &system, &constellation, &region]).await.unwrap().get("character_id");
    let pos = [1234.0, 5678.0, 9012.0];
    let ship = items::creation::positioned(&db, crate::consts::TYPE_CAPSULE, character, system, pos).await.unwrap();
    characters::selection::set_ship(&db, character, ship).await.unwrap();
    let selected = characters::selection::selected(&db, character, account).await.unwrap().unwrap();
    let (corp, alliance) = (selected.corporation_id, selected.alliance_id);
    let events: i64 = db.query_one("SELECT count(*) AS count FROM game.map_events", &[]).await.unwrap().get("count");
    let world = World::new(db.clone(), sde.clone(), 1);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let _peer = tokio::net::TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
    let (stream, addr) = listener.accept().await.unwrap();
    let login = Login { user_id: account, user_name: account.to_string(), role: 0, client_id: account, session_id: account, language_id: "EN".into() };
    let cfg = Arc::new(Config::default());
    let chat = crate::chat::Hub::new(&cfg.chat.domain, db.clone(), world.clone());
    let mut c = Client::new(Conn::new(stream, addr), cfg, db.clone(), sde, chat, world.clone(), login);
    let w = world.clone();
    let result = tokio::spawn(async move {
        assert_eq!(c.session.int("charid"), None);
        let mut previous_expiry = 0;
        for _ in 0..2 {
            c.outbox.clear();
            let start = crate::client::filetime_now();
            select_character(&mut c, character).await.unwrap();
            let ball = w.with(system, move |s| s.pilot_ball(character).cloned()).await.unwrap().unwrap();
            assert_eq!((ball.id, ball.type_id, ball.char_id, ball.owner_id, ball.corp_id, ball.alliance_id),
                (ship, crate::consts::TYPE_CAPSULE, Some(character), character, corp, alliance.unwrap_or(-1)));
            assert_eq!(ball.pos, pos);
            let fit = crate::services::fit::Fit::load(&c.db, c.sde.clone(), character, ship).await.unwrap();
            let fitted_speed = fit.value(crate::services::fit::Key::Ship, crate::consts::dogma::ATTR_MAX_VELOCITY);
            if fitted_speed > 0.0 { assert_eq!(ball.max_vel, fitted_speed as f32); }
            assert_eq!(ball.cloak, CLOAK_RESTORE);
            assert!(ball.cloak_until >= start + 60 * FILETIME_SECOND);
            assert!(ball.cloak_until > previous_expiry);
            previous_expiry = ball.cloak_until;
            assert_eq!(c.session.int("charid"), Some(character));
            assert_eq!(c.session.int("corpid"), Some(corp));
            assert_eq!(c.session.int("allianceid"), alliance);
            assert_eq!(c.session.int("shipid"), Some(ship));
            assert_eq!(c.session.int("stationid"), None);
            assert_eq!(c.session.int("solarsystemid"), Some(system));
            assert_eq!(c.session.int("solarsystemid2"), Some(system));
            assert_eq!(c.session.int("constellationid"), Some(constellation));
            assert_eq!(c.session.int("regionid"), Some(region));
            let session = c.outbox.iter().position(|p| p.command == macho::cmd::SESSIONCHANGENOTIFICATION).unwrap();
            let timer = c.outbox.iter().position(|p| matches!(&p.destination, Address::Broadcast { broadcast_id, .. } if broadcast_id.as_text().as_deref() == Some("OnJumpCloakUpdated"))).unwrap();
            assert!(session < timer);
            let envelope = c.outbox[timer].body[0].items().unwrap();
            let payload = marshal::loads(envelope[1].as_bytes().unwrap()).unwrap();
            assert_eq!(payload, Value::tuple([Value::Int(1), Value::tuple([Value::Int(ship), Value::Int(ball.cloak_until), Value::Int(60 * FILETIME_SECOND)])]));
            log_off(&mut c).await.unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
    }).await;
    assert_eq!(db.query_one("SELECT count(*) AS count FROM game.map_events", &[]).await.unwrap().get::<_, i64>("count"), events);
    assert!(db.query("SELECT 1 FROM game.map_visits WHERE character_id=$1", &[&character]).await.unwrap().is_empty());
    world.remove_ball(system, ship).await;
    db.execute("DELETE FROM game.skills WHERE character_id=$1", &[&character]).await.unwrap();
    db.execute("DELETE FROM game.item_attributes WHERE item_id=$1", &[&ship]).await.unwrap();
    db.execute("UPDATE game.characters SET ship_id=NULL WHERE character_id=$1", &[&character]).await.unwrap();
    db.execute("DELETE FROM game.items WHERE item_id=$1", &[&ship]).await.unwrap();
    db.execute("DELETE FROM game.characters WHERE character_id=$1", &[&character]).await.unwrap();
    db.execute("DELETE FROM game.accounts WHERE user_id=$1", &[&account]).await.unwrap();
    result.unwrap();
}
