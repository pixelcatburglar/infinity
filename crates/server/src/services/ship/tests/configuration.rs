//! Dispatch regressions for ship-bay configuration and visibility.
use super::*;
use crate::services::tests::{client, fixture};

const KEYS: [&str; 4] = [
    "FleetHangar_AllowFleetAccess", "FleetHangar_AllowCorpAccess",
    "SMB_AllowFleetAccess", "SMB_AllowCorpAccess",
];

async fn transfer(db: &crate::db::Db, sde: &crate::sde::Sde, id: i64, owner: i64) {
    let item = crate::services::items::load_items(db, sde, crate::dao::items::loading::ItemQuery::Id(id)).await.unwrap().pop().unwrap();
    assert!(crate::dao::items::inventory::move_item(db, &item, item.location_id, item.flag_id, owner, item.singleton).await.unwrap());
}

async fn grant(db: &crate::db::Db, ship: i64, owner: i64) {
    db.transaction(|tx, _: &mut ()| Box::pin(async move {
        assert!(crate::dao::items::ship::lock_reload_ship(tx, ship).await?);
        assert!(crate::dao::items::ship_configuration::patch(tx, ship, owner, &crate::dao::items::ship_configuration::Patch { fleet_hangar_corp: Some(true), ..Default::default() }).await?);
        Ok::<_, crate::db::Error>(())
    }), |(), ()| async move { Ok(()) }).await.unwrap();
}

async fn rpc(c: &mut Client, method: &str, args: &[Value], _ship: i64) -> CallResult {
    let station = c.session.int("stationid");
    let location = station.or_else(|| c.session.int("solarsystemid")).unwrap();
    let bind = Value::tuple([Value::Int(location), Value::Int(if station.is_some() { 15 } else { 5 })]);
    let call = Call { service: "ship", method, args, kwargs: &Value::None, bound: true, bind_params: Some(&bind) };
    crate::services::dispatch(c, &call).await
}

fn assert_off(value: Value) {
    let Value::Dict(entries) = &value else { panic!("expected configuration dict") };
    assert_eq!(entries.len(), KEYS.len());
    for key in KEYS {
        assert_eq!(entries.iter().filter(|(k, v)| *k == Value::str(key) && *v == Value::Bool(false)).count(), 1);
    }
    assert_eq!(marshal::loads(&marshal::dumps(&value)).unwrap(), value);
}

fn assert_settings(value: Value, expected: [bool; 4]) {
    let Value::Dict(entries) = &value else { panic!("expected configuration dict") };
    assert_eq!(entries.len(), KEYS.len());
    for (key, expected) in KEYS.into_iter().zip(expected) {
        assert_eq!(entries.iter().filter(|(k, v)| *k == Value::str(key) && *v == Value::Bool(expected)).count(), 1);
    }
}

fn assert_readable(result: CallResult) {
    let Err(CallError::User { msg, dict }) = result else { panic!("expected readable user refusal") };
    assert_eq!(msg, infinity_core::messages::MSG_CUSTOM_NOTIFY);
    assert!(dict.iter().any(|(key, value)| key == "notify" && matches!(value, Value::Unicode(text) if !text.trim().is_empty())));
    let wire = crate::client::user_error(&msg, dict);
    assert_eq!(marshal::loads(&marshal::dumps(&wire)).unwrap(), wire);
}

#[derive(Clone, Copy)]
enum Scenario { Get, Refusals, Configure, Space }

