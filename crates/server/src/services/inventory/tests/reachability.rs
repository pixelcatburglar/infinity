use super::{inventory, load_items, move_fingerprint, move_item, Call, CallError, Container, ItemQuery, Value, CATEGORY_SHIP, CONTAINER_HANGAR, FLAG_CARGO, FLAG_CORP_SAG1, FLAG_HANGAR, INVENTORY, ROLE_DIRECTOR, ROLE_HANGAR_CAN_TAKE_1};
use crate::services::tests::fixture;

type Notifications = tokio::sync::mpsc::UnboundedReceiver<(String, Vec<Value>)>;

fn spy_notifications(client: &mut crate::client::Client) -> Notifications {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    client.notify_tx = tx;
    rx
}

fn clear_notifications(client: &mut crate::client::Client, rx: &mut Notifications) {
    client.outbox.clear();
    while rx.try_recv().is_ok() {}
}

fn assert_no_notifications(client: &mut crate::client::Client, rx: &mut Notifications) {
    let outbox_empty = client.outbox.is_empty();
    let mut receiver_empty = true;
    while rx.try_recv().is_ok() { receiver_empty = false; }
    assert!(outbox_empty && receiver_empty, "unexpected notification");
}

fn has_item_change(client: &mut crate::client::Client, rx: &mut Notifications) -> bool {
    let mut found = false;
    while let Ok((name, _)) = rx.try_recv() {
        found |= name == "OnItemChange";
    }
    found |= client.outbox.drain(..).any(|packet| matches!(&packet.destination, crate::macho::Address::Broadcast { broadcast_id, .. } if broadcast_id.as_text().as_deref() == Some("OnItemChange")));
    found
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated database with SDE"]
async fn station_dispatch_rejects_remote_moves_and_merges() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let actor = fixture::create(&db).await.unwrap();
    let (mut client, _peer) = crate::services::tests::client(db.clone(), actor.character).await;
    let mut notifications = spy_notifications(&mut client);
    let mut stations = client.sde.map.stations();
    let station = stations.next().unwrap().station_id;
    let remote = stations.find(|s| s.station_id != station).unwrap().station_id;
    drop(stations);
    let type_id = client.sde.types.iter().find(|t| t.category_id == Some(infinity_core::consts::CATEGORY_CHARGE) && t.published == Some(true)).unwrap().type_id;
    let ship_type = client.sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SHIP)).unwrap().type_id;
    client.session.apply(vec![("stationid", Value::Int(station)), ("corpid", Value::Int(1))]);
    let insert = |location, flag, quantity| {
        let db = &db;
        async move { crate::dao::items::transfers::insert(db, type_id, actor.character, location, flag, quantity).await.unwrap() }
    };
    let local_a = insert(station, FLAG_HANGAR, 10).await;
    let local_b = insert(station, FLAG_HANGAR, 10).await;
    let remote_a = insert(remote, FLAG_HANGAR, 10).await;
    let remote_b = insert(remote, FLAG_HANGAR, 10).await;
    let local_ship = crate::dao::items::transfers::insert(&db, ship_type, actor.character, station, FLAG_HANGAR, 1).await.unwrap();
    let remote_ship = crate::dao::items::transfers::insert(&db, ship_type, actor.character, remote, FLAG_HANGAR, 1).await.unwrap();
    db.execute("UPDATE game.items SET singleton = true WHERE item_id = ANY($1)", &[&&[local_ship, remote_ship][..]]).await.unwrap();
    let local_cargo = insert(local_ship, FLAG_CARGO, 10).await;
    let local_cargo2 = insert(local_ship, FLAG_CARGO, 5).await;
    let remote_cargo = insert(remote_ship, FLAG_CARGO, 10).await;
    let local_bind = Value::tuple([Value::Int(station), Value::Int(CONTAINER_HANGAR), Value::Int(actor.character)]);
    let remote_bind = Value::tuple([Value::Int(remote), Value::Int(CONTAINER_HANGAR), Value::Int(actor.character)]);
    let remote_ship_bind = Value::tuple([Value::Int(remote_ship)]);
    let kwargs = Value::Dict(vec![]);
    let cases = [
        ("Add", &local_bind, vec![Value::Int(remote_a), Value::Int(remote)]),
        ("Add", &remote_bind, vec![Value::Int(local_a), Value::Int(station)]),
        ("MultiAdd", &local_bind, vec![Value::list([Value::Int(remote_a)]), Value::Int(remote)]),
        ("MultiAdd", &remote_ship_bind, vec![Value::list([Value::Int(local_a)]), Value::Int(station)]),
        ("Add", &local_bind, vec![Value::Int(remote_cargo), Value::Int(remote_ship)]),
        ("MultiMerge", &remote_bind, vec![Value::list([Value::tuple([Value::Int(remote_a), Value::Int(remote_b), Value::Int(1)])]), Value::Int(remote)]),
        ("MultiMerge", &local_bind, vec![Value::list([Value::tuple([Value::Int(remote_a), Value::Int(local_b), Value::Int(1)])]), Value::Int(remote)]),
        ("MultiMerge", &local_bind, vec![Value::list([Value::tuple([Value::Int(local_a), Value::Int(local_b), Value::Int(1)])]), Value::Int(remote)]),
        ("MultiMerge", &remote_bind, vec![Value::list([Value::tuple([Value::Int(local_a), Value::Int(local_b), Value::Int(1)])])]),
        ("MultiMerge", &local_bind, vec![Value::list([Value::tuple([Value::Int(local_a), Value::Int(local_b), Value::Int(1)]), Value::tuple([Value::Int(remote_a), Value::Int(local_b), Value::Int(1)])])]),
        ("StackAll", &remote_bind, vec![]),
        ("StackAll", &remote_ship_bind, vec![]),
    ];
    for (method, bind, args) in cases {
        let before = move_fingerprint(&db).await;
        clear_notifications(&mut client, &mut notifications);
        let call = Call { service: INVENTORY, method, args: &args, kwargs: &kwargs, bound: true, bind_params: Some(bind) };
        assert!(inventory(&mut client, &call).await.is_err(), "{method} unexpectedly succeeded");
        assert_eq!(move_fingerprint(&db).await, before, "{method} changed rows");
        assert_no_notifications(&mut client, &mut notifications);
    }
    let args = [Value::Int(local_cargo), Value::Int(local_ship)];
    let call = Call { service: INVENTORY, method: "Add", args: &args, kwargs: &kwargs, bound: true, bind_params: Some(&local_bind) };
    assert!(inventory(&mut client, &call).await.is_ok());
    let row = load_items(&db, &client.sde, ItemQuery::Id(local_cargo)).await.unwrap().pop().unwrap();
    assert_eq!(row.location_id, station);
    assert!(has_item_change(&mut client, &mut notifications));
    clear_notifications(&mut client, &mut notifications);
    let args = [Value::list([Value::Int(local_cargo2)]), Value::Int(local_ship)];
    let call = Call { service: INVENTORY, method: "MultiAdd", args: &args, kwargs: &kwargs, bound: true, bind_params: Some(&local_bind) };
    assert!(inventory(&mut client, &call).await.is_ok());
    let row = load_items(&db, &client.sde, ItemQuery::Id(local_cargo2)).await.unwrap().pop().unwrap();
    assert_eq!(row.location_id, station);
    assert!(has_item_change(&mut client, &mut notifications));
    let merge = [Value::list([Value::tuple([Value::Int(local_a), Value::Int(local_b), Value::Int(1)])])];
    let call = Call { service: INVENTORY, method: "MultiMerge", args: &merge, kwargs: &kwargs, bound: true, bind_params: Some(&local_bind) };
    assert!(inventory(&mut client, &call).await.is_ok());
    assert!(has_item_change(&mut client, &mut notifications));
    let row = load_items(&db, &client.sde, ItemQuery::Id(local_b)).await.unwrap().pop().unwrap();
    assert_eq!(row.quantity, 11);
    clear_notifications(&mut client, &mut notifications);
    let call = Call { service: INVENTORY, method: "StackAll", args: &[], kwargs: &kwargs, bound: true, bind_params: Some(&local_bind) };
    assert!(inventory(&mut client, &call).await.is_ok());
    let stacks = load_items(&db, &client.sde, ItemQuery::Ids(&[local_a, local_b, local_cargo, local_cargo2])).await.unwrap();
    assert_eq!(stacks.len(), 1);
    assert_eq!(stacks[0].quantity, 35);
    assert!(has_item_change(&mut client, &mut notifications));
    fixture::cleanup(&db, actor).await.unwrap();
    assert_eq!(fixture::remaining(&db, actor).await.unwrap(), 0);
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated database with SDE"]
async fn ancestry_rechecks_roots_and_rejects_cycles_and_missing_parents() {
    use super::super::reachability::{Endpoint, Reachability};
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let actor = fixture::create(&db).await.unwrap();
    let (mut client, _peer) = crate::services::tests::client(db.clone(), actor.character).await;
    let mut stations = client.sde.map.stations();
    let station = stations.next().unwrap().station_id;
    let remote = stations.find(|s| s.station_id != station).unwrap().station_id;
    drop(stations);
    client.session.apply(vec![("stationid", Value::Int(station))]);
    let ty = client.sde.types.iter().find(|t| t.group_id == Some(infinity_core::consts::GROUP_CARGO_CONTAINER)).unwrap().type_id;
    let root = crate::dao::items::transfers::insert(&db, ty, actor.character, station, FLAG_HANGAR, 1).await.unwrap();
    let nested = crate::dao::items::transfers::insert(&db, ty, actor.character, root, FLAG_CARGO, 1).await.unwrap();
    let alternate = crate::dao::items::transfers::insert(&db, ty, actor.character, station, FLAG_HANGAR, 1).await.unwrap();
    db.execute("UPDATE game.items SET singleton = true WHERE item_id = ANY($1)", &[&&[root, nested, alternate][..]]).await.unwrap();
    let endpoint = Endpoint { location: nested, flag: FLAG_CARGO };
    let reach = Reachability::capture(&crate::services::identity::Identity::of_client(&client), &client.db, &client.world, client.session.int("solarsystemid"), &[endpoint]).await.unwrap();
    assert!(reach.check(&db, &client.sde, endpoint).await.is_ok());
    for (column, value) in [("owner_id", actor.character + 1), ("flag_id", i64::from(FLAG_HANGAR)), ("location_id", alternate)] {
        let original: i64 = db.query_one(&format!("SELECT {column}::bigint AS value FROM game.items WHERE item_id = $1"), &[&nested]).await.unwrap().get("value");
        db.execute(&format!("UPDATE game.items SET {column} = $2::bigint WHERE item_id = $1"), &[&nested, &value]).await.unwrap();
        let before = move_fingerprint(&db).await;
        assert!(reach.check(&db, &client.sde, endpoint).await.is_err());
        assert_eq!(move_fingerprint(&db).await, before);
        assert!(client.outbox.is_empty());
        db.execute(&format!("UPDATE game.items SET {column} = $2::bigint WHERE item_id = $1"), &[&nested, &original]).await.unwrap();
    }
    db.execute("UPDATE game.items SET singleton = false WHERE item_id = $1", &[&root]).await.unwrap();
    assert!(reach.check(&db, &client.sde, endpoint).await.is_err());
    db.execute("UPDATE game.items SET singleton = true WHERE item_id = $1", &[&root]).await.unwrap();
    db.execute("UPDATE game.items SET location_id = $2 WHERE item_id = $1", &[&nested, &alternate]).await.unwrap();
    let recaptured = Reachability::capture(&crate::services::identity::Identity::of_client(&client), &client.db, &client.world, client.session.int("solarsystemid"), &[endpoint]).await.unwrap();
    assert!(recaptured.check(&db, &client.sde, endpoint).await.is_ok());
    db.execute("UPDATE game.items SET location_id = $2 WHERE item_id = $1", &[&nested, &root]).await.unwrap();
    for location in [remote, nested, -999_i64] {
        db.execute("UPDATE game.items SET location_id = $2 WHERE item_id = $1", &[&root, &location]).await.unwrap();
        let before = move_fingerprint(&db).await;
        let reach = reach.clone();
        let sde = client.sde.clone();
        let result: Result<(), CallError> = db.transaction(move |tx, _: &mut ()| {
            let reach = reach.clone();
            let sde = sde.clone();
            Box::pin(async move { reach.check(tx, &sde, endpoint).await })
        }, |(), ()| async move { Ok(()) }).await;
        assert!(result.is_err());
        assert_eq!(move_fingerprint(&db).await, before);
        assert!(client.outbox.is_empty());
    }
    fixture::cleanup(&db, actor).await.unwrap();
    assert_eq!(fixture::remaining(&db, actor).await.unwrap(), 0);
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated database with SDE"]
async fn space_surface_range_and_ship_bays_use_the_authoritative_root() {
    use super::super::reachability::{Endpoint, Reachability};
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let actor = fixture::create(&db).await.unwrap();
    let guest = fixture::create(&db).await.unwrap();
    let (mut client, _peer) = crate::services::tests::client(db.clone(), actor.character).await;
    let mut notifications = spy_notifications(&mut client);
    let system = -101_i64;
    let ship_type = client.sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SHIP)).unwrap().type_id;
    let maintenance_type = client.sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SHIP) && t.attribute(908).is_some_and(|v| v.is_finite() && v > 0.0)).unwrap().type_id;
    let fleet_type = client.sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SHIP) && t.attribute(912).is_some_and(|v| v.is_finite() && v > 0.0)).unwrap().type_id;
    let no_bay_type = client.sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SHIP) && !t.attribute(908).is_some_and(|v| v.is_finite() && v > 0.0) && !t.attribute(912).is_some_and(|v| v.is_finite() && v > 0.0)).unwrap().type_id;
    let can_type = client.sde.types.iter().find(|t| t.group_id == Some(infinity_core::consts::GROUP_CARGO_CONTAINER) && t.attribute(715).is_none()).unwrap().type_id;
    let ship = crate::dao::items::transfers::insert(&db, ship_type, actor.character, system, FLAG_HANGAR, 1).await.unwrap();
    let other_ship = crate::dao::items::transfers::insert(&db, maintenance_type, actor.character, system, FLAG_HANGAR, 1).await.unwrap();
    let fleet_ship = crate::dao::items::transfers::insert(&db, fleet_type, actor.character, system, FLAG_HANGAR, 1).await.unwrap();
    let no_bay_ship = crate::dao::items::transfers::insert(&db, no_bay_type, actor.character, system, FLAG_HANGAR, 1).await.unwrap();
    let can = crate::dao::items::transfers::insert(&db, can_type, actor.character, system, FLAG_HANGAR, 1).await.unwrap();
    let nested = crate::dao::items::transfers::insert(&db, can_type, actor.character, can, FLAG_CARGO, 1).await.unwrap();
    let override_type = client.sde.types.iter().find(|t| {
        t.group_id == Some(infinity_core::consts::GROUP_CARGO_CONTAINER)
            && t.attribute(715).is_some_and(|distance| distance.is_finite() && distance > 0.0 && distance != 2500.0)
    }).expect("SDE must contain a cargo container with a non-default operational distance");
    let override_distance = override_type.attribute(715).unwrap();
    let override_can = crate::dao::items::transfers::insert(&db, override_type.type_id, actor.character, system, FLAG_HANGAR, 1).await.unwrap();
    let uncaptured_can = crate::dao::items::transfers::insert(&db, can_type, actor.character, system, FLAG_HANGAR, 1).await.unwrap();
    let missing_ball = crate::dao::items::transfers::insert(&db, can_type, actor.character, system, FLAG_HANGAR, 1).await.unwrap();
    db.execute("UPDATE game.items SET singleton = true WHERE item_id = ANY($1)", &[&&[ship, other_ship, fleet_ship, no_bay_ship, can, nested, override_can, uncaptured_can, missing_ball][..]]).await.unwrap();
    client.session.apply(vec![("shipid", Value::Int(ship)), ("solarsystemid", Value::Int(system))]);
    let active_endpoint = Endpoint { location: ship, flag: FLAG_CARGO };
    let absent_actor = Reachability::capture(&crate::services::identity::Identity::of_client(&client), &client.db, &client.world, client.session.int("solarsystemid"), &[active_endpoint]).await.unwrap();
    assert!(absent_actor.check(&db, &client.sde, active_endpoint).await.is_err());
    client.world.system(system).await.unwrap();
    let absent_pilot = Reachability::capture(&crate::services::identity::Identity::of_client(&client), &client.db, &client.world, client.session.int("solarsystemid"), &[active_endpoint]).await.unwrap();
    assert!(absent_pilot.check(&db, &client.sde, active_endpoint).await.is_err());
    let ball = |id, x| crate::space::Ball {
        id, type_id: 1, owner_id: actor.character, corp_id: 1, alliance_id: 0, char_id: (id == ship).then_some(actor.character),
        mode: crate::space::MODE_STOP, radius: 50.0, signature_radius: None, mass: 1.0, max_vel: 0.0, agility: 1.0, speed_fraction: 0.0,
        pos: [x, 0.0, 0.0], vel: [0.0; 3], old_pos: [x, 0.0, 0.0], old_vel: [0.0; 3], segment_acceleration: None,
        goto: [0.0; 3], follow_id: 0, follow_range: 0.0, free: true, global: false, massive: false, interactive: false,
        warp: crate::space::WarpState::default(), last_ms: 0, interpolation_anchor_ms: 0, effect_stamp: 0,
        cloak: 0, cloak_until: 0, health: [1.0; 3], launcher: 0, slim: vec![],
    };
    let mut unpiloted = ball(ship, 0.0);
    unpiloted.char_id = None;
    client.world.with(system, move |s| { s.insert(unpiloted); }).await.unwrap();
    let absent_index = Reachability::capture(&crate::services::identity::Identity::of_client(&client), &client.db, &client.world, client.session.int("solarsystemid"), &[active_endpoint]).await.unwrap();
    assert!(absent_index.check(&db, &client.sde, active_endpoint).await.is_err());
    let (pilot, root, inactive, fleet, no_bay) = (ball(ship, 0.0), ball(can, 2600.0), ball(other_ship, 2600.0), ball(fleet_ship, 2600.0), ball(no_bay_ship, 2600.0));
    client.world.with(system, move |s| { s.insert(pilot); s.insert(root); s.insert(inactive); s.insert(fleet); s.insert(no_bay); }).await.unwrap();
    let endpoints = [
        Endpoint { location: ship, flag: FLAG_CARGO },
        Endpoint { location: nested, flag: FLAG_CARGO },
        Endpoint { location: other_ship, flag: FLAG_CARGO },
        Endpoint { location: other_ship, flag: 90 },
        Endpoint { location: fleet_ship, flag: 155 },
        Endpoint { location: no_bay_ship, flag: 90 },
        Endpoint { location: no_bay_ship, flag: 155 },
    ];
    let reach = Reachability::capture(&crate::services::identity::Identity::of_client(&client), &client.db, &client.world, client.session.int("solarsystemid"), &endpoints).await.unwrap();
    for (endpoint, allowed) in endpoints.into_iter().zip([true, true, false, true, true, false, false]) {
        assert_eq!(reach.check(&db, &client.sde, endpoint).await.is_ok(), allowed);
    }
    db.execute("UPDATE game.items SET singleton = false WHERE item_id = $1", &[&can]).await.unwrap();
    let packaged = Reachability::capture(&crate::services::identity::Identity::of_client(&client), &client.db, &client.world, client.session.int("solarsystemid"), &[endpoints[1]]).await.unwrap();
    assert!(packaged.check(&db, &client.sde, endpoints[1]).await.is_err());
    db.execute("UPDATE game.items SET singleton = true WHERE item_id = $1", &[&can]).await.unwrap();
    let wrong_type = client.sde.types.iter().find(|t| t.group_id.is_some_and(|g| [329, 330, 346].contains(&g))).expect("SDE must contain a formerly misclassified group").type_id;
    let wrong = crate::dao::items::transfers::insert(&db, wrong_type, actor.character, system, FLAG_HANGAR, 1).await.unwrap();
    db.execute("UPDATE game.items SET singleton = true WHERE item_id = $1", &[&wrong]).await.unwrap();
    let wrong_ball = ball(wrong, 0.0);
    client.world.with(system, move |s| { s.insert(wrong_ball); }).await.unwrap();
    let wrong_endpoint = Endpoint { location: wrong, flag: FLAG_CARGO };
    let wrong_reach = Reachability::capture(&crate::services::identity::Identity::of_client(&client), &client.db, &client.world, client.session.int("solarsystemid"), &[wrong_endpoint]).await.unwrap();
    assert!(wrong_reach.check(&db, &client.sde, wrong_endpoint).await.is_err());
    let commodity = client.sde.types.iter().find(|t| t.category_id == Some(infinity_core::consts::CATEGORY_CHARGE) && t.published == Some(true)).unwrap().type_id;
    let source = crate::dao::items::transfers::insert(&db, commodity, actor.character, nested, FLAG_CARGO, 10).await.unwrap();
    let source2 = crate::dao::items::transfers::insert(&db, commodity, actor.character, nested, FLAG_CARGO, 10).await.unwrap();
    let bare_source = crate::dao::items::transfers::insert(&db, commodity, actor.character, can, FLAG_CARGO, 10).await.unwrap();
    let aboard = crate::dao::items::transfers::insert(&db, commodity, actor.character, ship, FLAG_CARGO, 10).await.unwrap();
    let inactive_cargo = crate::dao::items::transfers::insert(&db, commodity, actor.character, other_ship, FLAG_CARGO, 1).await.unwrap();
    let maintenance_cargo = crate::dao::items::transfers::insert(&db, commodity, actor.character, other_ship, 90, 1).await.unwrap();
    let fleet_cargo = crate::dao::items::transfers::insert(&db, commodity, actor.character, fleet_ship, 155, 1).await.unwrap();
    let far = ball(can, 2601.0);
    client.world.with(system, move |s| { s.insert(far); }).await.unwrap();
    for (destination, id) in [(Container::Item(ship), source), (Container::Item(ship), bare_source), (Container::Item(nested), aboard), (Container::Item(ship), inactive_cargo)] {
        let before = move_fingerprint(&db).await;
        clear_notifications(&mut client, &mut notifications);
        assert!(move_item(&crate::services::identity::Identity::of_client(&client), &client.db, &client.world, &client.sde, client.session.int("solarsystemid"), client.notify_tx.clone(), &destination, id, None, None, None, |n| crate::services::effects::online::dispatch_notification(n, &client.world, &client.notify_tx, client.session.int("charid"))).await.is_err());
        assert_eq!(move_fingerprint(&db).await, before);
        assert_no_notifications(&mut client, &mut notifications);
    }
    let bind = Value::tuple([Value::Int(nested)]);
    let kwargs = Value::Dict(vec![]);
    let ship_bind = Value::tuple([Value::Int(ship)]);
    let distant_add = [Value::Int(source), Value::Int(nested)];
    let before = move_fingerprint(&db).await;
    clear_notifications(&mut client, &mut notifications);
    let call = Call { service: INVENTORY, method: "Add", args: &distant_add, kwargs: &kwargs, bound: true, bind_params: Some(&ship_bind) };
    assert!(inventory(&mut client, &call).await.is_err());
    assert_eq!(move_fingerprint(&db).await, before);
    assert_no_notifications(&mut client, &mut notifications);
    let distant_multi = [Value::list([Value::Int(bare_source)]), Value::Int(can)];
    let before = move_fingerprint(&db).await;
    clear_notifications(&mut client, &mut notifications);
    let call = Call { service: INVENTORY, method: "MultiAdd", args: &distant_multi, kwargs: &kwargs, bound: true, bind_params: Some(&ship_bind) };
    assert!(inventory(&mut client, &call).await.is_err());
    assert_eq!(move_fingerprint(&db).await, before);
    assert_no_notifications(&mut client, &mut notifications);
    let merge = [Value::list([Value::tuple([Value::Int(source), Value::Int(source2), Value::Int(1)])])];
    for (method, args) in [("MultiMerge", &merge[..]), ("StackAll", &[][..])] {
        let before = move_fingerprint(&db).await;
        clear_notifications(&mut client, &mut notifications);
        let call = Call { service: INVENTORY, method, args, kwargs: &kwargs, bound: true, bind_params: Some(&bind) };
        assert!(inventory(&mut client, &call).await.is_err());
        assert_eq!(move_fingerprint(&db).await, before);
        assert_no_notifications(&mut client, &mut notifications);
    }
    let near = ball(can, 2600.0);
    client.world.with(system, move |s| { s.insert(near); }).await.unwrap();
    let call = Call { service: INVENTORY, method: "MultiMerge", args: &merge, kwargs: &kwargs, bound: true, bind_params: Some(&bind) };
    assert!(inventory(&mut client, &call).await.is_ok());
    assert!(has_item_change(&mut client, &mut notifications));
    let call = Call { service: INVENTORY, method: "Add", args: &distant_add, kwargs: &kwargs, bound: true, bind_params: Some(&ship_bind) };
    assert!(inventory(&mut client, &call).await.is_ok());
    assert!(has_item_change(&mut client, &mut notifications));
    let multi = [Value::list([Value::Int(source2)]), Value::Int(nested)];
    let call = Call { service: INVENTORY, method: "MultiAdd", args: &multi, kwargs: &kwargs, bound: true, bind_params: Some(&ship_bind) };
    assert!(inventory(&mut client, &call).await.is_ok());
    assert!(has_item_change(&mut client, &mut notifications));
    for id in [maintenance_cargo, fleet_cargo] {
        assert!(move_item(&crate::services::identity::Identity::of_client(&client), &client.db, &client.world, &client.sde, client.session.int("solarsystemid"), client.notify_tx.clone(), &Container::Item(ship), id, None, None, None, |n| crate::services::effects::online::dispatch_notification(n, &client.world, &client.notify_tx, client.session.int("charid"))).await.is_ok());
        assert!(has_item_change(&mut client, &mut notifications));
    }
    for id in [source, source2, maintenance_cargo, fleet_cargo] {
        let moved = load_items(&db, &client.sde, ItemQuery::Id(id)).await.unwrap().pop().unwrap();
        assert_eq!(moved.location_id, ship);
    }
    let guest_ship = crate::dao::items::creation::rookie_ship(&db, ship_type, guest.character, system, FLAG_HANGAR).await.unwrap();
    let mut guest_ball = ball(guest_ship, 2600.0);
    guest_ball.owner_id = guest.character;
    guest_ball.char_id = Some(guest.character);
    client.world.with(system, move |s| { s.insert(guest_ball); }).await.unwrap();
    let (guest_sender, mut guest_notices) = tokio::sync::mpsc::unbounded_channel();
    client.world.register_pilot(guest.character, guest_sender);
    crate::dao::items::ship_configuration::patch(&db, fleet_ship, actor.character,
        &crate::dao::items::ship_configuration::Patch { fleet_hangar_corp: Some(true), ..Default::default() }).await.unwrap();
    // The host can take guest property, including guest descendants of an
    // already host-owned root, through the real bound Add dispatch.
    let guest_stack = crate::dao::items::transfers::insert(&db, commodity, guest.character, fleet_ship, infinity_core::consts::FLAG_FLEET_HANGAR, 2).await.unwrap();
    let root = crate::dao::items::transfers::insert(&db, can_type, actor.character, fleet_ship, infinity_core::consts::FLAG_FLEET_HANGAR, 1).await.unwrap();
    let root_row = load_items(&db, &client.sde, ItemQuery::Id(root)).await.unwrap().pop().unwrap();
    assert!(crate::dao::items::inventory::move_item(&db, &root_row, fleet_ship, infinity_core::consts::FLAG_FLEET_HANGAR, actor.character, true).await.unwrap());
    let child = crate::dao::items::transfers::insert(&db, commodity, guest.character, root, FLAG_CARGO, 1).await.unwrap();
    assert!(crate::dao::items::dogma::set_override(&db, ship, infinity_core::consts::dogma::ATTR_CAPACITY, 1_000_000.0).await.unwrap());
    let observed = load_items(&db, &client.sde, ItemQuery::Ids(&[aboard, guest_stack, root])).await.unwrap();
    let input = vec![observed.iter().find(|item| item.item_id == aboard).unwrap().clone(),
        observed.iter().find(|item| item.item_id == root).unwrap().clone(),
        observed.iter().find(|item| item.item_id == guest_stack).unwrap().clone()];
    let source_endpoint = Endpoint { location: fleet_ship, flag: infinity_core::consts::FLAG_FLEET_HANGAR };
    let destination_endpoint = Endpoint { location: ship, flag: FLAG_CARGO };
    let identity = crate::services::identity::Identity::of_client(&client);
    let batch_reach = Reachability::capture(&identity, &db, &client.world, Some(system), &[source_endpoint, destination_endpoint]).await.unwrap();
    let results = super::super::movement::authorize_moves(&db, &client.sde, &identity, &batch_reach, &input,
        None, &Container::Item(ship), Some(FLAG_CARGO)).await.unwrap().unwrap();
    assert_eq!(results.len(), 3);
    assert_eq!(results[0].1, super::super::destination::Custody::Preserve);
    assert_eq!(results[1].1, super::super::destination::Custody::TakeSubtree);
    assert_eq!(results[2].1, super::super::destination::Custody::TakeSubtree);
    assert!(results.iter().all(|(destination, _)| destination.item_owner == actor.character));
    let mut stale = input.clone();
    stale[2].quantity += 1;
    assert!(super::super::movement::authorize_moves(&db, &client.sde, &identity, &batch_reach, &stale,
        None, &Container::Item(ship), Some(FLAG_CARGO)).await.unwrap().is_none());
    for id in [guest_stack, root] {
        let args = [Value::Int(id), Value::Int(fleet_ship)];
        let call = Call { service: INVENTORY, method: "Add", args: &args, kwargs: &kwargs, bound: true, bind_params: Some(&ship_bind) };
        assert!(inventory(&mut client, &call).await.is_ok());
    }
    let mut guest_removal = false;
    while let Ok((event, args)) = guest_notices.try_recv() {
        if event == "OnItemChange" && args.first().is_some_and(|row|
            crate::services::item_descriptor().get(row, "itemID") == Some(Value::Int(guest_stack))) {
            assert_eq!(crate::services::item_descriptor().get(&args[0], "locationID"), Some(Value::Int(infinity_core::consts::LOCATION_JUNKYARD)));
            assert!(!guest_removal, "source-only viewer must be deduplicated");
            guest_removal = true;
        }
    }
    let deposit = crate::dao::items::transfers::insert(&db, commodity, actor.character, ship, FLAG_CARGO, 1).await.unwrap();
    let fleet_bind = Value::tuple([Value::Int(fleet_ship)]);
    let fleet_kwargs = Value::Dict(vec![(Value::str("flag"), Value::Int(i64::from(infinity_core::consts::FLAG_FLEET_HANGAR)))]);
    let deposit_args = [Value::Int(deposit), Value::Int(ship)];
    let deposit_call = Call { service: INVENTORY, method: "Add", args: &deposit_args, kwargs: &fleet_kwargs, bound: true, bind_params: Some(&fleet_bind) };
    inventory(&mut client, &deposit_call).await.unwrap();
    let mut guest_destination = false;
    while let Ok((event, args)) = guest_notices.try_recv() {
        if event == "OnItemChange" && args.first().is_some_and(|row|
            crate::services::item_descriptor().get(row, "itemID") == Some(Value::Int(deposit))) {
            assert_eq!(crate::services::item_descriptor().get(&args[0], "locationID"), Some(Value::Int(fleet_ship)));
            assert!(!guest_destination, "destination viewer must be deduplicated");
            guest_destination = true;
        }
    }
    assert!(guest_destination);
    crate::dao::items::ship_configuration::patch(&db, fleet_ship, actor.character,
        &crate::dao::items::ship_configuration::Patch { fleet_hangar_corp: Some(false), ..Default::default() }).await.unwrap();
    let revoked = crate::dao::items::transfers::insert(&db, commodity, actor.character, ship, FLAG_CARGO, 1).await.unwrap();
    let revoked_args = [Value::Int(revoked), Value::Int(ship)];
    let revoked_call = Call { service: INVENTORY, method: "Add", args: &revoked_args, kwargs: &fleet_kwargs, bound: true, bind_params: Some(&fleet_bind) };
    inventory(&mut client, &revoked_call).await.unwrap();
    assert!(guest_notices.try_recv().is_err(), "revoked viewer received a bay change");
    crate::dao::items::inventory::delete(&db, deposit).await.unwrap();
    crate::dao::items::inventory::delete(&db, revoked).await.unwrap();
    assert!(guest_removal, "source-only viewer must receive removal, not the private destination");
    let transferred = load_items(&db, &client.sde, ItemQuery::Ids(&[guest_stack, root, child])).await.unwrap();
    assert_eq!(transferred.len(), 3);
    assert!(transferred.iter().all(|item| item.owner_id == actor.character));
    assert_eq!(transferred.iter().find(|item| item.item_id == child).unwrap().location_id, root);
    let rejected = crate::dao::items::transfers::insert(&db, commodity, actor.character, ship, FLAG_CARGO, 1).await.unwrap();
    assert!(crate::dao::items::dogma::set_override(&db, fleet_ship, infinity_core::consts::dogma::ATTR_FLEET_HANGAR_CAPACITY, 0.0).await.unwrap());
    clear_notifications(&mut client, &mut notifications);
    let rejected_args = [Value::Int(rejected), Value::Int(ship)];
    let rejected_call = Call { service: INVENTORY, method: "Add", args: &rejected_args, kwargs: &fleet_kwargs, bound: true, bind_params: Some(&fleet_bind) };
    assert!(inventory(&mut client, &rejected_call).await.is_err());
    let unchanged = load_items(&db, &client.sde, ItemQuery::Id(rejected)).await.unwrap().pop().unwrap();
    assert_eq!((unchanged.owner_id, unchanged.location_id, unchanged.quantity), (actor.character, ship, 1));
    assert_no_notifications(&mut client, &mut notifications);
    assert!(guest_notices.try_recv().is_err());
    assert!(crate::dao::items::dogma::set_override(&db, fleet_ship, infinity_core::consts::dogma::ATTR_FLEET_HANGAR_CAPACITY, 1_000_000.0).await.unwrap());
    let split_source = crate::dao::items::transfers::insert(&db, commodity, guest.character, fleet_ship, infinity_core::consts::FLAG_FLEET_HANGAR, 4).await.unwrap();
    let actor_stack = crate::dao::items::transfers::insert(&db, commodity, actor.character, fleet_ship, infinity_core::consts::FLAG_FLEET_HANGAR, 2).await.unwrap();
    let split_args = [Value::Int(fleet_ship), Value::Int(split_source), Value::Int(1), Value::Int(guest.character)];
    let split_call = Call { service: "invbroker", method: "SplitStack", args: &split_args, kwargs: &kwargs, bound: true, bind_params: None };
    let split_id = super::super::inv_broker::broker(&mut client, &split_call).await.unwrap().unwrap().as_int().unwrap();
    let split = load_items(&db, &client.sde, ItemQuery::Id(split_id)).await.unwrap().pop().unwrap();
    assert_eq!(split.owner_id, guest.character);
    let mut previous_owner_quantity = false;
    while let Ok((event, args)) = guest_notices.try_recv() {
        if event == "OnItemChange" && args.first().is_some_and(|row|
            crate::services::item_descriptor().get(row, "itemID") == Some(Value::Int(split_source))) {
            assert_eq!(crate::services::item_descriptor().get(&args[0], "locationID"), Some(Value::Int(fleet_ship)));
            assert_eq!(crate::services::item_descriptor().get(&args[0], "quantity"), Some(Value::Int(3)));
            assert!(!previous_owner_quantity);
            previous_owner_quantity = true;
        }
    }
    assert!(previous_owner_quantity, "original owner must receive the changed source quantity even after access revocation");
    let fleet_bind = Value::tuple([Value::Int(fleet_ship)]);
    let fleet_flag = [Value::Int(i64::from(infinity_core::consts::FLAG_FLEET_HANGAR))];
    let stack_call = Call { service: INVENTORY, method: "StackAll", args: &fleet_flag, kwargs: &kwargs, bound: true, bind_params: Some(&fleet_bind) };
    inventory(&mut client, &stack_call).await.unwrap();
    let stacked = load_items(&db, &client.sde, ItemQuery::LocationFlags(fleet_ship, &[infinity_core::consts::FLAG_FLEET_HANGAR])).await.unwrap();
    assert_eq!(stacked.iter().filter(|item| item.owner_id == guest.character && item.type_id == commodity).map(|item| i128::from(item.quantity)).sum::<i128>(), 4);
    assert_eq!(stacked.iter().filter(|item| item.owner_id == actor.character && item.type_id == commodity).map(|item| i128::from(item.quantity)).sum::<i128>(), 2);
    let guest_id = stacked.iter().find(|item| item.owner_id == guest.character && item.type_id == commodity).unwrap().item_id;
    let trash_args = [Value::list([Value::Int(guest_id)]), Value::Int(system)];
    let trash_call = Call { service: "invbroker", method: "TrashItems", args: &trash_args, kwargs: &kwargs, bound: true, bind_params: None };
    assert_eq!(super::super::inv_broker::broker(&mut client, &trash_call).await.unwrap().unwrap(), Value::list([]));
    assert!(load_items(&db, &client.sde, ItemQuery::Id(guest_id)).await.unwrap().is_empty());
    assert_eq!(load_items(&db, &client.sde, ItemQuery::Id(actor_stack)).await.unwrap().pop().unwrap().owner_id, actor.character);
    let second_fleet = crate::dao::items::creation::rookie_ship(&db, fleet_type, actor.character, system, FLAG_HANGAR).await.unwrap();
    let second_ball = ball(second_fleet, 2600.0);
    client.world.with(system, move |s| { s.insert(second_ball); }).await.unwrap();
    assert!(crate::dao::items::dogma::set_override(&db, second_fleet, infinity_core::consts::dogma::ATTR_FLEET_HANGAR_CAPACITY, 1_000_000.0).await.unwrap());
    let cross_bay = crate::dao::items::transfers::insert(&db, commodity, guest.character, fleet_ship, infinity_core::consts::FLAG_FLEET_HANGAR, 1).await.unwrap();
    let second_bind = Value::tuple([Value::Int(second_fleet)]);
    let cross_args = [Value::Int(cross_bay), Value::Int(fleet_ship)];
    let cross_call = Call { service: INVENTORY, method: "Add", args: &cross_args, kwargs: &fleet_kwargs, bound: true, bind_params: Some(&second_bind) };
    inventory(&mut client, &cross_call).await.unwrap();
    let cross = load_items(&db, &client.sde, ItemQuery::Id(cross_bay)).await.unwrap().pop().unwrap();
    assert_eq!((cross.owner_id, cross.location_id), (actor.character, second_fleet));
    // An ordinary container still belongs to the same shared bay.
    let box_in_bay = crate::dao::items::creation::rookie_ship(&db, can_type, actor.character, fleet_ship, infinity_core::consts::FLAG_FLEET_HANGAR).await.unwrap();
    let guest_leaf = crate::dao::items::transfers::insert(&db, commodity, guest.character, fleet_ship, infinity_core::consts::FLAG_FLEET_HANGAR, 1).await.unwrap();
    let box_bind = Value::tuple([Value::Int(box_in_bay)]);
    let box_args = [Value::Int(guest_leaf), Value::Int(fleet_ship)];
    let box_call = Call { service: INVENTORY, method: "Add", args: &box_args, kwargs: &kwargs, bound: true, bind_params: Some(&box_bind) };
    inventory(&mut client, &box_call).await.unwrap();
    let leaf = load_items(&db, &client.sde, ItemQuery::Id(guest_leaf)).await.unwrap().pop().unwrap();
    assert_eq!((leaf.owner_id, leaf.location_id, leaf.flag_id), (guest.character, box_in_bay, FLAG_HANGAR));

    // An actor-owned stored ship's private cargo is a different custody boundary.
    let inner_ship = crate::dao::items::creation::rookie_ship(&db, fleet_type, actor.character, fleet_ship, infinity_core::consts::FLAG_FLEET_HANGAR).await.unwrap();
    assert!(crate::dao::items::dogma::set_override(&db, inner_ship, infinity_core::consts::dogma::ATTR_CAPACITY, 1_000_000_000.0).await.unwrap());
    let guest_root = crate::dao::items::creation::rookie_ship(&db, can_type, guest.character, fleet_ship, infinity_core::consts::FLAG_FLEET_HANGAR).await.unwrap();
    let guest_child = crate::dao::items::transfers::insert(&db, commodity, guest.character, guest_root, FLAG_CARGO, 1).await.unwrap();
    let inner_bind = Value::tuple([Value::Int(inner_ship)]);
    let inner_args = [Value::Int(guest_root), Value::Int(fleet_ship)];
    let inner_call = Call { service: INVENTORY, method: "Add", args: &inner_args, kwargs: &kwargs, bound: true, bind_params: Some(&inner_bind) };
    inventory(&mut client, &inner_call).await.unwrap();
    let taken = load_items(&db, &client.sde, ItemQuery::Ids(&[guest_root, guest_child])).await.unwrap();
    assert_eq!(taken.len(), 2);
    assert!(taken.iter().all(|row| row.owner_id == actor.character));
    assert_eq!(taken.iter().find(|row| row.item_id == guest_root).unwrap().location_id, inner_ship);
    assert_eq!(taken.iter().find(|row| row.item_id == guest_child).unwrap().location_id, guest_root);
    let stored = crate::dao::items::creation::rookie_ship(&db, ship_type, guest.character, other_ship, infinity_core::consts::FLAG_SHIP_HANGAR).await.unwrap();
    let hidden = crate::dao::items::transfers::insert(&db, commodity, actor.character, stored, FLAG_CARGO, 1).await.unwrap();
    let hidden_args = [Value::Int(hidden), Value::Int(stored)];
    let hidden_call = Call { service: INVENTORY, method: "Add", args: &hidden_args, kwargs: &kwargs, bound: true, bind_params: Some(&ship_bind) };
    clear_notifications(&mut client, &mut notifications);
    assert!(inventory(&mut client, &hidden_call).await.is_err());
    assert_eq!(load_items(&db, &client.sde, ItemQuery::Id(hidden)).await.unwrap().pop().unwrap().location_id, stored);
    assert_no_notifications(&mut client, &mut notifications);
    let partial = crate::dao::items::transfers::insert(&db, commodity, guest.character, fleet_ship, infinity_core::consts::FLAG_FLEET_HANGAR, 1).await.unwrap();
    let partial_args = [Value::list([Value::Int(partial), Value::Int(hidden)])];
    let partial_call = Call { service: INVENTORY, method: "MultiAdd", args: &partial_args, kwargs: &kwargs, bound: true, bind_params: Some(&ship_bind) };
    assert!(inventory(&mut client, &partial_call).await.is_err());
    let moved = load_items(&db, &client.sde, ItemQuery::Id(partial)).await.unwrap().pop().unwrap();
    assert_eq!((moved.owner_id, moved.location_id), (actor.character, ship));
    assert_eq!(load_items(&db, &client.sde, ItemQuery::Id(hidden)).await.unwrap().pop().unwrap().location_id, stored);
    assert!(has_item_change(&mut client, &mut notifications));
    let absent = Endpoint { location: missing_ball, flag: FLAG_CARGO };
    let no_ball = Reachability::capture(&crate::services::identity::Identity::of_client(&client), &client.db, &client.world, client.session.int("solarsystemid"), &[absent]).await.unwrap();
    assert!(no_ball.check(&db, &client.sde, absent).await.is_err());
    let (custom, uncaptured) = (ball(override_can, override_distance + 100.0), ball(uncaptured_can, 0.0));
    client.world.with(system, move |s| { s.insert(custom); s.insert(uncaptured); }).await.unwrap();
    let custom_endpoint = Endpoint { location: override_can, flag: FLAG_CARGO };
    let custom_reach = Reachability::capture(&crate::services::identity::Identity::of_client(&client), &client.db, &client.world, client.session.int("solarsystemid"), &[custom_endpoint]).await.unwrap();
    assert!(custom_reach.check(&db, &client.sde, custom_endpoint).await.is_ok());
    let custom_far = ball(override_can, override_distance + 101.0);
    client.world.with(system, move |s| { s.insert(custom_far); }).await.unwrap();
    let custom_reach = Reachability::capture(&crate::services::identity::Identity::of_client(&client), &client.db, &client.world, client.session.int("solarsystemid"), &[custom_endpoint]).await.unwrap();
    assert!(custom_reach.check(&db, &client.sde, custom_endpoint).await.is_err());
    let nested_endpoint = Endpoint { location: nested, flag: FLAG_CARGO };
    let captured = Reachability::capture(&crate::services::identity::Identity::of_client(&client), &client.db, &client.world, client.session.int("solarsystemid"), &[nested_endpoint]).await.unwrap();
    db.execute("UPDATE game.items SET location_id = $2 WHERE item_id = $1", &[&nested, &uncaptured_can]).await.unwrap();
    assert!(captured.check(&db, &client.sde, nested_endpoint).await.is_err());
    db.execute("UPDATE game.items SET location_id = $2 WHERE item_id = $1", &[&nested, &can]).await.unwrap();
    let (far_can, far_ship, far_fleet) = (ball(can, 2601.0), ball(other_ship, 2601.0), ball(fleet_ship, 2601.0));
    client.world.with(system, move |s| { s.insert(far_can); s.insert(far_ship); s.insert(far_fleet); }).await.unwrap();
    let reach = Reachability::capture(&crate::services::identity::Identity::of_client(&client), &client.db, &client.world, client.session.int("solarsystemid"), &endpoints).await.unwrap();
    for endpoint in [endpoints[1], endpoints[3], endpoints[4]] {
        assert!(reach.check(&db, &client.sde, endpoint).await.is_err());
    }
    db.execute("UPDATE game.items SET location_id = $2 WHERE item_id = $1", &[&can, &(system - 1)]).await.unwrap();
    let reach = Reachability::capture(&crate::services::identity::Identity::of_client(&client), &client.db, &client.world, client.session.int("solarsystemid"), &endpoints).await.unwrap();
    assert!(reach.check(&db, &client.sde, endpoints[1]).await.is_err());
    client.world.unregister_pilot_generation(guest.character, None);
    fixture::cleanup(&db, guest).await.unwrap();
    fixture::cleanup(&db, actor).await.unwrap();
    assert_eq!(fixture::remaining(&db, actor).await.unwrap(), 0);
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated database with SDE"]
async fn corporate_offices_are_local_even_for_ceo_and_director() {
    use super::super::reachability::{Endpoint, Reachability};
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let actor = fixture::create(&db).await.unwrap();
    let (mut client, _peer) = crate::services::tests::client(db.clone(), actor.character).await;
    let mut notifications = spy_notifications(&mut client);
    let mut stations = client.sde.map.stations();
    let station = stations.next().unwrap().station_id;
    let remote = stations.find(|s| s.station_id != station).unwrap().station_id;
    drop(stations);
    let corp: i64 = db.query_one("INSERT INTO game.corporations (name, ticker, ceo_id, creator_id, station_id, created) VALUES ('reach-' || nextval('game.corporations_corporation_id_seq')::text, 'R' || nextval('game.corporations_corporation_id_seq')::text, $1, $1, $2, 0) RETURNING corporation_id", &[&actor.character, &station]).await.unwrap().get("corporation_id");
    db.execute("UPDATE game.characters SET corporation_id = $2, corp_roles = $3 WHERE character_id = $1", &[&actor.character, &corp, &ROLE_DIRECTOR]).await.unwrap();
    client.session.apply(vec![("stationid", Value::Int(station)), ("corpid", Value::Int(corp))]);
    let ty = client.sde.types.iter().find(|t| t.group_id == Some(infinity_core::consts::GROUP_CARGO_CONTAINER)).unwrap().type_id;
    let office_type = client.sde.types.get(27).expect("office type missing from SDE").type_id;
    let office = crate::dao::items::transfers::insert(&db, office_type, corp, station, FLAG_HANGAR, 1).await.unwrap();
    let distant_office = crate::dao::items::transfers::insert(&db, office_type, corp, remote, FLAG_HANGAR, 1).await.unwrap();
    crate::dao::offices::insert(&db, office, corp, station, 1).await.unwrap();
    crate::dao::offices::insert(&db, distant_office, corp, remote, 1).await.unwrap();
    let local = crate::dao::items::transfers::insert(&db, ty, corp, office, FLAG_CORP_SAG1, 1).await.unwrap();
    let distant = crate::dao::items::transfers::insert(&db, ty, corp, distant_office, FLAG_CORP_SAG1, 1).await.unwrap();
    db.execute("UPDATE game.items SET singleton = true WHERE item_id = ANY($1)", &[&&[office, distant_office, local, distant][..]]).await.unwrap();
    let endpoints = [Endpoint { location: local, flag: FLAG_CARGO }, Endpoint { location: distant, flag: FLAG_CARGO }];
    let reach = Reachability::capture(&crate::services::identity::Identity::of_client(&client), &client.db, &client.world, client.session.int("solarsystemid"), &endpoints).await.unwrap();
    assert!(reach.check(&db, &client.sde, endpoints[0]).await.is_ok());
    assert!(reach.check(&db, &client.sde, endpoints[1]).await.is_err());
    let commodity = client.sde.types.iter().find(|t| t.category_id == Some(infinity_core::consts::CATEGORY_CHARGE) && t.published == Some(true)).unwrap().type_id;
    let local_stack = crate::dao::items::transfers::insert(&db, commodity, corp, local, FLAG_CARGO, 10).await.unwrap();
    let remote_stack = crate::dao::items::transfers::insert(&db, commodity, corp, distant, FLAG_CARGO, 10).await.unwrap();
    let personal = crate::dao::items::transfers::insert(&db, commodity, actor.character, station, FLAG_HANGAR, 10).await.unwrap();
    let hangar = Container::Hangar { station_id: station, owner_id: actor.character };
    for (destination, id) in [(hangar.clone(), remote_stack), (Container::Item(distant_office), personal)] {
        let before = move_fingerprint(&db).await;
        clear_notifications(&mut client, &mut notifications);
        assert!(move_item(&crate::services::identity::Identity::of_client(&client), &client.db, &client.world, &client.sde, client.session.int("solarsystemid"), client.notify_tx.clone(), &destination, id, None, None, None, |n| crate::services::effects::online::dispatch_notification(n, &client.world, &client.notify_tx, client.session.int("charid"))).await.is_err());
        assert_eq!(move_fingerprint(&db).await, before);
        assert_no_notifications(&mut client, &mut notifications);
    }
    let other_ceo = fixture::create(&db).await.unwrap();
    db.execute("UPDATE game.corporations SET ceo_id = $2 WHERE corporation_id = $1", &[&corp, &other_ceo.character]).await.unwrap();
    db.execute("UPDATE game.characters SET corp_roles = 0 WHERE character_id = $1", &[&actor.character]).await.unwrap();
    let before = move_fingerprint(&db).await;
    clear_notifications(&mut client, &mut notifications);
    assert!(move_item(&crate::services::identity::Identity::of_client(&client), &client.db, &client.world, &client.sde, client.session.int("solarsystemid"), client.notify_tx.clone(), &hangar, local_stack, None, None, None, |n| crate::services::effects::online::dispatch_notification(n, &client.world, &client.notify_tx, client.session.int("charid"))).await.is_err());
    assert_eq!(move_fingerprint(&db).await, before);
    assert_no_notifications(&mut client, &mut notifications);
    db.execute("UPDATE game.characters SET corp_roles_hq = $2 WHERE character_id = $1", &[&actor.character, &ROLE_HANGAR_CAN_TAKE_1]).await.unwrap();
    clear_notifications(&mut client, &mut notifications);
    assert!(move_item(&crate::services::identity::Identity::of_client(&client), &client.db, &client.world, &client.sde, client.session.int("solarsystemid"), client.notify_tx.clone(), &hangar, local_stack, None, None, None, |n| crate::services::effects::online::dispatch_notification(n, &client.world, &client.notify_tx, client.session.int("charid"))).await.is_ok());
    let moved = load_items(&db, &client.sde, ItemQuery::Id(local_stack)).await.unwrap().pop().unwrap();
    assert_eq!((moved.owner_id, moved.location_id), (actor.character, station));
    assert!(has_item_change(&mut client, &mut notifications));
    clear_notifications(&mut client, &mut notifications);
    let result = move_item(&crate::services::identity::Identity::of_client(&client), &client.db, &client.world, &client.sde, client.session.int("solarsystemid"), client.notify_tx.clone(), &Container::Item(office), personal, None, None, None, |n| crate::services::effects::online::dispatch_notification(n, &client.world, &client.notify_tx, client.session.int("charid"))).await;
    assert!(result.is_ok(), "local office deposit failed: {result:?}");
    let moved = load_items(&db, &client.sde, ItemQuery::Id(personal)).await.unwrap().pop().unwrap();
    assert_eq!((moved.owner_id, moved.location_id), (corp, office));
    assert!(has_item_change(&mut client, &mut notifications));
    crate::dao::offices::delete(&db, office).await.unwrap();
    crate::dao::offices::delete(&db, distant_office).await.unwrap();
    db.execute("DELETE FROM game.items WHERE owner_id = $1", &[&corp]).await.unwrap();
    db.execute("UPDATE game.characters SET corporation_id = 1 WHERE character_id = $1", &[&actor.character]).await.unwrap();
    db.execute("DELETE FROM game.corporations WHERE corporation_id = $1", &[&corp]).await.unwrap();
    fixture::cleanup(&db, other_ceo).await.unwrap();
    assert_eq!(fixture::remaining(&db, other_ceo).await.unwrap(), 0);
    fixture::cleanup(&db, actor).await.unwrap();
    assert_eq!(fixture::remaining(&db, actor).await.unwrap(), 0);
}
