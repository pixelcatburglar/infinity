use super::*;
use crate::services::{self, CallError, value_field};

const REFUSALS: &[&str] = &[
    "UserAbandonPlanet", "UserUpdateNetwork", "GMGetLocalDistributionReport",
    "UserLaunchCommodities", "UserTransferCommodities", "GetProgramResultInfo",
    "GMForceInstallProgram", "GMConvertCommandCenter", "GMAddCommodity",
    "GMGetSynchedServerState", "GMRunDepletionSim", "GMGetCompleteResource", "GMCreateNuggetLayer",
];

/// clientPlanet.py: UserAbandonPlanet(), UserUpdateNetwork(changes),
/// GMGetLocalDistributionReport(planetID, point), UserLaunchCommodities(pinID, commodities),
/// UserTransferCommodities(path, commodities), GetProgramResultInfo(pinID, typeID, heads, radius),
/// GMForceInstallProgram(pinID, typeID, cycleTime, lifetime, quantity, radius),
/// GMConvertCommandCenter(pinID), GMAddCommodity(pinID, typeID, quantity),
/// GMGetSynchedServerState(characterID), GMRunDepletionSim(typeID, info).
/// planetUISvc.py: GMGetCompleteResource(typeID, layer), GMCreateNuggetLayer(planetID, typeID).
fn action_args(method: &str, planet: i64, character: i64, item: i64, kind: i64) -> Vec<Value> {
    let pin = || Value::Int(item);
    let ty = || Value::Int(kind);
    match method {
        "UserAbandonPlanet" => vec![],
        "UserUpdateNetwork" => vec![Value::list([])],
        "GMGetLocalDistributionReport" => vec![Value::Int(planet), Value::tuple([Value::Float(0.0), Value::Float(0.0)])],
        "UserLaunchCommodities" => vec![pin(), Value::Dict(vec![(ty(), Value::Int(1))])],
        "UserTransferCommodities" => vec![Value::list([pin(), pin()]), Value::Dict(vec![(ty(), Value::Int(1))])],
        "GetProgramResultInfo" => vec![pin(), ty(), Value::list([]), Value::Float(0.0)],
        "GMForceInstallProgram" => vec![pin(), ty(), Value::Int(1), Value::Int(1), Value::Int(1), Value::Float(0.0)],
        "GMConvertCommandCenter" => vec![pin()],
        "GMAddCommodity" => vec![pin(), ty(), Value::Int(1)],
        "GMGetSynchedServerState" => vec![Value::Int(character)],
        "GMRunDepletionSim" => vec![ty(), keyval([("totalDuration", Value::Int(1)), ("points", Value::list([]))])],
        "GMGetCompleteResource" => vec![ty(), Value::str("base")],
        "GMCreateNuggetLayer" => vec![Value::Int(planet), ty()],
        _ => panic!("unknown action {method}"),
    }
}

/// clientPlanet.py CreateResourceInfo constructs the KeyVal passed to GetResourceData.
fn scan_info(kind: i64) -> Value {
    keyval([
        ("resourceTypeID", Value::Int(kind)), ("updateTime", Value::Int(0)),
        ("oldBand", Value::Int(0)), ("planetology", Value::Int(0)),
        ("advancedPlanetology", Value::Int(0)), ("remoteSensing", Value::Int(1)),
        ("proximity", Value::Int(0)), ("newBand", Value::Int(1)),
    ])
}

/// planetSvc.py supplies planetID and characterID for networks, only planetID for pins/extractors;
/// clientPlanet.py supplies no arguments for planet/resource info and a KeyVal for resource data.
fn read_args(method: &str, planet: i64, character: i64, kind: i64) -> Vec<Value> {
    match method {
        "GetFullNetworkForOwner" => vec![Value::Int(planet), Value::Int(character)],
        "GetCommandPinsForPlanet" | "GetExtractorsForPlanet" => vec![Value::Int(planet)],
        "GetResourceData" => vec![scan_info(kind)],
        _ => vec![],
    }
}

