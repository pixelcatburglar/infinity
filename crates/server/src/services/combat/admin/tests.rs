use super::*;
use crate::dao::{characters::selection, items::creation};
use crate::services::{effects::tests::fixture as effects, slash::tests::support, tests::fixture};
use infinity_core::consts::{CATEGORY_SHIP, GM_ROLES, TYPE_CAPSULE};
use marshal::Value;

/// Real `/suicide` through the slash dispatch: a live ship ejects a capsule,
/// the capsule returns home, and the docked caller is refused.
#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn slash_suicide_destroys_ship_and_capsule_and_refuses_docked() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let fixture = fixture::create(&db).await.unwrap();
    let (mut c, _peer) = crate::services::tests::client(db.clone(), fixture.character).await;
    let result = crate::tests::catch_scenario(async {
    let character = fixture.character;
    (c.login.user_id, c.login.role) = (fixture.account, GM_ROLES);
    let system = c.sde.map.systems().next().unwrap().solar_system_id;
    let (home_station, home_system) = c.sde.map.stations().find(|s| s.solar_system_id.is_some() && s.constellation_id.is_some() && s.region_id.is_some()).map(|s| (s.station_id, s.solar_system_id)).unwrap();
    let hull = c.sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_SHIP) && t.type_id != TYPE_CAPSULE && t.group_id.is_some_and(|g| c.sde.repairable_ship.unrepairable(CATEGORY_SHIP, g, t.type_id))).expect("a published non-capsule unrepairable hull must exist in the SDE").type_id;
    let ship = creation::positioned(&db, hull, character, system, [1.0; 3]).await.unwrap();
    db.execute("UPDATE game.characters SET home_station_id=$2, station_id=NULL, solar_system_id=$3, ship_id=$4 WHERE character_id=$1",
        &[&character, &home_station, &system, &ship]).await.unwrap();
    c.session.apply(vec![("userid", Value::Int(fixture.account)), ("corpid", Value::Int(1)), ("shipid", Value::Int(ship)), ("solarsystemid", Value::Int(system)), ("solarsystemid2", Value::Int(system))]);
    let mut rx = support::register_online(&c.world, OnlineSession::from_session(&c.session).unwrap());
    effects::admit(&db, &c.world, system, effects::ball(ship, hull, character, Some(character), 0.0)).await;
    c.world.observe(system, c.login.client_id, ship, c.destiny_tx.clone()).await.unwrap();
    assert_eq!(support::wire(support::rpc(&mut c, "/suicide").await), support::wire(Ok(Value::unicode(messages::gm::gm_suicide_completed(ship)))));
    let notices: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).filter(|(n, _)| n == "__ship_destroyed").collect();
    let [(_, args)] = <[(String, Vec<Value>); 1]>::try_from(notices).unwrap();
    assert_eq!(args.first(), Some(&Value::Int(ship)));
    crate::services::ship_destroyed(&mut c, &args).await.unwrap();
    let capsule = selection::ship_id(&db, character).await.unwrap().unwrap();
    assert_ne!(capsule, ship);
    assert_eq!(db.query_one("SELECT type_id FROM game.items WHERE item_id=$1", &[&capsule]).await.unwrap().get::<_, i32>("type_id"), TYPE_CAPSULE);
    assert!(db.query("SELECT item_id FROM game.items WHERE item_id=$1", &[&ship]).await.unwrap().is_empty());
    assert_eq!((c.session.int("shipid"), c.world.online_session(character).unwrap().ship_id), (Some(capsule), Some(capsule)));
    assert_eq!(c.world.with(system, move |s| s.pilot_ball(character).map(|b| b.id)).await.flatten(), Some(capsule));
    assert_eq!(support::wire(support::rpc(&mut c, "/suicide").await), support::wire(Ok(Value::unicode(messages::gm::gm_suicide_completed(capsule)))));
    let notices: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).filter(|(n, _)| n == "__ship_destroyed").collect();
    let [(_, args)] = <[(String, Vec<Value>); 1]>::try_from(notices).unwrap();
    assert_eq!(args.first(), Some(&Value::Int(capsule)));
    crate::services::ship_destroyed(&mut c, &args).await.unwrap();
    let home = selection::ship_id(&db, character).await.unwrap().unwrap();
    assert_ne!(home, capsule);
    assert!(db.query("SELECT item_id FROM game.items WHERE item_id=$1", &[&capsule]).await.unwrap().is_empty());
    assert_eq!(db.query_one("SELECT location_id FROM game.items WHERE item_id=$1", &[&home]).await.unwrap().get::<_, i64>("location_id"), home_station);
    let row = db.query_one("SELECT station_id, solar_system_id FROM game.characters WHERE character_id=$1", &[&character]).await.unwrap();
    assert_eq!((row.get::<_, Option<i64>>("station_id"), row.get::<_, Option<i64>>("solar_system_id")), (Some(home_station), home_system));
    assert_eq!((c.session.int("stationid"), c.session.int("solarsystemid"), c.session.int("solarsystemid2")), (Some(home_station), None, home_system));
    let online = c.world.online_session(character).unwrap();
    assert_eq!((online.ship_id, online.station_id, online.system_id), (Some(home), Some(home_station), home_system));
    assert!(c.world.with(system, move |s| s.pilot_ball(character).is_none()).await.expect("actor stopped"));
    assert_eq!(support::wire(support::rpc(&mut c, "/suicide").await), support::wire(Err(notify_error(messages::gm::GM_SUICIDE_UNAVAILABLE))));
    assert_eq!(support::wire(support::rpc(&mut c, "/suicide me").await),
        support::wire(Err(crate::services::user_error(messages::MSG_SLASH_ERROR, vec![("reason".into(), Value::unicode(messages::gm::GM_SUICIDE_USAGE))]))));
    assert_eq!(selection::ship_id(&db, character).await.unwrap(), Some(home));
    assert!(!std::iter::from_fn(|| rx.try_recv().ok()).any(|(n, _)| n == "__ship_destroyed"));
    }).await;
    c.world.unobserve(c.login.client_id);
    c.world.unregister_pilot_generation(fixture.character, None);
    let system = c.sde.map.systems().next().unwrap().solar_system_id;
    if let Some(id) = c.world.with(system, move |s| s.pilot_ball(fixture.character).map(|b| b.id)).await.flatten() { c.world.remove_ball(system, id).await; }
    let kills: Vec<i64> = db.query("SELECT kill_id FROM game.killmails WHERE victim_id=$1", &[&fixture.character]).await.unwrap().iter().map(|r| r.get("kill_id")).collect();
    db.execute("DELETE FROM game.map_events WHERE source=$1 AND source_id=ANY($2)", &[&(crate::dao::map_telemetry::EventSource::Killmail as i16), &kills]).await.unwrap();
    let jobs: Vec<String> = kills.iter().map(|id| format!("{}:{id}", crate::services::bounty::payout::KIND)).collect();
    db.execute("DELETE FROM game.jobs WHERE job_id=ANY($1)", &[&jobs]).await.unwrap();
    db.execute("DELETE FROM game.bounty_payouts WHERE killmail_id=ANY($1)", &[&kills]).await.unwrap();
    db.execute("DELETE FROM game.notifications WHERE receiver_id=$1", &[&fixture.character]).await.unwrap();
    db.execute("DELETE FROM game.killmail_wars WHERE kill_id=ANY($1)", &[&kills]).await.unwrap();
    db.execute("DELETE FROM game.killmails WHERE kill_id=ANY($1)", &[&kills]).await.unwrap();
    db.execute("UPDATE game.characters SET ship_id=NULL WHERE character_id=$1", &[&fixture.character]).await.unwrap();
    fixture::cleanup(&db, fixture).await.unwrap();
    if let Err(panic) = result { std::panic::resume_unwind(panic); }
}
