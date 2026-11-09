use super::*;
use infinity_core::{consts::CATEGORY_SHIP, session::OnlineSession};
use crate::{dao::{characters::selection, items::{creation, space_ships::FreeShip}}, services::combat::damage::{apply_damage_guarded, DamageSource}};
use crate::tests::catch_scenario;

async fn cleanup(db: &Db, fixture: crate::services::tests::fixture::Fixture, victims: &[i64]) {
    let kills: Vec<i64> = db.query("SELECT kill_id FROM game.killmails WHERE victim_id=$1 AND victim_item_id=ANY($2)", &[&fixture.character, &victims]).await.unwrap().iter().map(|row| row.get("kill_id")).collect();
    db.execute("DELETE FROM game.map_events WHERE source=$1 AND source_id=ANY($2)", &[&(crate::dao::map_telemetry::EventSource::Killmail as i16), &kills]).await.unwrap();
    for kill in &kills {
        let job = format!("{}:{kill}", crate::services::bounty::payout::KIND);
        db.execute("DELETE FROM game.jobs WHERE job_id=$1", &[&job]).await.unwrap();
    }
    db.execute("DELETE FROM game.bounty_payouts WHERE killmail_id=ANY($1)", &[&kills]).await.unwrap();
    db.execute("DELETE FROM game.notifications WHERE receiver_id=$1", &[&fixture.character]).await.unwrap();
    db.execute("DELETE FROM game.killmail_wars WHERE kill_id=ANY($1)", &[&kills]).await.unwrap();
    db.execute("DELETE FROM game.killmails WHERE kill_id=ANY($1)", &[&kills]).await.unwrap();
    db.execute("UPDATE game.characters SET ship_id=NULL WHERE character_id=$1", &[&fixture.character]).await.unwrap();
    crate::services::tests::fixture::cleanup(db, fixture).await.unwrap();
}

