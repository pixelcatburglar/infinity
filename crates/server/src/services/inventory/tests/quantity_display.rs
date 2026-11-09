//! Bound-inventory RPC over an over-capacity stack: stored quantities stay exact,
//! rows and `OnItemChange` values the client sees are capped at the display limit.

use super::*;

/// Decode a fresh bound moniker into the `invInventory` bind params the client
/// registered for it.
async fn bound_params(client: &mut Client, method: &str, args: &[Value]) -> Value {
    let kwargs = Value::Dict(vec![]);
    let bound = crate::services::dispatch(client, &Call { service: "invbroker", method, args, kwargs: &kwargs, bound: false, bind_params: None }).await.unwrap();
    let Value::Callback(callback) = bound else { panic!("expected bound moniker") };
    let oid = marshal::loads(callback.as_bytes().unwrap()).unwrap().items().unwrap()[0].as_text().unwrap().into_owned();
    client.bound.get(&oid).expect("registered bind").1.clone()
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated database with SDE"]
async fn inventory_rpc_projects_display_cap_without_changing_stored_quantities() {
    const CAP: i64 = infinity_core::consts::CLIENT_MAX_SHORT_AMOUNT;
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let actor = crate::services::tests::fixture::create(&db).await.unwrap();
    let result = crate::tests::catch_scenario(async {
        let (mut client, _peer) = crate::services::tests::client(db.clone(), actor.character).await;
        let station = client.sde.map.stations().find(|s| s.solar_system_id.is_some()).expect("SDE station");
        let (station_id, system_id) = (station.station_id, station.solar_system_id.unwrap());
        let ship_type = client.sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_SHIP)).expect("published SDE ship").type_id;
        let cargo_type = client.sde.types.iter().find(|t| t.published == Some(true)
            && t.category_id != Some(CATEGORY_SHIP) && t.volume.is_some_and(|v| v.is_finite() && v > 0.0)
            && client.sde.is_tradable(t.type_id)).expect("published cargo type").type_id;
        crate::dao::characters::ship::dock(&db, actor.character, station_id, system_id).await.unwrap();
        let ship = crate::dao::items::creation::rookie_ship(&db, ship_type, actor.character, station_id, FLAG_HANGAR).await.unwrap();
        crate::dao::characters::selection::set_ship(&db, actor.character, ship).await.unwrap();
        let stack = crate::dao::items::transfers::insert(&db, cargo_type, actor.character, ship, FLAG_CARGO, i64::MAX).await.unwrap();
        client.session.apply(vec![("stationid", Value::Int(station_id)), ("solarsystemid", Value::Int(system_id)), ("solarsystemid2", Value::Int(system_id)), ("shipid", Value::Int(ship))]);
        let (sender, mut notices) = tokio::sync::mpsc::unbounded_channel();
        client.notify_tx = sender;
        let kwargs = Value::Dict(vec![]);
        let ship_params = bound_params(&mut client, "GetInventoryFromId", &[Value::Int(ship)]).await;
        let list_args = [Value::Int(i64::from(FLAG_CARGO))];
        let list = crate::services::dispatch(&mut client, &Call { service: INVENTORY, method: "List", args: &list_args, kwargs: &kwargs, bound: true, bind_params: Some(&ship_params) }).await.unwrap();
        let rows = marshal::loads(&marshal::dumps(&list)).unwrap().items().unwrap().to_vec();
        assert_eq!(rows.len(), 1);
        let descriptor = crate::services::item_descriptor();
        assert_eq!(descriptor.get(&rows[0], "itemID"), Some(Value::Int(stack)));
        assert_eq!(descriptor.get(&rows[0], "quantity"), Some(Value::Int(CAP)));
        assert_eq!(load_items(&db, &client.sde, ItemQuery::Id(stack)).await.unwrap().pop().unwrap().quantity, i64::MAX);
        let hangar_params = bound_params(&mut client, "GetInventory", &[Value::Int(CONTAINER_HANGAR)]).await;
        let add_args = [Value::Int(stack), Value::Int(ship)];
        let add_kwargs = Value::dict([("flag", Value::Int(i64::from(FLAG_HANGAR))), ("qty", Value::Int(CAP))]);
        let moved = crate::services::dispatch(&mut client, &Call { service: INVENTORY, method: "Add", args: &add_args, kwargs: &add_kwargs, bound: true, bind_params: Some(&hangar_params) }).await.unwrap().as_int().unwrap();
        assert_ne!(moved, stack);
        let source = load_items(&db, &client.sde, ItemQuery::Id(stack)).await.unwrap().pop().unwrap();
        let created = load_items(&db, &client.sde, ItemQuery::Id(moved)).await.unwrap().pop().unwrap();
        assert_eq!((source.owner_id, source.location_id, source.flag_id, source.quantity), (actor.character, ship, FLAG_CARGO, i64::MAX - CAP));
        assert_eq!((created.owner_id, created.location_id, created.flag_id, created.quantity), (actor.character, station_id, FLAG_HANGAR, CAP));
        let (mut seen_source, mut seen_created) = (false, false);
        while let Ok((event, args)) = notices.try_recv() {
            if event != "OnItemChange" { continue; }
            let Some(id) = descriptor.get(&args[0], "itemID").and_then(|v| v.as_int()) else { continue };
            if id == stack {
                seen_source = true;
                assert_eq!(descriptor.get(&args[0], "quantity"), Some(Value::Int(CAP)));
                assert_eq!(descriptor.get(&args[0], "locationID"), Some(Value::Int(ship)));
                assert_eq!(descriptor.get(&args[0], "flagID"), Some(Value::Int(i64::from(FLAG_CARGO))));
                assert_eq!(args[1], Value::Dict(vec![(Value::Int(infinity_core::consts::IX_STACK_SIZE), Value::Int(CAP))]));
            } else if id == moved {
                seen_created = true;
                assert_eq!(descriptor.get(&args[0], "quantity"), Some(Value::Int(CAP)));
                assert_eq!(descriptor.get(&args[0], "locationID"), Some(Value::Int(station_id)));
                assert_eq!(descriptor.get(&args[0], "flagID"), Some(Value::Int(i64::from(FLAG_HANGAR))));
                assert_eq!(args[1], Value::Dict(vec![
                    (Value::Int(infinity_core::consts::IX_LOCATION_ID), Value::Int(ship)),
                    (Value::Int(infinity_core::consts::IX_FLAG), Value::Int(i64::from(FLAG_CARGO))),
                ]));
            }
        }
        assert!(seen_source && seen_created, "expected OnItemChange for both touched rows");
    }).await;
    crate::services::tests::fixture::cleanup(&db, actor).await.unwrap();
    assert_eq!(crate::services::tests::fixture::remaining(&db, actor).await.unwrap(), 0);
    if let Err(panic) = result { std::panic::resume_unwind(panic); }
}
