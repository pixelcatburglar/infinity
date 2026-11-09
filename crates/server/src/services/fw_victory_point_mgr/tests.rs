use super::*;
use crate::services::{self, CallError};

fn call<'a>(service: &'a str, method: &'a str, args: &'a [Value], kwargs: &'a Value) -> Call<'a> {
    Call { service, method, args, kwargs, bound: false, bind_params: None }
}

fn wire(value: Value) -> Value { marshal::loads(&marshal::dumps(&value)).unwrap() }

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn faction_warfare_dispatch_contract() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let fixture = services::tests::fixture::create(&db).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
    let (mut client, _peer) = services::tests::client(test_db, fixture.character).await;
    let kwargs = Value::Dict(vec![]);
    let home = client.sde.factions.get(infinity_core::consts::FACTION_CALDARI).unwrap().solar_system_id.unwrap();
    let station = client.sde.map.stations().find(|station| station.solar_system_id.is_some_and(|id| client.sde.map.system(id).is_some())).expect("SDE station with system");
    let docked_system = station.solar_system_id.unwrap();
    let station_id = station.station_id;
    assert!(client.sde.map.system(home).is_some());
    for (system, station) in [(Value::Int(home), Value::None), (Value::Int(docked_system), Value::Int(station_id)), (Value::None, Value::None)] {
        client.session.apply(vec![("solarsystemid2", system.clone()), ("stationid", station)]);
        let actual = services::dispatch(&mut client, &call("fwVictoryPointMgr", "GetLocalVictoryPointState", &[], &kwargs)).await.unwrap();
        assert_eq!(wire(actual), wire(Value::tuple([system, Value::None])));
    }
    let hq = services::dispatch(&mut client, &call("fwWarzoneSolarsystem", "GetHQSystemIDs", &[], &kwargs)).await.unwrap();
    let Value::Dict(entries) = wire(hq) else { panic!("expected HQ mapping") };
    assert_eq!(entries.len(), 6);
    for faction in infinity_core::consts::WARFARE_FACTIONS {
        let home = client.sde.factions.get(faction).unwrap().solar_system_id.unwrap();
        assert!(client.sde.map.system(home).is_some());
        assert!(entries.contains(&(Value::Int(faction), Value::Int(home))));
    }
    let arg = [Value::Int(infinity_core::consts::FACTION_CALDARI)];
    for (service, method, args, expected) in [
        ("fwVictoryPointMgr", "GetAllVictoryPointStates", &[][..], Value::Dict(vec![])),
        ("facWarMgr", "GetStats_Character", &[][..], Value::Dict(vec![])),
        ("facWarMgr", "GetStats_Corp", &[][..], Value::Dict(vec![])),
        ("facWarMgr", "GetStats_Alliance", &[][..], Value::Dict(vec![])),
        ("shipCasterLandingPadMgr", "GetFactionLandingPads", &arg[..], Value::list([])),
        ("shipCasterLandingPadMgr", "GetAllLandingPads", &[][..], Value::list([])),
        ("shipCasterLauncherMgr", "GetFactionsWithShipcaster", &[][..], Value::list([])),
    ] {
        assert_eq!(wire(services::dispatch(&mut client, &call(service, method, args, &kwargs)).await.unwrap()), wire(expected));
    }
    let zone = wire(services::dispatch(&mut client, &call("map", "GetFacWarZoneInfo", &arg, &kwargs)).await.unwrap());
    for (key, expected) in [("systemUpgradeLevel", Value::Dict(vec![])), ("factionPoints", Value::Int(0)), ("maxWarZonePoints", Value::Int(1))] {
        assert_eq!(services::value_field(&zone, key), Some(&expected));
    }
    let stats = wire(services::dispatch(&mut client, &call("facWarMgr", "GetStats_TopAndAllKillsAndVPs", &[], &kwargs)).await.unwrap());
    let Value::Tuple(sides) = stats else { panic!("expected stats tuple") };
    assert_eq!(sides.len(), 2);
    for side in sides {
        let Value::Dict(groups) = side else { panic!("expected groups") };
        assert_eq!(groups.len(), 3);
        for group in [infinity_core::consts::GROUP_CHARACTER, infinity_core::consts::GROUP_CORPORATION, infinity_core::consts::GROUP_ALLIANCE] {
            let counters = &groups.iter().find(|(k, _)| *k == Value::Int(group)).unwrap().1;
            let Value::Dict(fields) = counters else { panic!("expected counters") };
            assert_eq!(fields.len(), 6);
            for key in ["killsY", "killsLW", "killsTotal", "vpY", "vpLW", "vpTotal"] { assert!(fields.contains(&(Value::str(key), Value::Int(0)))); }
        }
    }
    assert!(dispatch(&client, &call("other", "GetLocalVictoryPointState", &[], &kwargs)).is_none());
    assert!(dispatch(&client, &call("fwVictoryPointMgr", "unknown", &[], &kwargs)).is_none());
    assert!(matches!(services::dispatch(&mut client, &call("fwCharacterEnlistmentMgr", "CreateMyDirectEnlistment", &arg, &kwargs)).await, Err(CallError::User { .. })));
    assert_eq!(services::dispatch(&mut client, &call("fwVictoryPointMgr", "unknown", &[], &kwargs)).await.unwrap(), Value::None);
    assert_eq!(services::dispatch(&mut client, &call("unknownFactionService", "unknown", &[], &kwargs)).await.unwrap(), Value::None);
    }).await;
    services::tests::fixture::cleanup(&db, fixture).await.unwrap();
    result.unwrap();
}