fn resume_scenario(result: std::thread::Result<()>) {
    if let Err(panic) = result { std::panic::resume_unwind(panic); }
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn headless_loss_publishes_capsule_and_committed_session() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let fixture = crate::services::tests::fixture::create(&db).await.unwrap();
    let character = fixture.character;
    let mut victims = Vec::new();
    let result = catch_scenario(async {
    let system = sde.map.systems().next().unwrap().solar_system_id;
    let hull = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SHIP) && t.type_id != TYPE_CAPSULE).unwrap().type_id;
    let ship = creation::positioned(&db, hull, character, system, [1.0, 2.0, 3.0]).await.unwrap();
    victims.push(ship);
    db.execute("UPDATE game.characters SET station_id=NULL, solar_system_id=$2, ship_id=$3 WHERE character_id=$1", &[&character, &system, &ship]).await.unwrap();
    let world = World::new(db.clone(), sde.clone(), 0);
    world.system(system).await.unwrap();
    let row = FreeShip { item_id: ship, type_id: hull, owner_id: character, corporation_id: 1, alliance_id: -1, pos: [1.0, 2.0, 3.0] };
    let prepared = crate::services::ship::balls::piloted(&db, sde.clone(), &row, character, 0.0).await.ok().unwrap();
    world.with(system, move |s| prepared.install(s, |s, ball| s.add_ship(ball))).await.unwrap();
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    world.register_pilot(character, tx);
    world.set_online_session(character, Some(OnlineSession { character_id: character, user_id: None, session_id: 1, ship_id: Some(ship), station_id: None, system_id: Some(system), corporation_id: Some(1), alliance_id: None, faction_id: None, fleet_id: None, role: 0, corporation_role: 0 }));
    for invalid in [f64::NAN, f64::INFINITY] {
        let row = FreeShip { item_id: ship, type_id: hull, owner_id: character, corporation_id: 1, alliance_id: -1, pos: [1.0, 2.0, 3.0] };
        let mut prepared = crate::services::ship::balls::piloted(&db, sde.clone(), &row, character, 0.0).await.ok().unwrap();
        prepared.ball.pos = [invalid, 0.0, 0.0];
        prepared.ball.old_pos = prepared.ball.pos;
        world.with(system, move |s| prepared.install(s, |s, ball| s.add_ship(ball))).await.unwrap();
        assert!(apply_damage_guarded(&db, &world, system, ship, [0.0; 4], DamageSource { force_origin: Some(crate::lua::events::resources::FactOrigin::Gm), ..Default::default() }, true, |_| true).await.is_err());
        assert_eq!(selection::ship_id(&db, character).await.unwrap(), Some(ship));
    }
    let row = FreeShip { item_id: ship, type_id: hull, owner_id: character, corporation_id: 1, alliance_id: -1, pos: [1.0, 2.0, 3.0] };
    let prepared = crate::services::ship::balls::piloted(&db, sde.clone(), &row, character, 0.0).await.ok().unwrap();
    world.with(system, move |s| prepared.install(s, |s, ball| s.add_ship(ball))).await.unwrap();
    let result = apply_damage_guarded(&db, &world, system, ship, [0.0; 4], DamageSource { force_origin: Some(crate::lua::events::resources::FactOrigin::Gm), ..Default::default() }, true, |_| true).await.ok().unwrap().unwrap();
    assert!(result.lethal);
    let capsule = selection::ship_id(&db, character).await.unwrap().unwrap();
    assert_ne!(capsule, ship);
    assert_eq!(world.with(system, move |s| s.pilot_ball(character).map(|b| b.id)).await.flatten(), Some(capsule));
    let session = world.online_session(character).unwrap();
    assert_eq!((session.ship_id, session.station_id, session.system_id), (Some(capsule), None, Some(system)));
    assert_eq!(db.query("SELECT item_id FROM game.items WHERE owner_id=$1 AND type_id=$2", &[&character, &TYPE_CAPSULE]).await.unwrap().len(), 1);
    world.remove_ball(system, capsule).await;
    world.unregister_pilot_generation(character, None);
    let headless_ship = creation::positioned(&db, hull, character, system, [1.0, 2.0, 3.0]).await.unwrap();
    victims.push(headless_ship);
    selection::set_ship(&db, character, headless_ship).await.unwrap();
    let row = FreeShip { item_id: headless_ship, type_id: hull, owner_id: character, corporation_id: 1, alliance_id: -1, pos: [1.0, 2.0, 3.0] };
    let prepared = crate::services::ship::balls::piloted(&db, sde.clone(), &row, character, 0.0).await.ok().unwrap();
    world.with(system, move |s| prepared.install(s, |s, ball| s.add_ship(ball))).await.unwrap();
    assert!(apply_damage_guarded(&db, &world, system, headless_ship, [0.0; 4], DamageSource { force_origin: Some(crate::lua::events::resources::FactOrigin::Gm), ..Default::default() }, true, |_| true).await.ok().unwrap().unwrap().lethal);
    let headless_capsule = selection::ship_id(&db, character).await.unwrap().unwrap();
    assert_ne!(headless_capsule, headless_ship);
    assert_eq!(world.with(system, move |s| s.pilot_ball(character).map(|b| b.id)).await.flatten(), Some(headless_capsule));
    world.remove_ball(system, headless_capsule).await;
    }).await;
    cleanup(&db, fixture, &victims).await;
    resume_scenario(result);
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn stale_reservation_refreshes_session_without_replacing_actor() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let fixture = crate::services::tests::fixture::create(&db).await.unwrap();
    let character = fixture.character;
    let mut victims = Vec::new();
    let result = catch_scenario(async {
    let system = sde.map.systems().next().unwrap().solar_system_id;
    let ship = creation::positioned(&db, TYPE_CAPSULE, character, system, [0.0; 3]).await.unwrap();
    victims.push(ship);
    let world = World::new(db.clone(), sde.clone(), 0);
    world.system(system).await.unwrap();
    let capsule = creation::positioned(&db, TYPE_CAPSULE, character, system, [1.0; 3]).await.unwrap();
    victims.push(capsule);
    let row = FreeShip { item_id: ship, type_id: TYPE_CAPSULE, owner_id: character, corporation_id: 1, alliance_id: -1, pos: [0.0; 3] };
    let prepared = crate::services::ship::balls::piloted(&db, sde.clone(), &row, character, 0.0).await.ok().unwrap();
    let (token, generation) = world.with(system, move |s| { prepared.install(s, |s, ball| s.add_ship(ball)); let snapshot = s.apply_damage(ship, [0.0; 4], DamageSource { force_origin: Some(crate::lua::events::resources::FactOrigin::Gm), ..Default::default() }, true).unwrap().unwrap().first_lethal.unwrap(); (snapshot.token, snapshot.generation) }).await.unwrap();
    let replacement_ball = crate::services::ship::balls::piloted(&db, sde.clone(), &row, character, 0.0).await.ok().unwrap();
    world.with(system, move |s| replacement_ball.install(s, |s, ball| s.add_ship(ball))).await.unwrap();
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    world.register_pilot(character, tx);
    world.set_online_session(character, Some(OnlineSession { character_id: character, user_id: None, session_id: 1, ship_id: Some(ship), station_id: None, system_id: Some(system), corporation_id: Some(1), alliance_id: None, faction_id: None, fleet_id: None, role: 0, corporation_role: 0 }));
    let expected = world.online_session(character);
    selection::set_ship(&db, character, capsule).await.unwrap();
    let mut replacement = expected.clone().unwrap();
    replacement.session_id += 1;
    let (replacement_tx, _replacement_rx) = tokio::sync::mpsc::unbounded_channel();
    world.register_pilot(character, replacement_tx);
    world.set_online_session(character, Some(replacement.clone()));
    let identity = Identity { char_id: character, corp_id: 1, ship_id: Some(ship), station_id: None, system_id: Some(system) };
    assert!(!publish_loss(&identity, &db, &world, &sde, LossOutcome { capsule, home: None, position: [1.0; 3] }, token, generation, None).await.ok().unwrap());
    assert_eq!(world.online_session(character), Some(replacement.clone()));
    assert!(!publish_loss(&identity, &db, &world, &sde, LossOutcome { capsule, home: None, position: [1.0; 3] }, token, generation, expected.clone()).await.ok().unwrap());
    assert_eq!(world.online_session(character), Some(replacement));
    assert_eq!(selection::ship_id(&db, character).await.unwrap(), Some(capsule));
    assert_eq!(world.with(system, move |s| s.pilot_ball(character).map(|b| b.id)).await.flatten(), Some(ship));
    assert!(world.ball(system, capsule).await.is_none());
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    world.register_pilot(character, tx);
    world.set_online_session(character, expected.clone());
    assert!(!publish_loss(&identity, &db, &world, &sde, LossOutcome { capsule, home: None, position: [1.0; 3] }, token, generation, expected).await.ok().unwrap());
    assert_eq!(world.online_session(character).unwrap().ship_id, Some(capsule));
    assert_eq!(world.with(system, move |s| s.pilot_ball(character).map(|b| b.id)).await.flatten(), Some(ship));
    assert!(world.ball(system, capsule).await.is_none());
    let prepared = crate::services::ship::balls::selected(&identity, &db, &world, &sde, Some(system), None, capsule, [1.0; 3]).await.ok().unwrap();
    world.with(system, move |s| { s.remove_ball(ship); prepared.install(s, |s, ball| s.add_ship(ball)); }).await.unwrap();
    assert_eq!(world.with(system, move |s| s.pilot_ball(character).map(|b| b.id)).await.flatten(), Some(capsule));
    let station = sde.map.stations().find(|s| s.solar_system_id.is_some() && s.constellation_id.is_some() && s.region_id.is_some()).unwrap();
    let home = (station.station_id, station.solar_system_id.unwrap(), station.constellation_id.unwrap(), station.region_id.unwrap());
    let docked = creation::rookie_ship(&db, TYPE_CAPSULE, character, home.0, FLAG_HANGAR).await.unwrap();
    let expected = world.online_session(character);
    crate::dao::characters::ship::set_home(&db, character, docked, home.0, home.1, home.2, home.3).await.unwrap();
    let (token, generation) = world.with(system, move |s| { let snapshot = s.apply_damage(capsule, [0.0; 4], DamageSource { force_origin: Some(crate::lua::events::resources::FactOrigin::Gm), ..Default::default() }, true).unwrap().unwrap().first_lethal.unwrap(); (snapshot.token, snapshot.generation) }).await.unwrap();
    let capsule_identity = Identity { ship_id: Some(capsule), ..identity };
    assert!(publish_loss(&capsule_identity, &db, &world, &sde, LossOutcome { capsule: docked, home: Some(home), position: [1.0; 3] }, token, generation, expected).await.ok().unwrap());
    let session = world.online_session(character).unwrap();
    assert_eq!((session.ship_id, session.station_id, session.system_id), (Some(docked), Some(home.0), Some(home.1)));
    assert!(world.with(system, move |s| s.pilot_ball(character).is_none() && !s.contains(docked)).await.unwrap());
    let mut replacement = session.clone();
    replacement.session_id += 1;
    let (replacement_tx, _replacement_rx) = tokio::sync::mpsc::unbounded_channel();
    world.register_pilot(character, replacement_tx);
    world.set_online_session(character, Some(replacement.clone()));
    let mut stale = session.clone();
    stale.ship_id = Some(ship);
    assert!(!world.replace_online_session(character, Some(&session), Some(stale)));
    assert_eq!(world.online_session(character), Some(replacement));
    world.unregister_pilot_generation(character, None);
    }).await;
    cleanup(&db, fixture, &victims).await;
    resume_scenario(result);
}
