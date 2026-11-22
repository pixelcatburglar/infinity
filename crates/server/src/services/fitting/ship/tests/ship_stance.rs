//! Assembly, GM creation and GM spawn each initialize the hull's default
//! hidden mode item and never-switched timing through the same core.
use super::super::{assemble_ships, gm_create_ship, gm_spawn_ship};
use infinity_core::consts::{FLAG_HANGAR, FLAG_HIDDEN_MODIFIERS, TYPE_CAPSULE};
use marshal::Value;

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn assembly_and_gm_paths_initialize_default_stance() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = crate::tests::shared_sde(&db).await;
    let fixture = crate::services::tests::fixture::create(&db).await.unwrap();
    let (mut client, _peer) = crate::services::tests::client_with_sde(db.clone(), fixture.character, sde.clone()).await;
    let station = sde.map.stations().next().unwrap().station_id;
    client.session.apply(vec![("stationid", Value::Int(station))]);
    db.execute("UPDATE game.characters SET station_id=$2 WHERE character_id=$1", &[&fixture.character, &station]).await.unwrap();
    let mut ships = vec![assemble_ships(&mut client, vec![crate::dao::items::transfers::insert(&db,
        crate::sde::ship_stances::HULL_STANCES[0].hull_type, fixture.character, station, FLAG_HANGAR, 1).await.unwrap()], None).await.unwrap()[0],
        gm_create_ship(&mut client, fixture.character, crate::sde::ship_stances::HULL_STANCES[0].hull_type).await.unwrap()];
    // GM spawn needs a live system holding the caller's own pilot ball.
    let system = sde.map.station(station).unwrap().solar_system_id.unwrap();
    let active = crate::dao::items::creation::positioned(&db, TYPE_CAPSULE, fixture.character, system, [0.0; 3]).await.unwrap();
    client.world.system(system).await.unwrap();
    let pilot = crate::services::ship::balls::piloted(&db, sde.clone(),
        &crate::dao::items::space_ships::FreeShip { item_id: active, type_id: TYPE_CAPSULE, owner_id: fixture.character, corporation_id: 1, alliance_id: 0, pos: [0.0; 3] },
        fixture.character, 0.0).await.unwrap();
    client.world.with(system, move |s| pilot.install(s, |s, ball| s.add_ship(ball))).await.unwrap();
    client.session.apply(vec![("stationid", Value::None), ("solarsystemid", Value::Int(system)), ("shipid", Value::Int(active))]);
    ships.push(gm_spawn_ship(&mut client, crate::sde::ship_stances::HULL_STANCES[0].hull_type).await.unwrap());
    assert_eq!(db.query_one("SELECT count(*) AS n FROM game.items WHERE location_id=ANY($1) AND flag_id=$2 AND type_id=$3",
        &[&ships, &FLAG_HIDDEN_MODIFIERS, &crate::sde::ship_stances::HULL_STANCES[0].modes[0].1]).await.unwrap().get::<_, i64>("n"), 3);
    assert_eq!(db.query_one("SELECT count(*) AS n FROM game.ship_stances WHERE ship_id=ANY($1) AND previous_stance_id IS NULL AND switched_at=0 AND ready_at=0",
        &[&ships]).await.unwrap().get::<_, i64>("n"), 3);
    crate::services::tests::fixture::cleanup(&db, fixture).await.unwrap();
}
