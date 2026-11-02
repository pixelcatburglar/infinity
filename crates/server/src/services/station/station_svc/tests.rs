use super::*;
use crate::services::value_field;

fn call<'a>(method: &'a str, args: &'a [Value], kwargs: &'a Value) -> Call<'a> {
    Call { service: "stationSvc", method, args, kwargs, bound: false, bind_params: None }
}

fn station(id: i64) -> Station {
    Station { station_id: id, type_id: Some(id), owner_id: Some(id), solar_system_id: Some(id),
        constellation_id: Some(id), region_id: Some(id), operation_id: Some(id),
        x: None, y: None, z: None, security: None }
}

fn wire(value: Value) -> Value { marshal::loads(&marshal::dumps(&value)).unwrap() }

#[test]
fn malformed_arguments_are_rejected() {
    let empty = Value::Dict(vec![]);
    let nonempty = Value::Dict(vec![(Value::str("id"), Value::Int(1))]);
    let cases = [
        (vec![], &empty), (vec![Value::Int(1), Value::Int(2)], &empty),
        (vec![Value::str("1")], &empty), (vec![Value::Bool(true)], &empty),
        (vec![Value::None], &empty), (vec![Value::BigLong(vec![0xff; 9])], &empty),
        (vec![Value::Int(0)], &empty), (vec![Value::Int(-1)], &empty),
        (vec![Value::Int(1)], &nonempty), (vec![Value::Int(1)], &Value::Bool(false)),
    ];
    for (args, kwargs) in &cases {
        assert!(matches!(request_id(&call("GetStation", args, kwargs)), Err(CallError::User { .. })));
    }
    assert_eq!(request_id(&call("GetStation", &[Value::Int(1)], &Value::None)).unwrap(), 1);
}

#[test]
fn full_i64_wire_and_required_fields() {
    let id = i64::MAX;
    assert_eq!(request_id(&call("GetStation", &[Value::Int(id)], &Value::None)).unwrap(), id);
    let row = wire(station_row(&station(id)).unwrap());
    let Value::Instance(class, state) = &row else { panic!("expected KeyVal instance") };
    assert_eq!(**class, Value::str("utillib.KeyVal"));
    assert!(matches!(state.as_ref(), Value::Dict(_)), "expected KeyVal attribute dictionary");
    for key in ["stationTypeID", "ownerID", "operationID", "solarSystemID", "constellationID", "regionID"] {
        assert_eq!(value_field(&row, key), Some(&Value::Int(id)));
    }
    assert_eq!(value_field(&row, "description"), Some(&Value::None));
    let owner = wire(owner_row(&station(id)).unwrap());
    for key in ["stationID", "solarSystemID"] { assert_eq!(value_field(&owner, key), Some(&Value::Int(id))); }
    let system = SolarSystem { solar_system_id: id, name: None, constellation_id: None, region_id: None,
        faction_id: None, security: None, x: None, y: None, z: None };
    assert_eq!(value_field(&wire(system_row(&system)), "factionID"), Some(&Value::None));
    let system = SolarSystem { faction_id: Some(id), ..system };
    assert_eq!(value_field(&wire(system_row(&system)), "factionID"), Some(&Value::Int(id)));
    for field in 0..6 {
        let mut row = station(id);
        match field { 0 => row.type_id = None, 1 => row.owner_id = None, 2 => row.operation_id = None,
            3 => row.solar_system_id = None, 4 => row.constellation_id = None, _ => row.region_id = None }
        assert!(matches!(station_row(&row), Err(CallError::Internal(_))));
        if field == 3 { assert!(matches!(owner_row(&row), Err(CallError::Internal(_)))); }
    }
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn sde_dispatch_contract() {
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let (mut client, _peer) = crate::services::tests::client(db, 1).await;

    let kwargs = Value::None;
    let stations: Vec<_> = client.sde.map.stations().filter_map(|s| s.owner_id.map(|o| (s.station_id, o))).collect();
    let (station_id, owner) = *stations.first().expect("SDE station with owner");
    let system_id = client.sde.map.station(station_id).unwrap().solar_system_id.expect("station system");
    let expected_station = station_row(client.sde.map.station(station_id).unwrap()).unwrap();
    let expected_system = system_row(client.sde.map.system(system_id).unwrap());
    for (method, id, expected) in [("GetStation", station_id, expected_station), ("GetSolarSystem", system_id, expected_system)] {
        let args = [Value::Int(id)];
        let result = crate::services::dispatch(&mut client, &call(method, &args, &kwargs)).await.unwrap();
        assert_eq!(wire(result), wire(expected));
    }
    let mut counts = std::collections::HashMap::new();
    for &(_, id) in &stations { *counts.entry(id).or_insert(0usize) += 1; }
    assert!(!counts.contains_key(&i64::MAX), "i64::MAX must be an owner with no stations");
    let mut owners = vec![i64::MAX, owner];
    owners.push(*counts.iter().find(|(_, count)| **count == 1).expect("SDE owner with one station").0);
    owners.push(*counts.iter().find(|(_, count)| **count > 1).expect("SDE owner with multiple stations").0);
    for id in owners {
        let expected = Value::list(client.sde.map.stations().filter(|s| s.owner_id == Some(id)).map(|s| owner_row(s).unwrap()));
        let args = [Value::Int(id)];
        let result = crate::services::dispatch(&mut client, &call("GetStationsForOwner", &args, &kwargs)).await.unwrap();
        assert_eq!(wire(result), wire(expected));
    }
    let missing = [Value::Int(i64::MAX)];
    assert_eq!(crate::services::dispatch(&mut client, &call("GetStation", &missing, &kwargs)).await.unwrap(), Value::None);
    assert!(matches!(crate::services::dispatch(&mut client, &call("GetSolarSystem", &missing, &kwargs)).await, Err(CallError::User { .. })));
    assert!(dispatch(&mut client, &call("GetSystemsForAlliance", &missing, &kwargs)).await.is_none());
    assert!(dispatch(&mut client, &call("unknown", &missing, &kwargs)).await.is_none());
    client.session.apply(vec![("stationid", Value::Int(station_id))]);
    let row = client.sde.map.station(station_id).unwrap();
    let expected = Value::list([row.owner_id, Some(row.station_id), row.operation_id, row.type_id].into_iter().map(|v| v.map(Value::Int).unwrap_or(Value::None)));
    let result = crate::services::dispatch(&mut client, &call("GetStationItemBits", &[], &kwargs)).await.unwrap();
    assert_eq!(wire(result), wire(expected));
}
