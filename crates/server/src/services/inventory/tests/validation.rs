//! Validation of submitted inventory snapshots through the bound broker.
use infinity_core::{consts::{CARGO_CONTAINER_GROUPS, FLAG_HANGAR, GROUP_STATION}};
use crate::{dao::items::{loading::ItemQuery, transfers}, services::{self, Call, CallError, items::load_items}};
use marshal::{DbType, Value};

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn validation_dispatch_checks_snapshots_and_current_custody() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let fixture = services::tests::fixture::create(&db).await.unwrap();
    let (mut client, _peer) = services::tests::client(db.clone(), fixture.character).await;
    let station = client.sde.map.stations().find(|s| s.solar_system_id.is_some()).unwrap().station_id;
    let system = client.sde.map.station(station).unwrap().solar_system_id.unwrap();
    client.session.apply(vec![("stationid", Value::Int(station)), ("solarsystemid", Value::Int(system))]);
    let container_type = client.sde.types.iter().find(|t| t.group_id.is_some_and(|g| CARGO_CONTAINER_GROUPS.contains(&g))).unwrap().type_id;
    let other_type = client.sde.types.iter().find(|t| t.group_id.is_some_and(|g| !CARGO_CONTAINER_GROUPS.contains(&g))).unwrap().type_id;
    let good = transfers::insert(&db, container_type, fixture.character, station, FLAG_HANGAR, 1).await.unwrap();
    let wrong = transfers::insert(&db, other_type, fixture.character, station, FLAG_HANGAR, 1).await.unwrap();
    let stacked = transfers::insert(&db, container_type, fixture.character, station, FLAG_HANGAR, 1).await.unwrap();
    db.execute("UPDATE game.items SET singleton=true WHERE item_id=ANY($1)", &[&vec![good, wrong]]).await.unwrap();
    let get = |id| ItemQuery::Id(id);
    let item = load_items(&db, &client.sde, get(good)).await.unwrap().pop().unwrap();
    let wrong_item = load_items(&db, &client.sde, get(wrong)).await.unwrap().pop().unwrap();
    let stacked_item = load_items(&db, &client.sde, get(stacked)).await.unwrap().pop().unwrap();
    let mut stale = item.clone(); stale.owner_id += 1;
    let mut missing = item.clone(); missing.item_id += 1000000;
    let bind = Value::tuple([Value::Int(station), Value::Int(GROUP_STATION)]);
    let kwargs = Value::None;
    let args = [Value::list([item.row(), stale.row(), missing.row(), wrong_item.row(), stacked_item.row(), stale.row()])];
    let call = Call { service: "invbroker", method: "ValidateItemListCanBeOpened", args: &args, kwargs: &kwargs, bound: true, bind_params: Some(&bind) };
    assert_eq!(services::dispatch(&mut client, &call).await.unwrap(), Value::list([Value::Int(good), Value::Int(missing.item_id), Value::Int(wrong), Value::Int(stacked)]));
    let args = [Value::list([item.row()])];
    let call = Call { service: "invbroker", method: "ValidateItemListCanBeOpened", args: &args, kwargs: &kwargs, bound: true, bind_params: Some(&bind) };
    assert_eq!(services::dispatch(&mut client, &call).await.unwrap(), Value::list([]));
    db.execute("UPDATE game.items SET owner_id=$2 WHERE item_id=$1", &[&good, &(fixture.character + 1)]).await.unwrap();
    assert_eq!(services::dispatch(&mut client, &call).await.unwrap(), Value::list([Value::Int(good)]));
    db.execute("UPDATE game.items SET owner_id=$2 WHERE item_id=$1", &[&good, &fixture.character]).await.unwrap();
    let mut bad_descriptor = services::item_descriptor(); bad_descriptor.columns[0].1 = DbType::Str;
    for forged in {
        let mut by_type = item.clone(); by_type.type_id = other_type;
        let mut by_location = item.clone(); by_location.location_id += 1;
        let mut by_flag = item.clone(); by_flag.flag_id += 1;
        [by_type, by_location, by_flag]
    } {
        let args = [Value::list([forged.row()])];
        let call = Call { service: "invbroker", method: "ValidateItemListCanBeOpened", args: &args, kwargs: &kwargs, bound: true, bind_params: Some(&bind) };
        assert_eq!(services::dispatch(&mut client, &call).await.unwrap(), Value::list([Value::Int(good)]));
    }
    let malformed = bad_descriptor.row(&[Value::Int(good), Value::Int(i64::from(container_type)), Value::Int(fixture.character), Value::Int(station), Value::Int(i64::from(FLAG_HANGAR)), Value::Int(1), Value::Int(1), Value::Int(1), Value::None]);
    let null_owner = services::item_descriptor().row(&[Value::Int(good), Value::Int(i64::from(container_type)), Value::None, Value::Int(station), Value::Int(i64::from(FLAG_HANGAR)), Value::Int(1), Value::Int(1), Value::Int(1), Value::None]);
    let mut zero = item.clone(); zero.item_id = 0;
    let mut negative = item.clone(); negative.item_id = -1;
    for row in [Value::Int(good), malformed, null_owner, zero.row(), negative.row()] {
        let args = [Value::list([row])];
        let call = Call { service: "invbroker", method: "ValidateItemListCanBeOpened", args: &args, kwargs: &kwargs, bound: true, bind_params: Some(&bind) };
        assert!(matches!(services::dispatch(&mut client, &call).await, Err(CallError::User { .. })));
    }
    for (bound, params, keywords) in [(false, bind.clone(), Value::None), (true, Value::list([Value::Int(station), Value::Int(GROUP_STATION)]), Value::None), (true, bind.clone(), Value::Dict(vec![(Value::str("bad"), Value::Int(1))]))] {
        let args = [Value::list([])];
        let call = Call { service: "invbroker", method: "ValidateItemListCanBeOpened", args: &args, kwargs: &keywords, bound, bind_params: Some(&params) };
        assert!(matches!(services::dispatch(&mut client, &call).await, Err(CallError::User { .. })));
    }
    let args = [Value::tuple([])];
    let call = Call { service: "invbroker", method: "ValidateItemListCanBeOpened", args: &args, kwargs: &kwargs, bound: true, bind_params: Some(&bind) };
    assert_eq!(services::dispatch(&mut client, &call).await.unwrap(), Value::list([]));
    services::tests::fixture::cleanup(&db, fixture).await.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn validation_accepts_only_singleton_cargo_container_groups() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let fixture = services::tests::fixture::create(&db).await.unwrap();
    let (mut client, _peer) = services::tests::client(db.clone(), fixture.character).await;
    let station = client.sde.map.stations().find(|s| s.solar_system_id.is_some()).unwrap().station_id;
    client.session.apply(vec![("stationid", Value::Int(station))]);
    let mut ids = Vec::new();
    for group in CARGO_CONTAINER_GROUPS {
        let ty = client.sde.types.iter().find(|t| t.group_id == Some(group)).unwrap().type_id;
        ids.push(transfers::insert(&db, ty, fixture.character, station, FLAG_HANGAR, 1).await.unwrap());
    }
    let ship = client.sde.types.iter().find(|t| t.category_id == Some(infinity_core::consts::CATEGORY_SHIP)).unwrap().type_id;
    let wreck = client.sde.types.iter().find(|t| t.group_id == Some(infinity_core::consts::GROUP_WRECK)).unwrap().type_id;
    let ship = transfers::insert(&db, ship, fixture.character, station, FLAG_HANGAR, 1).await.unwrap();
    let wreck = transfers::insert(&db, wreck, fixture.character, station, FLAG_HANGAR, 1).await.unwrap();
    let singleton_ids = [ids.as_slice(), &[ship, wreck]].concat();
    db.execute("UPDATE game.items SET singleton=true WHERE item_id=ANY($1)", &[&singleton_ids]).await.unwrap();
    let stacked_type = client.sde.types.iter().find(|t| t.group_id == Some(CARGO_CONTAINER_GROUPS[0])).unwrap().type_id;
    let stacked = transfers::insert(&db, stacked_type, fixture.character, station, FLAG_HANGAR, 1).await.unwrap();
    ids.extend([ship, wreck, stacked]);
    let rows = load_items(&db, &client.sde, ItemQuery::Ids(&ids)).await.unwrap();
    let snapshots = ids.iter().map(|id| rows.iter().find(|item| item.item_id == *id).unwrap().row()).collect::<Vec<_>>();
    let args = [Value::list(snapshots)];
    let bind = Value::tuple([Value::Int(station), Value::Int(GROUP_STATION)]);
    let kwargs = Value::None;
    let call = Call { service: "invbroker", method: "ValidateItemListCanBeOpened", args: &args, kwargs: &kwargs, bound: true, bind_params: Some(&bind) };
    assert_eq!(services::dispatch(&mut client, &call).await.unwrap(), Value::list([Value::Int(ship), Value::Int(wreck), Value::Int(stacked)]));
    services::tests::fixture::cleanup(&db, fixture).await.unwrap();
}