async fn exercise(scenario: Scenario) {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let owner = fixture::create(&db).await.unwrap();
    let foreign = fixture::create(&db).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (mut c, _peer) = client(test_db.clone(), owner.character).await;
        c.login.user_id = owner.account;
        let station = c.sde.map.stations().find(|s| s.solar_system_id.is_some()).unwrap();
        let station_id = station.station_id;
        let system = station.solar_system_id.unwrap();
        let remote_station = c.sde.map.stations().find(|s| s.station_id != station_id).unwrap().station_id;
        let hull = c.sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(infinity_core::consts::CATEGORY_SHIP)
            && (!matches!(scenario, Scenario::Space) || t.attribute(infinity_core::consts::dogma::ATTR_FLEET_HANGAR_CAPACITY).is_some_and(|v| v.is_finite() && v > 0.0))).unwrap().type_id;
        let nonship_type = c.sde.types.iter().find(|t| t.category_id.is_some() && t.category_id != Some(infinity_core::consts::CATEGORY_SHIP)).unwrap().type_id;
        let current = crate::dao::items::creation::rookie_ship(&test_db, hull, owner.character, station_id, FLAG_HANGAR).await.unwrap();
        let local = crate::dao::items::creation::rookie_ship(&test_db, hull, owner.character, station_id, FLAG_HANGAR).await.unwrap();
        let remote = crate::dao::items::creation::rookie_ship(&test_db, hull, owner.character, remote_station, FLAG_HANGAR).await.unwrap();
        let other = crate::dao::items::creation::rookie_ship(&test_db, hull, foreign.character, station_id, FLAG_HANGAR).await.unwrap();
        let nonship = crate::dao::items::creation::rookie_ship(&test_db, nonship_type, owner.character, station_id, FLAG_HANGAR).await.unwrap();
        let invisible = crate::dao::items::creation::positioned(&test_db, hull, owner.character, system, [0.0; 3]).await.unwrap();
        crate::dao::characters::selection::set_ship(&test_db, owner.character, current).await.unwrap();
        crate::dao::characters::ship::dock(&test_db, owner.character, station_id, system).await.unwrap();
        c.session.apply(vec![("shipid", Value::Int(current)), ("stationid", Value::Int(station_id)), ("solarsystemid2", Value::Int(system)), ("solarsystemid", Value::None)]);
        match scenario {
            Scenario::Get => {
                for id in [current, local] {
                    assert_off(rpc(&mut c, "GetShipConfiguration", &[Value::Int(id)], current).await.unwrap());
                }
            }
            Scenario::Refusals => {
                for id in [remote, other, nonship, invisible, i64::MAX] {
                    assert_readable(rpc(&mut c, "GetShipConfiguration", &[Value::Int(id)], current).await);
                }
                assert_readable(rpc(&mut c, "GetShipConfiguration", &[Value::None], current).await);
            }
            Scenario::Configure => {
                let patch = Value::dict([(KEYS[0], Value::Bool(true))]);
                assert_eq!(rpc(&mut c, "ConfigureShip", &[patch], current).await.unwrap(), Value::None);
                let mut expected = [true, false, false, false];
                assert_settings(rpc(&mut c, "GetShipConfiguration", &[Value::Int(current)], current).await.unwrap(), expected);
                for index in 1..4 {
                    let patch = Value::dict([(KEYS[index], Value::Bool(true))]);
                    rpc(&mut c, "ConfigureShip", &[patch], current).await.unwrap();
                    expected[index] = true;
                    assert_settings(rpc(&mut c, "GetShipConfiguration", &[Value::Int(current)], current).await.unwrap(), expected);
                }
                let invalid = Value::dict([(KEYS[0], Value::Bool(false)), (KEYS[1], Value::Int(1))]);
                assert_readable(rpc(&mut c, "ConfigureShip", &[invalid], current).await);
                assert_settings(rpc(&mut c, "GetShipConfiguration", &[Value::Int(current)], current).await.unwrap(), expected);
                assert_readable(rpc(&mut c, "ConfigureShip", &[Value::dict([(KEYS[0], Value::Bool(false)), ("unknown", Value::Bool(true))])], current).await);
                let bad_bind = Value::tuple([Value::Int(remote_station), Value::Int(15)]);
                let args = [Value::dict([(KEYS[0], Value::Bool(false))])];
                let call = Call { service: "ship", method: "ConfigureShip", args: &args, kwargs: &Value::None, bound: true, bind_params: Some(&bad_bind) };
                assert_readable(crate::services::dispatch(&mut c, &call).await);
                c.session.apply(vec![("shipid", Value::Int(local))]);
                assert_readable(rpc(&mut c, "ConfigureShip", &args, local).await);
                c.session.apply(vec![("shipid", Value::Int(current))]);
                transfer(&test_db, &c.sde, current, foreign.character).await;
                assert_readable(rpc(&mut c, "ConfigureShip", &args, current).await);
                crate::dao::characters::selection::set_ship(&test_db, foreign.character, current).await.unwrap();
                crate::dao::characters::ship::dock(&test_db, foreign.character, station_id, system).await.unwrap();
                c.login.user_id = foreign.account;
                c.session.apply(vec![("charid", Value::Int(foreign.character))]);
                assert_off(rpc(&mut c, "GetShipConfiguration", &[Value::Int(current)], current).await.unwrap());
                rpc(&mut c, "ConfigureShip", &[Value::dict([(KEYS[3], Value::Bool(true))])], current).await.unwrap();
                assert_settings(rpc(&mut c, "GetShipConfiguration", &[Value::Int(current)], current).await.unwrap(), [false, false, false, true]);
                c.login.user_id = owner.account;
                c.session.apply(vec![("charid", Value::Int(owner.character))]);
                transfer(&test_db, &c.sde, current, owner.character).await;
                assert_off(rpc(&mut c, "GetShipConfiguration", &[Value::Int(current)], current).await.unwrap());
            }
            Scenario::Space => {
                let ego = crate::dao::items::creation::positioned(&test_db, hull, owner.character, system, [0.0; 3]).await.unwrap();
                c.session.apply(vec![("shipid", Value::Int(ego)), ("stationid", Value::None), ("solarsystemid", Value::Int(system))]);
                assert_readable(rpc(&mut c, "GetShipConfiguration", &[Value::Int(ego)], ego).await);
                c.world.system(system).await.unwrap();
                let mut pilot = ship();
                pilot.id = ego;
                pilot.owner_id = owner.character;
                pilot.char_id = Some(owner.character);
                pilot.type_id = hull;
                pilot.cloak = 1;
                let mut target = pilot.clone();
                target.id = invisible;
                target.char_id = None;
                target.cloak = 0;
                c.world.with(system, move |s| { s.insert(pilot); s.insert(target); }).await.unwrap();
                assert_off(rpc(&mut c, "GetShipConfiguration", &[Value::Int(ego)], ego).await.unwrap());
                assert_off(rpc(&mut c, "GetShipConfiguration", &[Value::Int(invisible)], ego).await.unwrap());
                transfer(&test_db, &c.sde, invisible, foreign.character).await;
                grant(&test_db, invisible, foreign.character).await;
                c.world.with(system, move |s| {
                    let mut target = ship();
                    target.id = invisible;
                    target.owner_id = foreign.character;
                    target.type_id = hull;
                    s.insert(target);
                }).await.unwrap();
                assert_settings(rpc(&mut c, "GetShipConfiguration", &[Value::Int(invisible)], ego).await.unwrap(), [false, true, false, false]);
                c.session.apply(vec![("stationid", Value::Int(station_id))]);
                assert_readable(rpc(&mut c, "GetShipConfiguration", &[Value::Int(invisible)], ego).await);
                c.session.apply(vec![("stationid", Value::None)]);
                let endpoint = crate::services::inventory::reachability::Endpoint { location: invisible, flag: infinity_core::consts::FLAG_FLEET_HANGAR };
                let identity = crate::services::identity::Identity::of_client(&c);
                let proof = crate::services::inventory::reachability::Reachability::capture(&identity, &test_db, &c.world, Some(system), &[endpoint]).await.unwrap();
                assert!(proof.check(&test_db, &c.sde, endpoint).await.is_ok());
                c.world.with(system, move |s| {
                    let mut target = ship();
                    target.id = invisible;
                    target.owner_id = foreign.character;
                    target.type_id = hull;
                    target.cloak = 1;
                    s.insert(target);
                }).await.unwrap();
                let proof = crate::services::inventory::reachability::Reachability::capture(&identity, &test_db, &c.world, Some(system), &[endpoint]).await.unwrap();
                assert!(proof.check(&test_db, &c.sde, endpoint).await.is_err());
                assert_readable(rpc(&mut c, "GetShipConfiguration", &[Value::Int(invisible)], ego).await);
                assert_readable(rpc(&mut c, "GetShipConfiguration", &[Value::Int(current)], current).await);
            }
        }
    }).await;
    fixture::cleanup(&db, owner).await.unwrap();
    fixture::cleanup(&db, foreign).await.unwrap();
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn configuration_current_and_local_ship_roundtrip() { exercise(Scenario::Get).await; }

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn configuration_refuses_invisible_foreign_missing_and_nonship() { exercise(Scenario::Refusals).await; }

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn configure_ship_persists_atomically_and_rejects_stale_scope() { exercise(Scenario::Configure).await; }

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn configuration_uses_actor_visibility_in_space() { exercise(Scenario::Space).await; }
