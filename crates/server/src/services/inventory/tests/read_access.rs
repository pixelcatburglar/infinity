//! Bound inventory reads across remote hulls and corporate custody.
use infinity_core::{consts::{CATEGORY_SHIP, FLAG_CARGO, FLAG_CORP_SAG1, FLAG_HANGAR, FLAG_SHIP_HANGAR, GROUP_CARGO_CONTAINER, GROUP_STATION, ROLE_HANGAR_CAN_QUERY_1, ROLE_HANGAR_CAN_TAKE_1}};
use crate::{dao::items::{loading::ItemQuery, transfers}, services::{self, items::load_items, Call, CallError}};
use marshal::{DbRowDescriptor, Value};

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn remote_nested_hulls_and_empty_and_full_containers() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = services::tests::fixture::create(&db).await.unwrap();
    let (mut client, _peer) = services::tests::client(db.clone(), f.character).await;
    let (local, remote, system) = {
        let mut stations = client.sde.map.stations().filter(|s| s.solar_system_id.is_some());
        let local = stations.next().unwrap().station_id;
        let remote_station = stations.find(|s| s.station_id != local).unwrap();
        (local, remote_station.station_id, remote_station.solar_system_id.unwrap())
    };
    let ship_type = client.sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SHIP)).unwrap().type_id;
    let can_type = client.sde.types.iter().find(|t| t.group_id == Some(GROUP_CARGO_CONTAINER)).unwrap().type_id;
    let hull = transfers::insert(&db, ship_type, f.character, remote, FLAG_HANGAR, 1).await.unwrap();
    let nested = transfers::insert(&db, ship_type, f.character, hull, FLAG_SHIP_HANGAR, 1).await.unwrap();
    let can = transfers::insert(&db, can_type, f.character, nested, FLAG_CARGO, 1).await.unwrap();
    db.execute("UPDATE game.items SET singleton=true WHERE item_id=ANY($1)", &[&vec![hull, nested, can]]).await.unwrap();
    client.session.apply(vec![("stationid", Value::Int(local))]);
    let bind = Value::tuple([Value::Int(local), Value::Int(GROUP_STATION)]);
    let kwargs = Value::None;
    let args = [Value::Int(can), Value::Int(remote), Value::Int(system)];
    let call = Call { service: "invbroker", method: "GetContainerContents", args: &args, kwargs: &kwargs, bound: true, bind_params: Some(&bind) };
    assert_eq!(services::dispatch(&mut client, &call).await.unwrap(), Value::list([]));
    let child = transfers::insert(&db, can_type, f.character, can, FLAG_CARGO, 3).await.unwrap();
    let rows = services::dispatch(&mut client, &call).await.unwrap();
    let rows = rows.items().unwrap();
    assert_eq!(rows.len(), 1);
    let Value::DbRow { descriptor, .. } = &rows[0] else { panic!("expected inventory DBRow") };
    assert_eq!(DbRowDescriptor::from_value(descriptor).unwrap().get(&rows[0], "itemID"), Some(Value::Int(child)));
    let wrong = [Value::Int(nested), Value::Int(remote), Value::Int(system + 1)];
    let call = Call { service: "invbroker", method: "GetContainerContents", args: &wrong, kwargs: &kwargs, bound: true, bind_params: Some(&bind) };
    assert!(matches!(services::dispatch(&mut client, &call).await, Err(CallError::User { .. })));
    for location in [i64::MAX, -1] {
        db.execute("UPDATE game.items SET location_id=$2 WHERE item_id=$1", &[&hull, &location]).await.unwrap();
        let args = [Value::Int(can), Value::Int(remote)];
        let call = Call { service: "invbroker", method: "GetContainerContents", args: &args, kwargs: &kwargs, bound: true, bind_params: Some(&bind) };
        assert!(matches!(services::dispatch(&mut client, &call).await, Err(CallError::User { .. })));
    }
    services::tests::fixture::cleanup(&db, f).await.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn corporate_query_not_take_and_revocation_rechecks_personal_descendant() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let actor = services::tests::fixture::create(&db).await.unwrap();
    let ceo = services::tests::fixture::create(&db).await.unwrap();
    let (mut client, _peer) = services::tests::client(db.clone(), actor.character).await;
    let station = client.sde.map.stations().find(|s| s.solar_system_id.is_some()).unwrap().station_id;
    let corp = services::tests::fixture::create_corporation(&db, ceo.character, station).await.unwrap();
    services::tests::fixture::assign_corporation(&db, actor.character, corp, 0).await.unwrap();
    client.session.apply(vec![("stationid", Value::Int(station)), ("corpid", Value::Int(corp))]);
    let ty = client.sde.types.iter().find(|t| t.group_id == Some(GROUP_CARGO_CONTAINER)).unwrap().type_id;
    let office = transfers::insert(&db, ty, corp, station, FLAG_HANGAR, 1).await.unwrap();
    crate::dao::offices::insert(&db, office, corp, station, 1).await.unwrap();
    let root = transfers::insert(&db, ty, corp, office, FLAG_CORP_SAG1, 1).await.unwrap();
    let personal = transfers::insert(&db, ty, actor.character, root, FLAG_CARGO, 1).await.unwrap();
    db.execute("UPDATE game.items SET singleton=true WHERE item_id=ANY($1)", &[&vec![office, root, personal]]).await.unwrap();
    let snapshot = load_items(&db, &client.sde, ItemQuery::Id(personal)).await.unwrap().pop().unwrap().row();
    let bind = Value::tuple([Value::Int(station), Value::Int(GROUP_STATION)]);
    let kwargs = Value::None;
    let args = [Value::Int(personal), Value::Int(station)];
    let validation = [Value::list([snapshot])];
    let call = Call { service: "invbroker", method: "GetContainerContents", args: &args, kwargs: &kwargs, bound: true, bind_params: Some(&bind) };
    let check = Call { service: "invbroker", method: "ValidateItemListCanBeOpened", args: &validation, kwargs: &kwargs, bound: true, bind_params: Some(&bind) };
    for (role, allowed) in [(ROLE_HANGAR_CAN_TAKE_1, false), (ROLE_HANGAR_CAN_QUERY_1, true), (0, false)] {
        db.execute("UPDATE game.characters SET corp_roles_hq=$2 WHERE character_id=$1", &[&actor.character, &role]).await.unwrap();
        let result = services::dispatch(&mut client, &call).await;
        if allowed { assert_eq!(result.unwrap(), Value::list([])); } else { assert!(matches!(result, Err(CallError::User { .. }))); }
        let result = services::dispatch(&mut client, &check).await.unwrap();
        assert_eq!(result, if allowed { Value::list([]) } else { Value::list([Value::Int(personal)]) });
    }
    crate::dao::offices::delete(&db, office).await.unwrap();
    db.execute("DELETE FROM game.items WHERE owner_id=$1", &[&corp]).await.unwrap();
    services::tests::fixture::cleanup_corporation(&db, corp, &[actor.character], 0).await.unwrap();
    services::tests::fixture::cleanup(&db, actor).await.unwrap();
    services::tests::fixture::cleanup(&db, ceo).await.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn foreign_shared_bay_requires_current_grant_visibility_and_range() {
    use infinity_core::consts::{CATEGORY_CHARGE, CATEGORY_MODULE, FLAG_FLEET_HANGAR, FLAG_FITTING_FIRST, GROUP_SOLAR_SYSTEM};
    use infinity_core::consts::dogma::ATTR_FLEET_HANGAR_CAPACITY;
    use crate::dao::items::{combat, ship_configuration::{self, Patch}};
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let actor = services::tests::fixture::create(&db).await.unwrap();
    let owner = services::tests::fixture::create(&db).await.unwrap();
    let (mut client, _peer) = services::tests::client(db.clone(), actor.character).await;
    let station = client.sde.map.stations().find(|s| s.solar_system_id.is_some()).unwrap();
    let system = station.solar_system_id.unwrap();
    let corp = services::tests::fixture::create_corporation(&db, owner.character, station.station_id).await.unwrap();
    for id in [actor.character, owner.character] { services::tests::fixture::assign_corporation(&db, id, corp, 0).await.unwrap(); }
    let hull_type = client.sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SHIP) && t.attribute(ATTR_FLEET_HANGAR_CAPACITY).is_some_and(|v| v > 0.0)).unwrap().type_id;
    let can_type = client.sde.types.iter().find(|t| t.group_id == Some(GROUP_CARGO_CONTAINER)).unwrap().type_id;
    let module_type = client.sde.types.iter().find(|t| t.category_id == Some(CATEGORY_MODULE)).unwrap().type_id;
    let charge_type = client.sde.types.iter().find(|t| t.category_id == Some(CATEGORY_CHARGE)).unwrap().type_id;
    let pilot = crate::dao::items::creation::rookie_ship(&db, hull_type, actor.character, system, FLAG_HANGAR).await.unwrap();
    let host = crate::dao::items::creation::rookie_ship(&db, hull_type, owner.character, system, FLAG_HANGAR).await.unwrap();
    let shared = transfers::insert(&db, can_type, actor.character, host, FLAG_FLEET_HANGAR, 1).await.unwrap();
    db.execute("UPDATE game.items SET singleton=true WHERE item_id=$1", &[&shared]).await.unwrap();
    let private = transfers::insert(&db, module_type, owner.character, host, FLAG_FITTING_FIRST, 1).await.unwrap();
    combat::apply_loads(&db, host, &[(private, FLAG_FITTING_FIRST, charge_type, 2, 0)]).await.unwrap();
    ship_configuration::patch(&db, host, owner.character, &Patch { fleet_hangar_corp: Some(true), ..Default::default() }).await.unwrap();
    client.session.apply(vec![("shipid", Value::Int(pilot)), ("stationid", Value::None), ("solarsystemid", Value::Int(system)), ("corpid", Value::Int(corp))]);
    client.world.system(system).await.unwrap();
    let ball = |id, who, x, character| crate::space::Ball {
        id, type_id: hull_type, owner_id: who, corp_id: corp, alliance_id: 0, char_id: character,
        mode: crate::space::MODE_STOP, radius: 50.0, signature_radius: None, mass: 1.0, max_vel: 0.0, agility: 1.0, speed_fraction: 0.0,
        pos: [x, 0.0, 0.0], vel: [0.0; 3], old_pos: [x, 0.0, 0.0], old_vel: [0.0; 3], segment_acceleration: None,
        goto: [0.0; 3], follow_id: 0, follow_range: 0.0, free: true, global: false, massive: false, interactive: false,
        warp: crate::space::WarpState::default(), last_ms: 0, interpolation_anchor_ms: 0, effect_stamp: 0,
        cloak: 0, cloak_until: 0, health: [1.0; 3], launcher: 0, slim: vec![],
    };
    let pilot_ball = ball(pilot, actor.character, 0.0, Some(actor.character));
    let host_ball = ball(host, owner.character, 2600.0, None);
    client.world.with(system, move |s| { s.insert(pilot_ball); s.insert(host_ball); }).await.unwrap();
    let bind = Value::tuple([Value::Int(system), Value::Int(GROUP_SOLAR_SYSTEM)]);
    let kwargs = Value::None;
    let args = [Value::Int(host), Value::Int(system)];
    let call = Call { service: "invbroker", method: "GetContainerContents", args: &args, kwargs: &kwargs, bound: true, bind_params: Some(&bind) };
    let snapshot = load_items(&db, &client.sde, ItemQuery::Id(shared)).await.unwrap().pop().unwrap().row();
    let validation = [Value::list([snapshot])];
    let check = Call { service: "invbroker", method: "ValidateItemListCanBeOpened", args: &validation, kwargs: &kwargs, bound: true, bind_params: Some(&bind) };
    let result = services::dispatch(&mut client, &call).await.unwrap();
    assert_eq!(result.items().unwrap().len(), 1);
    assert_eq!(services::item_descriptor().get(&result.items().unwrap()[0], "itemID"), Some(Value::Int(shared)));
    assert_eq!(services::dispatch(&mut client, &check).await.unwrap(), Value::list([]));
    let far = ball(host, owner.character, 3000.0, None);
    client.world.with(system, move |s| { s.insert(far); }).await.unwrap();
    assert!(matches!(services::dispatch(&mut client, &call).await, Err(CallError::User { .. })));
    assert_eq!(services::dispatch(&mut client, &check).await.unwrap(), Value::list([Value::Int(shared)]));
    let mut hidden = ball(host, owner.character, 2600.0, None); hidden.cloak = 1;
    client.world.with(system, move |s| { s.insert(hidden); }).await.unwrap();
    assert!(matches!(services::dispatch(&mut client, &call).await, Err(CallError::User { .. })));
    let near = ball(host, owner.character, 2600.0, None);
    client.world.with(system, move |s| { s.insert(near); }).await.unwrap();
    ship_configuration::patch(&db, host, owner.character, &Patch { fleet_hangar_corp: Some(false), ..Default::default() }).await.unwrap();
    assert!(matches!(services::dispatch(&mut client, &call).await, Err(CallError::User { .. })));
    assert_eq!(services::dispatch(&mut client, &check).await.unwrap(), Value::list([Value::Int(shared)]));
    db.execute("DELETE FROM game.module_charges WHERE ship_id=$1", &[&host]).await.unwrap();
    services::tests::fixture::cleanup_corporation(&db, corp, &[actor.character, owner.character], 0).await.unwrap();
    services::tests::fixture::cleanup(&db, actor).await.unwrap();
    services::tests::fixture::cleanup(&db, owner).await.unwrap();
}
