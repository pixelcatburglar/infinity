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
    let type_id = client.sde.types.iter().find(|t| t.category_id == Some(crate::consts::CATEGORY_CHARGE) && t.published == Some(true)).unwrap().type_id;
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
    let ty = client.sde.types.iter().find(|t| t.group_id == Some(crate::consts::GROUP_CARGO_CONTAINER)).unwrap().type_id;
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
    let (mut client, _peer) = crate::services::tests::client(db.clone(), actor.character).await;
    let mut notifications = spy_notifications(&mut client);
    let system = -101_i64;
    let ship_type = client.sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SHIP)).unwrap().type_id;
    let maintenance_type = client.sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SHIP) && t.attribute(908).is_some_and(|v| v.is_finite() && v > 0.0)).unwrap().type_id;
    let fleet_type = client.sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SHIP) && t.attribute(912).is_some_and(|v| v.is_finite() && v > 0.0)).unwrap().type_id;
    let no_bay_type = client.sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SHIP) && !t.attribute(908).is_some_and(|v| v.is_finite() && v > 0.0) && !t.attribute(912).is_some_and(|v| v.is_finite() && v > 0.0)).unwrap().type_id;
    let can_type = client.sde.types.iter().find(|t| t.group_id == Some(crate::consts::GROUP_CARGO_CONTAINER) && t.attribute(715).is_none()).unwrap().type_id;
    let ship = crate::dao::items::transfers::insert(&db, ship_type, actor.character, system, FLAG_HANGAR, 1).await.unwrap();
    let other_ship = crate::dao::items::transfers::insert(&db, maintenance_type, actor.character, system, FLAG_HANGAR, 1).await.unwrap();
    let fleet_ship = crate::dao::items::transfers::insert(&db, fleet_type, actor.character, system, FLAG_HANGAR, 1).await.unwrap();
    let no_bay_ship = crate::dao::items::transfers::insert(&db, no_bay_type, actor.character, system, FLAG_HANGAR, 1).await.unwrap();
    let can = crate::dao::items::transfers::insert(&db, can_type, actor.character, system, FLAG_HANGAR, 1).await.unwrap();
    let nested = crate::dao::items::transfers::insert(&db, can_type, actor.character, can, FLAG_CARGO, 1).await.unwrap();
    let override_type = client.sde.types.iter().find(|t| {
        t.group_id == Some(crate::consts::GROUP_CARGO_CONTAINER)
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
        warp: crate::space::WarpState::default(), warp_factor: 0, last_ms: 0, interpolation_anchor_ms: 0, effect_stamp: 0,
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
    let commodity = client.sde.types.iter().find(|t| t.category_id == Some(crate::consts::CATEGORY_CHARGE) && t.published == Some(true)).unwrap().type_id;
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
    let ty = client.sde.types.iter().find(|t| t.group_id == Some(crate::consts::GROUP_CARGO_CONTAINER)).unwrap().type_id;
    let office = crate::dao::items::transfers::insert(&db, ty, corp, station, FLAG_HANGAR, 1).await.unwrap();
    let distant_office = crate::dao::items::transfers::insert(&db, ty, corp, remote, FLAG_HANGAR, 1).await.unwrap();
    crate::dao::offices::insert(&db, office, corp, station, 1).await.unwrap();
    crate::dao::offices::insert(&db, distant_office, corp, remote, 1).await.unwrap();
    let local = crate::dao::items::transfers::insert(&db, ty, corp, office, FLAG_CORP_SAG1, 1).await.unwrap();
    let distant = crate::dao::items::transfers::insert(&db, ty, corp, distant_office, FLAG_CORP_SAG1, 1).await.unwrap();
    db.execute("UPDATE game.items SET singleton = true WHERE item_id = ANY($1)", &[&&[office, distant_office, local, distant][..]]).await.unwrap();
    let endpoints = [Endpoint { location: local, flag: FLAG_CARGO }, Endpoint { location: distant, flag: FLAG_CARGO }];
    let reach = Reachability::capture(&crate::services::identity::Identity::of_client(&client), &client.db, &client.world, client.session.int("solarsystemid"), &endpoints).await.unwrap();
    assert!(reach.check(&db, &client.sde, endpoints[0]).await.is_ok());
    assert!(reach.check(&db, &client.sde, endpoints[1]).await.is_err());
    let commodity = client.sde.types.iter().find(|t| t.category_id == Some(crate::consts::CATEGORY_CHARGE) && t.published == Some(true)).unwrap().type_id;
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
    db.execute("UPDATE game.characters SET corp_roles = $2 WHERE character_id = $1", &[&actor.character, &ROLE_HANGAR_CAN_TAKE_1]).await.unwrap();
    clear_notifications(&mut client, &mut notifications);
    assert!(move_item(&crate::services::identity::Identity::of_client(&client), &client.db, &client.world, &client.sde, client.session.int("solarsystemid"), client.notify_tx.clone(), &hangar, local_stack, None, None, None, |n| crate::services::effects::online::dispatch_notification(n, &client.world, &client.notify_tx, client.session.int("charid"))).await.is_ok());
    let moved = load_items(&db, &client.sde, ItemQuery::Id(local_stack)).await.unwrap().pop().unwrap();
    assert_eq!((moved.owner_id, moved.location_id), (actor.character, station));
    assert!(has_item_change(&mut client, &mut notifications));
    clear_notifications(&mut client, &mut notifications);
    assert!(move_item(&crate::services::identity::Identity::of_client(&client), &client.db, &client.world, &client.sde, client.session.int("solarsystemid"), client.notify_tx.clone(), &Container::Item(office), personal, None, None, None, |n| crate::services::effects::online::dispatch_notification(n, &client.world, &client.notify_tx, client.session.int("charid"))).await.is_ok());
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
