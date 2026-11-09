//! Bound broker content reads through the service dispatcher.
use infinity_core::{consts::{CATEGORY_SHIP, FLAG_FITTING_FIRST, FLAG_HANGAR, GROUP_STATION}};
use crate::{dao::items::{combat, transfers}, services::{self, Call, CallError}};
use marshal::{DbRowDescriptor, DbType, Value};

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn contents_dispatch_and_native_charge_roundtrip() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let fixture = services::tests::fixture::create(&db).await.unwrap();
    let (mut client, _peer) = services::tests::client(db.clone(), fixture.character).await;
    let station = client.sde.map.stations().find(|s| s.solar_system_id.is_some()).unwrap().station_id;
    let system = client.sde.map.station(station).unwrap().solar_system_id.unwrap();
    let ship_type = client.sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SHIP)).unwrap().type_id;
    let charge_type = client.sde.types.iter().find(|t| t.category_id == Some(infinity_core::consts::CATEGORY_CHARGE)).unwrap().type_id;
    let module_type = client.sde.types.iter().find(|t| t.category_id == Some(infinity_core::consts::CATEGORY_MODULE)).unwrap().type_id;
    client.session.apply(vec![("stationid", Value::Int(station)), ("solarsystemid", Value::Int(system))]);
    let ship = transfers::insert(&db, ship_type, fixture.character, station, FLAG_HANGAR, 1).await.unwrap();
    db.execute("UPDATE game.items SET singleton=true WHERE item_id=$1", &[&ship]).await.unwrap();
    let module = transfers::insert(&db, module_type, fixture.character, ship, FLAG_FITTING_FIRST, 1).await.unwrap();
    combat::apply_loads(&db, ship, &[(module, FLAG_FITTING_FIRST, charge_type, 2, 0)]).await.unwrap();
    let bind = Value::tuple([Value::Int(station), Value::Int(GROUP_STATION)]);
    let kwargs = Value::None;
    for args in [vec![Value::Int(ship), Value::Int(station)], vec![Value::Int(ship), Value::Int(station), Value::None], vec![Value::Int(ship), Value::Int(station), Value::Int(system)]] {
        let call = Call { service: "invbroker", method: "GetContainerContents", args: &args, kwargs: &kwargs, bound: true, bind_params: Some(&bind) };
        let result = services::dispatch(&mut client, &call).await.unwrap();
        let result = marshal::loads(&marshal::dumps(&result)).unwrap();
        let rows = result.items().unwrap();
        assert_eq!(rows.len(), 2);
        let charge = rows.iter().find(|row| matches!(row, Value::DbRow { descriptor, .. } if DbRowDescriptor::from_value(descriptor).is_some_and(|d| d.columns[0].1 == DbType::Str))).unwrap();
        let Value::DbRow { descriptor, .. } = charge else { unreachable!() };
        let descriptor = DbRowDescriptor::from_value(descriptor).unwrap();
        let mut expected_header = services::item_descriptor();
        expected_header.columns[0].1 = DbType::Str;
        assert_eq!(descriptor, expected_header);
        let charge_sde = client.sde.types.get(charge_type).unwrap();
        for (name, value) in [
            ("typeID", Value::Int(i64::from(charge_type))),
            ("ownerID", Value::Int(fixture.character)),
            ("flagID", Value::Int(i64::from(FLAG_FITTING_FIRST))),
            ("groupID", Value::Int(i64::from(charge_sde.group_id.unwrap()))),
            ("categoryID", Value::Int(i64::from(charge_sde.category_id.unwrap()))),
            ("customInfo", Value::None),
        ] { assert_eq!(descriptor.get(charge, name), Some(value)); }
        assert_eq!(descriptor.get(charge, "itemID"), Some(Value::tuple([Value::Int(ship), Value::Int(i64::from(FLAG_FITTING_FIRST)), Value::Int(i64::from(charge_type))])));
        assert_eq!(descriptor.get(charge, "locationID"), Some(Value::Int(ship)));
        assert_eq!(descriptor.get(charge, "quantity"), Some(Value::Int(2)));
    }
    for (args, bound, params, keywords) in [
        (vec![Value::Int(ship), Value::Int(station + 1)], true, bind.clone(), Value::None),
        (vec![Value::Int(ship), Value::Int(station)], false, bind.clone(), Value::None),
        (vec![Value::Int(ship), Value::Int(station)], true, Value::list([Value::Int(station), Value::Int(GROUP_STATION)]), Value::None),
        (vec![Value::Int(ship), Value::Int(station)], true, bind.clone(), Value::Dict(vec![(Value::str("bad"), Value::Int(1))])),
    ] {
        let call = Call { service: "invbroker", method: "GetContainerContents", args: &args, kwargs: &keywords, bound, bind_params: Some(&params) };
        assert!(matches!(services::dispatch(&mut client, &call).await, Err(CallError::User { .. })));
    }
    let wrong_system = [Value::Int(ship), Value::Int(station), Value::Int(system + 1)];
    let wrong = Call { service: "invbroker", method: "GetContainerContents", args: &wrong_system, kwargs: &kwargs, bound: true, bind_params: Some(&bind) };
    assert!(matches!(services::dispatch(&mut client, &wrong).await, Err(CallError::User { .. })));
    db.execute("UPDATE game.items SET owner_id=$2 WHERE item_id=$1", &[&ship, &(fixture.character + 1)]).await.unwrap();
    let args = [Value::Int(ship), Value::Int(station)];
    let stale = Call { service: "invbroker", method: "GetContainerContents", args: &args, kwargs: &kwargs, bound: true, bind_params: Some(&bind) };
    assert!(matches!(services::dispatch(&mut client, &stale).await, Err(CallError::User { .. })));
    db.execute("UPDATE game.items SET owner_id=$2 WHERE item_id=$1", &[&ship, &fixture.character]).await.unwrap();
    db.execute("DELETE FROM game.module_charges WHERE ship_id=$1", &[&ship]).await.unwrap();
    services::tests::fixture::cleanup(&db, fixture).await.unwrap();
}
