use super::*;

#[test]
fn validates_unicode_character_limit_and_controls() {
    assert!(valid_name("舰船", SHIP_NAME_LIMIT));
    assert!(!valid_name("a\n", CONTAINER_NAME_LIMIT));
    assert!(!valid_name(&"x".repeat(CONTAINER_NAME_LIMIT + 1), CONTAINER_NAME_LIMIT));
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated database"]
async fn set_label_uses_guarded_row_and_updates_location() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let fixture = crate::services::tests::fixture::create(&db).await.unwrap();
    let (mut client, _peer) = crate::services::tests::client(db.clone(), fixture.character).await;
    let station = client.sde.map.stations().next().expect("test SDE has a station");
    let system = station.solar_system_id.expect("test SDE station has a solar system");
    client.session.apply(vec![("stationid", marshal::Value::Int(station.station_id)), ("solarsystemid", marshal::Value::Int(system)), ("shipid", marshal::Value::Int(0))]);
    let ty = client.sde.types.iter().find(|ty| crate::consts::CARGO_CONTAINER_GROUPS.contains(&(ty.group_id.unwrap_or_default() as i32))).expect("test SDE has a renameable container type");
    let item = crate::dao::items::transfers::insert(&db, ty.type_id, fixture.character, station.station_id, crate::consts::FLAG_HANGAR, 1).await.unwrap();
    super::set_label(&mut client, item, "新名称".into()).await.unwrap();
    assert_eq!(db.query_one("SELECT name FROM game.items WHERE item_id=$1", &[&item]).await.unwrap().get::<_, String>("name"), "新名称");
    crate::services::tests::fixture::cleanup(&db, fixture).await.unwrap();
}
