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
    let d = db.clone();
    let result = tokio::spawn(async move {
        let (mut client, _peer) = crate::services::tests::client(d.clone(), fixture.character).await;
        let station = client.sde.map.stations().next().expect("test SDE has a station");
        let system = station.solar_system_id.expect("test SDE station has a solar system");
        client.session.apply(vec![("stationid", Value::Int(station.station_id)), ("solarsystemid", Value::Int(system)), ("shipid", Value::Int(0))]);
        let ship_type = client.sde.types.iter().find(|ty| ty.category_id == Some(CATEGORY_SHIP)).expect("test SDE has a ship type").type_id;
        let can_type = client.sde.types.iter().find(|ty| ty.group_id.is_some_and(|group| CARGO_CONTAINER_GROUPS.contains(&group))).expect("test SDE has a renameable container type").type_id;
        client.world.system(system).await.unwrap();
        let ship = crate::dao::items::creation::positioned(&d, ship_type, fixture.character, system, [1e9; 3]).await.unwrap();
        let free = crate::dao::items::space_ships::unpiloted(&d, system).await.unwrap().into_iter().find(|s| s.item_id == ship).unwrap();
        let prepared = crate::services::ship::balls::piloted(&d, client.sde.clone(), &free, fixture.character, 0.0).await.unwrap();
        client.world.with(system, move |s| prepared.install(s, |s, b| s.add_ship(b))).await.unwrap();
        let can = crate::dao::items::transfers::insert(&d, can_type, fixture.character, station.station_id, infinity_core::consts::FLAG_HANGAR, 1).await.unwrap();
        let name = "新名称ξ";
        for (scenario, item) in [("ship", ship), ("container", can)] {
            let kwargs = Value::Dict(vec![]);
            let ids = [Value::list([Value::Int(item)])];
            let before = crate::services::dispatch(&mut client, &crate::services::Call { service: "config", method: "GetMultiLocationsEx", args: &ids, kwargs: &kwargs, bound: false, bind_params: None }).await.unwrap();
            let before_wire = marshal::dumps(&before);
            client.outbox.clear();
            let args = [Value::Int(item), Value::unicode(name)];
            let call = crate::services::Call { service: "invbroker", method: "SetLabel", args: &args, kwargs: &kwargs, bound: true, bind_params: None };
            assert_eq!(crate::services::dispatch(&mut client, &call).await.unwrap(), Value::None);
            let persisted = crate::dao::characters::social::locations(&d, &[item]).await.unwrap().iter().find(|row| row.id == item).and_then(|row| row.name.clone());
            assert_eq!(persisted.as_deref(), Some(name));
            if scenario == "ship" {
                let names = client.world.with(system, move |s| s.ball(item).map(|b| b.slim.iter().filter(|(k, _)| *k == "name").cloned().collect::<Vec<_>>())).await.flatten().expect("installed ship ball");
                assert_eq!(names.as_slice(), [("name", Value::unicode(name))]);
            }
            let after = crate::services::dispatch(&mut client, &crate::services::Call { service: "config", method: "GetMultiLocationsEx", args: &ids, kwargs: &kwargs, bound: false, bind_params: None }).await.unwrap();
            let after_wire = marshal::dumps(&after);
            let located = after.items().unwrap()[1].items().unwrap().iter().find(|row| row.items().unwrap()[0].as_int() == Some(item)).expect("GetMultiLocationsEx returns the item row");
            assert_eq!(located.items().unwrap()[1], Value::unicode(name));
            let packet = client.outbox.iter().find(|p| matches!(&p.destination, infinity_wire::macho::Address::Broadcast { broadcast_id, .. } if broadcast_id.as_text().as_deref() == Some("OnCfgDataChanged"))).expect("caller receives the location-cache notification");
            let envelope = packet.body[0].items().unwrap()[1].as_bytes().unwrap().to_vec();
            let payload = marshal::loads(&envelope).unwrap();
            let notify = payload.items().unwrap()[1].items().unwrap();
            assert_eq!(notify[0].as_text().as_deref(), Some("evelocations"));
            let data = notify[1].items().expect("the notification carries one location record");
            assert_eq!(data[0].as_int(), Some(item));
            assert_eq!(data[1], Value::unicode(name));
            println!("LABEL_CACHE_CAPTURE {}", serde_json::json!({
                "scenario": scenario, "id": format!("set-label-{scenario}"), "kind": "notification", "event": "OnCfgDataChanged",
                "caller": fixture.character.to_string(), "item": item.to_string(), "name": name, "dao_name": persisted,
                "envelope": envelope, "get_multi_before": before_wire, "get_multi_after": after_wire,
            }));
        }
    }).await;
    crate::services::tests::fixture::cleanup(&db, fixture).await.unwrap();
    assert_eq!(crate::services::tests::fixture::remaining(&db, fixture).await.unwrap(), 0);
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated database with SDE"]
async fn label_owned_container_inside_foreign_private_hull_is_refused() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let actor = crate::services::tests::fixture::create(&db).await.unwrap();
    let owner = crate::services::tests::fixture::create(&db).await.unwrap();
    let (mut client, _peer) = crate::services::tests::client(db.clone(), actor.character).await;
    let station = client.sde.map.stations().next().unwrap().station_id;
    client.session.apply(vec![("stationid", Value::Int(station))]);
    let hull_type = client.sde.types.iter().find(|ty| ty.category_id == Some(CATEGORY_SHIP)).unwrap().type_id;
    let container_type = client.sde.types.iter().find(|ty| ty.group_id.is_some_and(|group| CARGO_CONTAINER_GROUPS.contains(&group))).unwrap().type_id;
    let hull = crate::dao::items::creation::rookie_ship(&db, hull_type, owner.character, station, infinity_core::consts::FLAG_HANGAR).await.unwrap();
    let container = crate::dao::items::transfers::insert(&db, container_type, actor.character, hull, infinity_core::consts::FLAG_CARGO, 1).await.unwrap();
    let name_before = crate::dao::characters::social::locations(&db, &[container]).await.unwrap().pop().unwrap().name;
    client.outbox.clear();
    let kwargs = Value::Dict(vec![]);
    let args = [Value::Int(container), Value::unicode("名称")];
    let call = crate::services::Call { service: "invbroker", method: "SetLabel", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
    assert!(crate::services::dispatch(&mut client, &call).await.is_err());
    assert!(client.outbox.is_empty());
    let name_after = crate::dao::characters::social::locations(&db, &[container]).await.unwrap().pop().unwrap().name;
    assert_eq!(name_after, name_before);
    let row = load_items(&db, &client.sde, ItemQuery::Id(container)).await.unwrap().pop().unwrap();
    assert_eq!(row.owner_id, actor.character);
    crate::dao::items::inventory::delete(&db, container).await.unwrap();
    crate::dao::items::inventory::delete(&db, hull).await.unwrap();
    crate::services::tests::fixture::cleanup(&db, actor).await.unwrap();
    crate::services::tests::fixture::cleanup(&db, owner).await.unwrap();
}
