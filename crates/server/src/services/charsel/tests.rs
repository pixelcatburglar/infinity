use super::*;
use crate::dao::items;
use infinity_core::consts::{FLAG_FITTING_FIRST, FLAG_HANGAR, FLAG_HI_SLOT_FIRST, FLAG_MED_SLOT_FIRST};
mod market_orders;

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn creation_dispatch_defers_ship_until_selection() {
    use crate::{config::deployment::DatabaseConfig, db::Db};
    crate::tests::init_config();
    let db = Db::connect(&DatabaseConfig { schema_on_start: false, ..crate::config::tests::test_database() }).await.unwrap();
    let account: i64 = db.query_one("INSERT INTO game.accounts (user_name, password_hash, role) VALUES (nextval('game.accounts_user_id_seq')::text, ''::bytea, 0) RETURNING user_id", &[]).await.unwrap().get("user_id");
    let (mut c, _peer) = crate::services::tests::client(db.clone(), 0).await;
    c.login.user_id = account;
    c.session.apply(vec![("userid", Value::Int(account)), ("charid", Value::None)]);
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let race = FALLBACK_RACE_ID;
        let school = c.sde.characters.schools_for_race(race as i32)
            .find(|s| s.corporation_id.is_some() && s.starting_stations.iter().any(|&id| c.sde.map.station(id).is_some())).unwrap();
        let school_id = school.school_id;
        let stations = school.starting_stations.clone();
        let name = format!("Pilot{account}");
        let appearance = Value::str("appearance");
        let portrait = Value::str("portrait");
        let kwargs = Value::Dict(vec![]);
        let args = vec![Value::unicode(name.clone()), Value::Int(race), Value::Int(FALLBACK_BLOODLINE_ID),
            Value::Int(1), Value::None, appearance.clone(), portrait.clone(), Value::Int(school_id)];
        let invalid = vec![Value::unicode("!")];
        let invalid_call = Call { service: "charUnboundMgr", method: "CreateCharacterWithDoll", args: &invalid, kwargs: &kwargs, bound: false, bind_params: None };
        assert!(matches!(crate::services::dispatch(&mut c, &invalid_call).await,
            Err(CallError::User { msg, .. }) if msg == messages::MSG_CHAR_NAME_INVALID));
        assert!(test_db.query("SELECT character_id FROM game.characters WHERE user_id=$1", &[&account]).await.unwrap().is_empty());
        let call = Call { service: "charUnboundMgr", method: "CreateCharacterWithDoll", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
        let char_id = crate::services::dispatch(&mut c, &call).await.unwrap().as_int().unwrap();
        let row = test_db.query_one("SELECT name, race_id, school_id, station_id, appearance, portrait, ship_id FROM game.characters WHERE character_id=$1 AND user_id=$2", &[&char_id, &account]).await.unwrap();
        assert_eq!(row.get::<_, String>("name"), name);
        assert_eq!(row.get::<_, i32>("race_id"), race as i32);
        assert_eq!(row.get::<_, Option<i32>>("school_id"), Some(school_id as i32));
        let station: i64 = row.get::<_, Option<i64>>("station_id").unwrap();
        assert!(stations.contains(&station));
        assert_eq!(row.get::<_, Vec<u8>>("appearance"), marshal::dumps(&appearance));
        assert_eq!(row.get::<_, Vec<u8>>("portrait"), marshal::dumps(&portrait));
        assert_eq!(row.get::<_, Option<i64>>("ship_id"), None);
        let mut expected: Vec<_> = c.sde.characters.race(race).unwrap().skills.iter().map(|s| (s.type_id, s.level)).collect();
        let mut actual: Vec<_> = test_db.query("SELECT type_id, level FROM game.skills WHERE character_id=$1", &[&char_id]).await.unwrap()
            .iter().map(|r| (r.get::<_, i32>("type_id"), r.get::<_, i32>("level"))).collect();
        expected.sort_unstable();
        actual.sort_unstable();
        assert_eq!(actual, expected);
        assert!(test_db.query("SELECT item_id FROM game.items WHERE owner_id=$1", &[&char_id]).await.unwrap().is_empty());
        let select_args = vec![Value::Int(char_id)];
        let select = Call { service: "charUnboundMgr", method: "SelectCharacterID", args: &select_args, kwargs: &kwargs, bound: false, bind_params: None };
        crate::services::dispatch(&mut c, &select).await.unwrap();
        let ship = c.session.int("shipid").unwrap();
        let ships = test_db.query("SELECT item_id FROM game.items WHERE owner_id=$1 AND location_id=$2 AND flag_id=$3", &[&char_id, &station, &FLAG_HANGAR]).await.unwrap();
        assert_eq!(ships.len(), 1);
        assert_eq!(ships[0].get::<_, i64>("item_id"), ship);
        crate::services::dispatch(&mut c, &select).await.unwrap();
        assert_eq!(c.session.int("shipid"), Some(ship));
        assert_eq!(test_db.query("SELECT item_id FROM game.items WHERE owner_id=$1 AND location_id=$2 AND flag_id=$3", &[&char_id, &station, &FLAG_HANGAR]).await.unwrap().len(), 1);
        let item_ids: Vec<i64> = test_db.query("SELECT item_id FROM game.items WHERE owner_id=$1 ORDER BY item_id", &[&char_id]).await.unwrap()
            .iter().map(|r| r.get("item_id")).collect();
        let skill_count = test_db.query("SELECT type_id FROM game.skills WHERE character_id=$1", &[&char_id]).await.unwrap().len();
        assert!(matches!(crate::services::dispatch(&mut c, &invalid_call).await,
            Err(CallError::User { msg, .. }) if msg == messages::MSG_CHAR_NAME_INVALID));
        let ids = test_db.query("SELECT character_id FROM game.characters WHERE user_id=$1", &[&account]).await.unwrap();
        assert_eq!(ids.len(), 1);
        assert_eq!(ids[0].get::<_, i64>("character_id"), char_id);
        let after_items: Vec<i64> = test_db.query("SELECT item_id FROM game.items WHERE owner_id=$1 ORDER BY item_id", &[&char_id]).await.unwrap()
            .iter().map(|r| r.get("item_id")).collect();
        assert_eq!(after_items, item_ids);
        assert_eq!(test_db.query("SELECT type_id FROM game.skills WHERE character_id=$1", &[&char_id]).await.unwrap().len(), skill_count);
    }).await;
    let chars = db.query("SELECT character_id FROM game.characters WHERE user_id=$1", &[&account]).await.unwrap();
    for row in chars {
        let character: i64 = row.get("character_id");
        let items = db.query("SELECT item_id FROM game.items WHERE owner_id=$1", &[&character]).await.unwrap();
        for item in &items {
            let id: i64 = item.get("item_id");
            db.execute("DELETE FROM game.item_attributes WHERE item_id=$1", &[&id]).await.unwrap();
        }
        db.execute("UPDATE game.characters SET ship_id=NULL WHERE character_id=$1", &[&character]).await.unwrap();
        for item in &items {
            let id: i64 = item.get("item_id");
            db.execute("DELETE FROM game.items WHERE location_id=$1 AND owner_id=$2", &[&id, &character]).await.unwrap();
        }
        db.execute("DELETE FROM game.items WHERE owner_id=$1", &[&character]).await.unwrap();
        db.execute("DELETE FROM game.skills WHERE character_id=$1", &[&character]).await.unwrap();
        db.execute("DELETE FROM game.characters WHERE character_id=$1", &[&character]).await.unwrap();
    }
    db.execute("DELETE FROM game.accounts WHERE user_id=$1", &[&account]).await.unwrap();
    result.unwrap();
}