fn refusal(error: CallError, text: &str) {
    let CallError::User { msg, dict } = error else { panic!("expected UserError") };
    assert_eq!(msg, infinity_core::messages::MSG_CUSTOM_NOTIFY);
    assert_eq!(dict, vec![("notify".into(), Value::unicode(text))]);
    let value = crate::client::user_error(&msg, dict);
    assert_eq!(marshal::loads(&marshal::dumps(&value)).unwrap(), value);
}

#[test]
fn pure_shapes_refusals_and_fallthrough() {
    let empty = Value::Dict(vec![]);
    for method in REFUSALS {
        let call = Call { service: "planetMgr", method, args: &[Value::None], kwargs: &empty, bound: false, bind_params: None };
        refusal(handle(&call, |_| None).unwrap().unwrap_err(), messages::planetary::UNAVAILABLE);
    }
    for (service, method) in [("planetMgr", "Unknown"), ("other", "GetPlanetInfo")] {
        let call = Call { service, method, args: &[], kwargs: &empty, bound: false, bind_params: None };
        assert!(handle(&call, |_| None).is_none());
    }
    for method in ["GetPlanetsForChar", "GetMyLaunchesDetails"] {
        let call = Call { service: "planetMgr", method, args: &[], kwargs: &empty, bound: false, bind_params: None };
        let value = handle(&call, |_| None).unwrap().unwrap();
        assert_eq!(value, Value::list([]));
        assert_eq!(marshal::loads(&marshal::dumps(&value)).unwrap(), value);
    }
    let planet = Celestial { item_id: 1, type_id: Some(2), solar_system_id: Some(3),
        group_id: Some(i64::from(GROUP_PLANET)), x: None, y: None, z: None,
        radius: Some(4.0), celestial_index: Some(5) };
    let id = Value::Int(1);
    for (method, expected) in [
        ("GetFullNetworkForOwner", Value::tuple([Value::list([]), Value::list([])])),
        ("GetCommandPinsForPlanet", Value::Dict(vec![])),
        ("GetExtractorsForPlanet", Value::list([])),
        ("GetPlanetResourceInfo", Value::Dict(vec![])),
        ("GetResourceData", keyval([("data", Value::None)])),
    ] {
        let args = read_args(method, 1, 1, 2);
        let call = Call { service: "planetMgr", method, args: &args, kwargs: &empty, bound: true, bind_params: Some(&id) };
        let value = handle(&call, |_| Some(&planet)).unwrap().unwrap();
        assert_eq!(value, expected);
        assert_eq!(marshal::loads(&marshal::dumps(&value)).unwrap(), value);
    }
}

#[test]
fn planet_info_preserves_double_and_rejects_missing_metadata() {
    let mut planet = Celestial { item_id: 1, type_id: Some(2), solar_system_id: Some(3),
        group_id: Some(i64::from(GROUP_PLANET)), x: None, y: None, z: None,
        radius: Some(12345678.123456789), celestial_index: Some(4) };
    let info = planet_info(&planet).unwrap();
    assert_eq!(value_field(&info, "radius"), Some(&Value::Float(planet.radius.unwrap())));
    assert_eq!(value_field(&info, "celestialIndex"), Some(&Value::Int(4)));
    assert!(value_field(&info, "pins").is_none());
    assert!(value_field(&info, "currentSimTime").is_none());
    assert_eq!(marshal::loads(&marshal::dumps(&info)).unwrap(), info);
    planet.radius = None;
    refusal(planet_info(&planet).unwrap_err(), messages::planetary::INVALID_PLANET);
    planet.radius = Some(1.0);
    planet.celestial_index = None;
    refusal(planet_info(&planet).unwrap_err(), messages::planetary::INVALID_PLANET);
    planet.celestial_index = Some(1);
    planet.type_id = None;
    refusal(planet_info(&planet).unwrap_err(), messages::planetary::INVALID_PLANET);
    planet.type_id = Some(1);
    planet.solar_system_id = None;
    refusal(planet_info(&planet).unwrap_err(), messages::planetary::INVALID_PLANET);
}

