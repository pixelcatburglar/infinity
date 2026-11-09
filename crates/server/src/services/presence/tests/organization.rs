//! Live organization changes through presence and the system mailbox.
use std::sync::Arc;
use infinity_core::{consts::TYPE_CAPSULE, session::OnlineSession};
use crate::{chat::Hub, dao::{characters, items}, db::Db, sde::Sde, services::{identity::Identity, ship}, space::World};
use marshal::Value;

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn affiliation_reaches_running_actor_without_loading_offline_system() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let system = sde.map.systems().find(|s| s.constellation_id.is_some() && s.region_id.is_some()).unwrap();
    let (system_id, constellation, region) = (system.solar_system_id, system.constellation_id.unwrap(), system.region_id.unwrap());
    let other_system = sde.map.systems().find(|s| s.solar_system_id != system_id && s.solar_system_id > 0).unwrap().solar_system_id;
    let (account, characters, ships, corp, alliance) = db.transaction(|tx, _: &mut ()| Box::pin(async move {
        let account: i64 = tx.query_one("INSERT INTO game.accounts (user_name,password_hash,role) VALUES (nextval('game.accounts_user_id_seq')::text,''::bytea,0) RETURNING user_id", &[]).await?.get("user_id");
        let mut characters = Vec::new();
        let mut ships = Vec::new();
        for _ in 0..2 {
            let id: i64 = tx.query_one("INSERT INTO game.characters (user_id,name,race_id,bloodline_id,gender_id,corporation_id,solar_system_id,constellation_id,region_id) VALUES ($1,nextval('game.characters_character_id_seq')::text,1,1,1,1,$2,$3,$4) RETURNING character_id", &[&account, &system_id, &constellation, &region]).await?.get("character_id");
            let hull = items::creation::positioned(tx, TYPE_CAPSULE, id, system_id, [1.0, 2.0, 3.0]).await?;
            characters::selection::set_ship(tx, id, hull).await?;
            characters.push(id);
            ships.push(hull);
        }
        let pilot = characters[0];
        let corp: i64 = tx.query_one("INSERT INTO game.corporations (name,ticker,ceo_id,creator_id,station_id,created) VALUES ('presence-org-' || nextval('game.corporations_corporation_id_seq')::text,'PO' || currval('game.corporations_corporation_id_seq')::text,$1,$1,1,1) RETURNING corporation_id", &[&pilot]).await?.get("corporation_id");
        let alliance: i64 = tx.query_one("INSERT INTO game.alliances (name,short_name,executor_corp_id,creator_corp_id,creator_char_id,created) VALUES ('presence-org-' || nextval('game.alliances_alliance_id_seq')::text,'PO' || currval('game.alliances_alliance_id_seq')::text,$1,$1,$2,1) RETURNING alliance_id", &[&corp, &pilot]).await?.get("alliance_id");
        tx.execute("UPDATE game.corporations SET alliance_id=$1 WHERE corporation_id=$2", &[&alliance, &corp]).await?;
        Ok::<_, crate::db::Error>((account, characters, ships, corp, alliance))
    }), |value, ()| async move { Ok(value) }).await.unwrap();
    let (pilot, observer, hull, other_hull) = (characters[0], characters[1], ships[0], ships[1]);
    let world = World::new(db.clone(), sde.clone(), 1);
    let hub = Hub::new("chat.test", db.clone(), world.clone());
    let test_db = db.clone();
    let test_world = world.clone();
    let result = tokio::spawn(async move {
        for (character, item) in [(pilot, hull), (observer, other_hull)] {
            let row = items::space_ships::FreeShip { item_id: item, type_id: TYPE_CAPSULE, owner_id: character, corporation_id: 1, alliance_id: -1, pos: [1.0, 2.0, 3.0] };
            let prepared = ship::balls::piloted(&test_db, sde.clone(), &row, character, 0.0).await.ok().unwrap();
            test_world.system(system_id).await.unwrap();
            test_world.with(system_id, move |s| prepared.install(s, |s, ball| s.add_ship(ball))).await.unwrap();
        }
        let (destiny, mut updates) = tokio::sync::mpsc::unbounded_channel();
        test_world.observe(system_id, observer, other_hull, destiny).await.unwrap();
        while updates.try_recv().is_ok() {}
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let identity = Identity { char_id: pilot, corp_id: 1, ship_id: Some(hull), station_id: None, system_id: Some(system_id) };
        let previous = OnlineSession { character_id: pilot, user_id: Some(account), session_id: 1, ship_id: Some(hull), station_id: None, system_id: Some(system_id), corporation_id: Some(1), alliance_id: None, faction_id: None, fleet_id: None, role: 0, corporation_role: 0 };
        super::super::online(&identity, &test_world, &hub, tx.clone(), None, previous.clone());
        test_db.execute("UPDATE game.characters SET corporation_id=$1 WHERE character_id=$2", &[&corp, &pilot]).await.unwrap();
        let selected = characters::selection::selected(&test_db, pilot, account).await.unwrap().unwrap();
        assert_eq!((selected.corporation_id, selected.alliance_id), (corp, Some(alliance)));
        let lower_tick = test_world.with(system_id, |s| s.current_time()).await.unwrap();
        let mut current = previous.clone();
        current.corporation_id = Some(selected.corporation_id);
        current.alliance_id = selected.alliance_id;
        super::super::online(&identity, &test_world, &hub, tx.clone(), Some(&previous), current.clone());
        let (updated, untouched, upper_tick) = test_world.with(system_id, move |s| (s.pilot_ball(pilot).unwrap().clone(), s.pilot_ball(observer).unwrap().clone(), s.current_time())).await.unwrap();
        assert_eq!((updated.id, updated.corp_id, updated.alliance_id), (hull, corp, alliance));
        assert_eq!((untouched.id, untouched.corp_id, untouched.alliance_id), (other_hull, 1, -1));
        let event = std::iter::from_fn(|| updates.try_recv().ok()).flatten().find(|v| v.items().and_then(|parts| parts.get(1)).and_then(Value::items).and_then(|call| call.first()).and_then(Value::as_text).is_some_and(|name| name == "OnSlimItemChange")).unwrap();
        let parts = event.items().unwrap();
        let tick = parts[0].as_int().unwrap();
        assert!((lower_tick..=upper_tick).contains(&tick));
        let args = parts[1].items().unwrap()[1].items().unwrap();
        assert_eq!(args[0], Value::Int(hull));
        let Value::Instance(_, state) = &args[1] else { panic!("expected slim item") };
        assert_eq!(state.get("corpID"), Some(&Value::Int(corp)));
        assert_eq!(state.get("allianceID"), Some(&Value::Int(alliance)));
        let mut replacement = current.clone();
        replacement.session_id += 1;
        super::super::online(&identity, &test_world, &hub, tx.clone(), None, replacement.clone());
        let mut stale = current.clone();
        stale.corporation_id = Some(1);
        stale.alliance_id = None;
        super::super::online(&identity, &test_world, &hub, tx.clone(), Some(&current), stale);
        let after_stale = test_world.with(system_id, move |s| (s.pilot_ball(pilot).unwrap().corp_id, s.pilot_ball(pilot).unwrap().alliance_id)).await.unwrap();
        assert_eq!(after_stale, (corp, alliance));
        assert_eq!(test_world.online_session(pilot).unwrap(), replacement);
        current = replacement;
        assert!(test_world.with(other_system, |_| ()).await.is_none());
        let mut offline_previous = current.clone();
        offline_previous.system_id = Some(other_system);
        super::super::online(&identity, &test_world, &hub, tx.clone(), Some(&current), offline_previous.clone());
        let mut offline_current = offline_previous.clone();
        offline_current.corporation_id = Some(1);
        offline_current.alliance_id = None;
        super::super::online(&identity, &test_world, &hub, tx, Some(&offline_previous), offline_current);
        assert!(test_world.with(other_system, |_| ()).await.is_none());
        let after = test_world.with(system_id, move |s| (s.pilot_ball(pilot).unwrap().corp_id, s.pilot_ball(pilot).unwrap().alliance_id)).await.unwrap();
        assert_eq!(after, (corp, alliance));
    }).await;
    world.unobserve(observer);
    for (id, hull) in characters.iter().zip(ships.iter()) {
        world.remove_ball(system_id, *hull).await;
        db.execute("DELETE FROM game.item_attributes WHERE item_id=$1", &[hull]).await.unwrap();
        db.execute("UPDATE game.characters SET ship_id=NULL,corporation_id=1 WHERE character_id=$1", &[id]).await.unwrap();
        db.execute("DELETE FROM game.items WHERE item_id=$1", &[hull]).await.unwrap();
    }
    db.execute("UPDATE game.corporations SET alliance_id=NULL WHERE corporation_id=$1", &[&corp]).await.unwrap();
    db.execute("DELETE FROM game.alliances WHERE alliance_id=$1", &[&alliance]).await.unwrap();
    db.execute("DELETE FROM game.corporations WHERE corporation_id=$1", &[&corp]).await.unwrap();
    for id in characters { db.execute("DELETE FROM game.characters WHERE character_id=$1", &[&id]).await.unwrap(); }
    db.execute("DELETE FROM game.accounts WHERE user_id=$1", &[&account]).await.unwrap();
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn observed_logoff_only_for_registered_generation() {
    use crate::services::tests::fixture;
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let pilot = fixture::create(&db).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let world = World::new(db.clone(), sde.clone(), 1);
    let hub = Hub::new("chat.test", db.clone(), world.clone());
    let identity = Identity { char_id: pilot.character, corp_id: 1, ship_id: None, station_id: None, system_id: None };
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let session = OnlineSession { character_id: pilot.character, user_id: Some(pilot.account), session_id: 7, ship_id: None, station_id: None, system_id: None, corporation_id: Some(1), alliance_id: None, faction_id: None, fleet_id: None, role: 0, corporation_role: 0 };
    super::super::online(&identity, &world, &hub, tx, None, session);
    let before = crate::db::outbox::tests::events(&db, "CharacterLoggedOff", pilot.character).await.len();
    super::super::leave(&identity, Some(pilot.account), &db, &world, &hub, &sde, None, Some(8), || {}).await.unwrap();
    assert_eq!(crate::db::outbox::tests::events(&db, "CharacterLoggedOff", pilot.character).await.len(), before);
    let observed: bool = db.query_one("SELECT observed_logoff_at IS NOT NULL AS observed FROM game.characters WHERE character_id=$1", &[&pilot.character]).await.unwrap().get("observed");
    assert!(!observed);
    super::super::leave(&identity, Some(pilot.account), &db, &world, &hub, &sde, None, Some(7), || {}).await.unwrap();
    let events = crate::db::outbox::tests::events(&db, "CharacterLoggedOff", pilot.character).await;
    assert_eq!(events.len(), before + 1);
    assert!(matches!(events.last(), Some(crate::lua::events::GameEvent::CharacterLoggedOff { user_id, char_id }) if (*user_id, *char_id) == (pilot.account, pilot.character)));
    let observed: Option<std::time::SystemTime> = db.query_one("SELECT observed_logoff_at FROM game.characters WHERE character_id=$1", &[&pilot.character]).await.unwrap().get("observed_logoff_at");
    assert!(observed.is_some());
    super::super::leave(&identity, Some(pilot.account), &db, &world, &hub, &sde, None, Some(7), || {}).await.unwrap();
    assert_eq!(crate::db::outbox::tests::events(&db, "CharacterLoggedOff", pilot.character).await.len(), before + 1);
    let after: Option<std::time::SystemTime> = db.query_one("SELECT observed_logoff_at FROM game.characters WHERE character_id=$1", &[&pilot.character]).await.unwrap().get("observed_logoff_at");
    assert_eq!(after, observed);
    fixture::cleanup(&db, pilot).await.unwrap();
}