/// SDE dgmAttributeTypes: lowSlots, medSlots and hiSlots.
const SLOT_ATTRIBUTES: [(i32, i32); 3] = [(FLAG_FITTING_FIRST, 12), (FLAG_MED_SLOT_FIRST, 13), (FLAG_HI_SLOT_FIRST, 14)];

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn rookie_grants_are_persistent_and_atomic() {
    use std::collections::HashSet;
    use crate::{config::deployment::DatabaseConfig, db::Db, sde::Sde, services::character::provisioning::ensure_ship};
    crate::tests::init_config();
    let db = Db::connect(&DatabaseConfig { schema_on_start: false, ..crate::config::tests::test_database() }).await.unwrap();
    let sde = Sde::load(&db).await.unwrap();
    let station = sde.map.stations().next().unwrap().station_id;
    let account: i64 = db.query_one("INSERT INTO game.accounts (user_name, password_hash, role) VALUES (nextval('game.accounts_user_id_seq')::text, ''::bytea, 0) RETURNING user_id", &[]).await.unwrap().get("user_id");
    let cases: &[(i32, i32, &[(i32, i32, i32)])] = &[
        (1, 601, &[(3638,27,1),(3651,28,1),(10039,19,1),(21857,20,1),(30839,11,1),(32465,87,1)]),
        (2, 588, &[(3636,27,1),(3651,28,1),(10039,19,1),(21857,20,1),(30839,11,1),(32465,87,1)]),
        (4, 596, &[(3634,27,1),(3651,28,1),(21857,19,1),(21853,11,1),(30839,12,1),(32465,87,1)]),
        (8, 606, &[(3640,27,1),(3651,28,1),(21857,19,1),(21853,11,1),(30839,12,1),(32465,87,1)]),
        (0, 606, &[(3640,27,1),(3651,28,1),(21857,19,1),(21853,11,1),(30839,12,1),(32465,87,1)]),
    ];
    for &(race, hull, expected) in cases {
        let owner: i64 = db.query_one("INSERT INTO game.characters (user_id, name, race_id, bloodline_id, gender_id, corporation_id, station_id, balance) VALUES ($1, nextval('game.characters_character_id_seq')::text, $2, 1, 1, 1, $3, 0) RETURNING character_id", &[&account, &race, &station]).await.unwrap().get("character_id");
        let (first, second) = tokio::join!(ensure_ship(&db, &sde, owner, race, station), ensure_ship(&db, &sde, owner, race, station));
        let ship = first.ok().unwrap();
        assert_eq!(second.ok().unwrap(), ship);
        assert_eq!(db.query_one("SELECT count(*) AS count FROM game.items WHERE owner_id = $1 AND location_id = $2 AND flag_id = $3", &[&owner, &station, &FLAG_HANGAR]).await.unwrap().get::<_, i64>("count"), 1);
        let row = db.query_one("SELECT type_id, owner_id, location_id, flag_id, quantity, singleton FROM game.items WHERE item_id = $1", &[&ship]).await.unwrap();
        assert_eq!((row.get::<_, i32>("type_id"), row.get::<_, i64>("owner_id"), row.get::<_, i64>("location_id"), row.get::<_, i32>("flag_id"), row.get::<_, i64>("quantity"), row.get::<_, bool>("singleton")), (hull, owner, station, FLAG_HANGAR, 1, true));
        let rows = db.query("SELECT type_id, owner_id, location_id, flag_id, quantity, singleton, online FROM game.items WHERE location_id = $1", &[&ship]).await.unwrap();
        assert_eq!(rows.len(), expected.len());
        let mut slots = HashSet::new();
        let hull_data = sde.types.get(hull).unwrap();
        for row in &rows {
            let ty: i32 = row.get("type_id");
            let flag: i32 = row.get("flag_id");
            let quantity: i64 = row.get("quantity");
            assert!(expected.iter().any(|&(type_id, slot, count)| (type_id, slot, i64::from(count)) == (ty, flag, quantity)));
            assert_eq!(row.get::<_, i64>("owner_id"), owner);
            assert_eq!(row.get::<_, i64>("location_id"), ship);
            let module = flag != infinity_core::consts::FLAG_DRONE_BAY;
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
        assert_eq!(ensure_ship(&db, &sde, owner, race, station).await.ok().unwrap(), ship);
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
    db.execute("DELETE FROM game.accounts WHERE user_id = $1", &[&account]).await.unwrap();
}

/// Test fixture clearance beyond all celestial and capsule collision radii, in metres.
const SPACE_SELECTION_FIXTURE_CLEARANCE: f64 = 1_000.0;

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
    let capsule_radius = f64::from(crate::services::ship::balls::radius(&sde, infinity_core::consts::TYPE_CAPSULE).unwrap());
    let max_surface_x = sde.map.celestials(system)
        .filter(|r| r.type_id.and_then(|id| i32::try_from(id).ok()).is_some_and(|id| sde.types.get(id).is_some()))
        .map(|r| {
            let surface = r.x.expect("NULL celestial x") + f64::from(sde.celestial_radius(r));
            assert!(surface.is_finite(), "celestial surface must be finite");
            surface
        })
        .max_by(f64::total_cmp)
        .expect("fixture system must contain a celestial");
    let pos = [max_surface_x + capsule_radius + SPACE_SELECTION_FIXTURE_CLEARANCE, 0.0, 0.0];
    assert!(pos[0].is_finite(), "fixture position must be finite");
    let account: i64 = db.query_one("INSERT INTO game.accounts (user_name,password_hash,role) VALUES (nextval('game.accounts_user_id_seq')::text,''::bytea,0) RETURNING user_id", &[]).await.unwrap().get("user_id");
    let character: i64 = db.query_one("INSERT INTO game.characters (user_id,name,race_id,bloodline_id,gender_id,corporation_id,station_id,solar_system_id,constellation_id,region_id) VALUES ($1,nextval('game.characters_character_id_seq')::text,1,1,1,1,NULL,$2,$3,$4) RETURNING character_id", &[&account, &system, &constellation, &region]).await.unwrap().get("character_id");
    let ship = items::creation::positioned(&db, infinity_core::consts::TYPE_CAPSULE, character, system, pos).await.unwrap();
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
    c.session.apply(vec![("userid", Value::Int(account))]);
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
                (ship, infinity_core::consts::TYPE_CAPSULE, Some(character), character, corp, alliance.unwrap_or(-1)));
            assert_eq!(ball.pos, pos);
            let fit = crate::services::fit::Fit::load(&c.db, c.sde.clone(), character, ship).await.unwrap();
            let fitted_speed = fit.value(crate::services::fit::Key::Ship, infinity_core::consts::dogma::ATTR_MAX_VELOCITY);
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
            let expert = c.outbox.iter().position(|p| matches!(&p.destination, Address::Broadcast { broadcast_id, .. } if broadcast_id.as_text().as_deref() == Some("OnExpertSystemsUpdated"))).unwrap();
            let timer = c.outbox.iter().position(|p| matches!(&p.destination, Address::Broadcast { broadcast_id, .. } if broadcast_id.as_text().as_deref() == Some("OnJumpCloakUpdated"))).unwrap();
            assert!(session < expert && expert < timer);
            let envelope = c.outbox[expert].body[0].items().unwrap();
            let payload = marshal::loads(envelope[1].as_bytes().unwrap()).unwrap();
            assert_eq!(payload, Value::tuple([Value::Int(1), Value::tuple([Value::Dict(vec![]), Value::Bool(false), Value::None])]));
            let envelope = c.outbox[timer].body[0].items().unwrap();
            let payload = marshal::loads(envelope[1].as_bytes().unwrap()).unwrap();
            assert_eq!(payload, Value::tuple([Value::Int(1), Value::tuple([Value::Int(ship), Value::Int(ball.cloak_until), Value::Int(60 * FILETIME_SECOND)])]));
            let before = crate::db::outbox::tests::events(&c.db, "CharacterLoggedOff", character).await.len();
            log_off(&mut c).await.unwrap();
            let logoffs = crate::db::outbox::tests::events(&c.db, "CharacterLoggedOff", character).await;
            assert_eq!(logoffs.len(), before + 1);
            assert!(matches!(logoffs.last(), Some(crate::lua::events::GameEvent::CharacterLoggedOff { user_id, char_id }) if (*user_id, *char_id) == (account, character)));
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            assert!(!w.is_online(character));
            assert!(w.online_session(character).is_none());
            assert!(!w.online_pilots().contains(&character));
            assert!(w.with(system, move |s| s.ball(ship).is_none()).await.unwrap());
        }
    }).await;
    assert_eq!(db.query_one("SELECT count(*) AS count FROM game.map_events", &[]).await.unwrap().get::<_, i64>("count"), events);
    assert!(db.query("SELECT 1 FROM game.map_visits WHERE character_id=$1", &[&character]).await.unwrap().is_empty());
    world.remove_ball(system, ship).await;
    db.execute("DELETE FROM game.progress_deliveries WHERE event_id IN (SELECT event_id FROM game.progress_events WHERE subject_character_id=$1)", &[&character]).await.unwrap();
    db.execute("DELETE FROM game.progress_events WHERE subject_character_id=$1", &[&character]).await.unwrap();
    db.execute("DELETE FROM game.skills WHERE character_id=$1", &[&character]).await.unwrap();
    db.execute("DELETE FROM game.item_attributes WHERE item_id=$1", &[&ship]).await.unwrap();
    db.execute("UPDATE game.characters SET ship_id=NULL WHERE character_id=$1", &[&character]).await.unwrap();
    db.execute("DELETE FROM game.items WHERE item_id=$1", &[&ship]).await.unwrap();
    db.execute("DELETE FROM game.characters WHERE character_id=$1", &[&character]).await.unwrap();
    db.execute("DELETE FROM game.accounts WHERE user_id=$1", &[&account]).await.unwrap();
    result.unwrap();
}