async fn snapshot(db: &crate::db::Db, ids: &[i64], items: &[i64]) -> String {
    use crate::dao::items::loading::{load_items, ItemQuery};
    let mut result = String::new();
    for id in items {
        for item in load_items(db, ItemQuery::Id(*id)).await.unwrap() {
            result.push_str(&format!("{:?}", (item.item_id, item.type_id, item.owner_id, item.location_id, item.flag_id, item.quantity, item.singleton, item.custom_info)));
        }
    }
    for id in ids {
        result.push_str(&format!("{:?}", (crate::dao::characters::selection::ship_id(db, *id).await.unwrap(),
            crate::dao::characters::crimewatch::solar_system(db, *id).await.unwrap(),
            crate::dao::wallets::characters::balance(db, *id).await.unwrap())));
    }
    result
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE and external flock"]
async fn real_dispatch_empty_planets_and_refusals_preserve_state() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let owner = services::tests::fixture::create(&db).await.unwrap();
    let other = services::tests::fixture::create(&db).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (mut client, peer) = services::tests::client(test_db.clone(), owner.character).await;
        client.login.user_id = owner.account;
        let valid = |p: &&Celestial| p.group_id == Some(i64::from(GROUP_PLANET)) && p.type_id.is_some() && p.solar_system_id.is_some() && p.radius.is_some() && p.celestial_index.is_some();
        let first = client.sde.map.all_celestials().filter(valid).next().unwrap();
        let second = client.sde.map.all_celestials().filter(valid).find(|p| p.type_id != first.type_id || p.solar_system_id != first.solar_system_id || p.radius != first.radius || p.celestial_index != first.celestial_index).unwrap();
        let planets = [first.item_id, second.item_id];
        let nonplanet = client.sde.map.all_celestials().find(|p| p.group_id != Some(i64::from(GROUP_PLANET))).unwrap().item_id;
        let system = client.sde.map.celestial(planets[0]).unwrap().solar_system_id.unwrap();
        let kind = client.sde.types.iter().find(|t| t.category_id == Some(infinity_core::consts::CATEGORY_SHIP)).unwrap().type_id;
        let own_item = crate::dao::items::transfers::insert(&test_db, kind, owner.character, system, 0, 1).await.unwrap();
        let other_item = crate::dao::items::transfers::insert(&test_db, kind, other.character, system, 0, 1).await.unwrap();
        let before = snapshot(&test_db, &[owner.character, other.character], &[own_item, other_item]).await;
        let (notify, mut notifications) = tokio::sync::mpsc::unbounded_channel();
        client.notify_tx = notify;
        let empty = Value::Dict(vec![]);
        let session: Vec<_> = client.session.attributes().map(|(k,v)| (k.to_owned(), v.clone())).collect();
        let outbox = client.outbox.len();
        assert!(client.world.with(system, |_| ()).await.is_none());
        for id in planets.iter().copied() {
            let bound = Value::Int(id);
            for method in ["GetPlanetsForChar", "GetMyLaunchesDetails", "GetFullNetworkForOwner", "GetCommandPinsForPlanet", "GetExtractorsForPlanet", "GetPlanetInfo", "GetPlanetResourceInfo", "GetResourceData"] {
                let args = read_args(method, id, other.character, i64::from(kind));
                let bound_read = !matches!(method, "GetPlanetsForChar" | "GetMyLaunchesDetails");
                let call = Call { service: "planetMgr", method, args: &args, kwargs: &empty, bound: bound_read, bind_params: bound_read.then_some(&bound) };
                let value = services::dispatch(&mut client, &call).await.unwrap();
                let expected = match method {
                    "GetPlanetsForChar" | "GetMyLaunchesDetails" | "GetExtractorsForPlanet" => Value::list([]),
                    "GetFullNetworkForOwner" => Value::tuple([Value::list([]), Value::list([])]),
                    "GetCommandPinsForPlanet" | "GetPlanetResourceInfo" => Value::Dict(vec![]),
                    "GetResourceData" => keyval([("data", Value::None)]),
                    _ => planet_info(client.sde.map.celestial(id).unwrap()).unwrap(),
                };
                assert_eq!(value, expected);
                assert_eq!(marshal::loads(&marshal::dumps(&value)).unwrap(), value);
            }
            let bind_args = [bound.clone(), Value::tuple([Value::str("GetPlanetInfo"), Value::tuple([]), empty.clone()])];
            let call = Call { service: "planetMgr", method: "MachoBindObject", args: &bind_args, kwargs: &empty, bound: false, bind_params: None };
            let result = services::dispatch(&mut client, &call).await.unwrap();
            let [callback, info] = result.items().unwrap() else { panic!("bind result") };
            assert!(matches!(callback, Value::Callback(_)));
            assert_eq!(*info, planet_info(client.sde.map.celestial(id).unwrap()).unwrap());
        }
        let good = Value::Int(planets[0]);
        let tuple = Value::tuple([good.clone()]);
        let wrong = Value::Int(nonplanet);
        let missing = Value::Int(i64::MAX);
        for (bound, params) in [(false, Some(&good)), (true, None), (true, Some(&tuple)), (true, Some(&wrong)), (true, Some(&missing))] {
            let args = [good.clone()];
            let call = Call { service: "planetMgr", method: "GetPlanetInfo", args: &args, kwargs: &empty, bound, bind_params: params };
            refusal(services::dispatch(&mut client, &call).await.unwrap_err(), messages::planetary::INVALID_PLANET);
        }
        for method in ["GetFullNetworkForOwner", "GetCommandPinsForPlanet", "GetExtractorsForPlanet"] {
            let args = [Value::Int(planets[1])];
            let call = Call { service: "planetMgr", method, args: &args, kwargs: &empty, bound: true, bind_params: Some(&good) };
            refusal(services::dispatch(&mut client, &call).await.unwrap_err(), messages::planetary::INVALID_PLANET);
        }
        for role in [0, i64::MAX] {
            client.login.role = role;
            for method in REFUSALS {
                for (character, item) in [(owner.character, own_item), (other.character, other_item)] {
                    let args = action_args(method, planets[0], character, item, i64::from(kind));
                    let call = Call { service: "planetMgr", method, args: &args, kwargs: &empty, bound: true, bind_params: Some(&good) };
                    refusal(services::dispatch(&mut client, &call).await.unwrap_err(), messages::planetary::UNAVAILABLE);
                }
                let args = [Value::None];
                let call = Call { service: "planetMgr", method, args: &args, kwargs: &empty, bound: true, bind_params: Some(&good) };
                refusal(services::dispatch(&mut client, &call).await.unwrap_err(), messages::planetary::UNAVAILABLE);
            }
        }
        for (service, method) in [("planetMgr", "Unknown"), ("other", "GetPlanetInfo")] {
            let call = Call { service, method, args: &[], kwargs: &empty, bound: false, bind_params: None };
            assert!(dispatch(&client, &call).is_none());
        }
        assert_eq!(snapshot(&test_db, &[owner.character, other.character], &[own_item, other_item]).await, before);
        assert_eq!(client.session.attributes().map(|(k,v)| (k.to_owned(), v.clone())).collect::<Vec<_>>(), session);
        assert_eq!(client.outbox.len(), outbox);
        assert!(client.world.with(system, |_| ()).await.is_none());
        assert!(notifications.try_recv().is_err());
        let mut byte = [0];
        assert_eq!(peer.try_read(&mut byte).unwrap_err().kind(), std::io::ErrorKind::WouldBlock);
    }).await;
    services::tests::fixture::cleanup(&db, owner).await.unwrap();
    services::tests::fixture::cleanup(&db, other).await.unwrap();
    result.unwrap();
}
