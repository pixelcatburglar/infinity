//! Ordinary-container accounting against real database rows and loaded SDE data.

use super::*;
use super::super::hold_storage::{self, Addition, Replaced};
use crate::dao::items::{loading, pending_reloads, transfers};
use std::sync::Arc;

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated database with SDE"]
async fn box_snapshot_counts_all_rows_reservations_and_validates_replacement() {
    crate::tests::init_config();
    let config = crate::config::tests::test_database();
    let db = crate::db::Db::connect(&config).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let (box_type, charge, capacity, unit) = sde.types.iter()
        .filter(|ty| ty.group_id.is_some_and(|group| infinity_core::consts::CARGO_CONTAINER_GROUPS.contains(&group)))
        .find_map(|parent| {
            let capacity = parent.attribute(ATTR_CAPACITY).or(parent.capacity).filter(|v| v.is_finite() && *v > 0.0)?;
            let (charge, unit) = sde.types.iter().filter(|ty| ty.category_id == Some(infinity_core::consts::CATEGORY_CHARGE))
                .find_map(|ty| {
                    let volume = crate::services::item_volume::item_volume(&sde, ty.type_id, false, 1, None).ok()?;
                    (volume.is_finite() && volume > 0.0 && volume * 8.0 < capacity && capacity / volume < 1_000_000.0).then_some((ty.type_id, volume))
                })?;
            Some((parent.type_id, charge, capacity, unit))
        }).expect("loaded SDE ordinary container and fitting charge");
    let (mut client, connection) = tokio_postgres::connect(&config.url, tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let tx = client.transaction().await.unwrap();
    let parent = transfers::insert(&tx, box_type, 1, 1, FLAG_HANGAR, 3).await.unwrap();
    let first = transfers::insert(&tx, charge, 1, parent, FLAG_CARGO, 2).await.unwrap();
    let second = transfers::insert(&tx, charge, 2, parent, FLAG_HANGAR, 3).await.unwrap();
    let nested = transfers::insert(&tx, charge, 1, first, FLAG_CARGO, 100).await.unwrap();
    let pending = pending_reloads::insert(&tx, 2, 1, 1, 1, charge, 2, FLAG_HANGAR, parent).await.unwrap();
    let expected = load_items(&tx, &sde, ItemQuery::Id(parent)).await.unwrap().pop().unwrap();
    let snapshot = loading::capacity_snapshot(&tx, parent).await.unwrap();
    assert_eq!(snapshot.items.len(), 2);
    assert_eq!(snapshot.reloads.len(), 1);
    let room = hold_storage::container_room(&sde, snapshot.parent.as_ref().unwrap(), &snapshot.items, &snapshot.reloads).unwrap().unwrap();
    assert_eq!((room.capacity, room.used), (capacity, unit * 7.0));
    assert_eq!(item_capacity_reply(&tx, &sde, parent, Some(&expected)).await.unwrap(), keyval([("capacity", Value::Float(capacity)), ("used", Value::Float(unit * 7.0))]));
    tx.execute("UPDATE game.items SET type_id = $2 WHERE item_id = $1", &[&parent, &charge]).await.unwrap();
    assert!(item_capacity_reply(&tx, &sde, parent, Some(&expected)).await.is_err());
    tx.execute("UPDATE game.items SET type_id = $2 WHERE item_id = $1", &[&parent, &box_type]).await.unwrap();
    for (change, restore) in [
        ("UPDATE game.items SET owner_id = 2 WHERE item_id = $1", "UPDATE game.items SET owner_id = 1 WHERE item_id = $1"),
        ("UPDATE game.items SET location_id = 2 WHERE item_id = $1", "UPDATE game.items SET location_id = 1 WHERE item_id = $1"),
        ("UPDATE game.items SET flag_id = 0 WHERE item_id = $1", "UPDATE game.items SET flag_id = $2 WHERE item_id = $1"),
        ("UPDATE game.items SET quantity = 2 WHERE item_id = $1", "UPDATE game.items SET quantity = 3 WHERE item_id = $1"),
        ("UPDATE game.items SET singleton = true WHERE item_id = $1", "UPDATE game.items SET singleton = false WHERE item_id = $1"),
    ] {
        tx.execute(change, &[&parent]).await.unwrap();
        assert!(item_capacity_reply(&tx, &sde, parent, Some(&expected)).await.is_err());
        if restore.contains("$2") { tx.execute(restore, &[&parent, &FLAG_HANGAR]).await.unwrap(); }
        else { tx.execute(restore, &[&parent]).await.unwrap(); }
    }
    let mut wrong_id = expected.clone();
    wrong_id.item_id += 1;
    assert!(item_capacity_reply(&tx, &sde, parent, Some(&wrong_id)).await.is_err());
    let replacement = [Addition { type_id: charge, singleton: false, quantity: 2 }];
    assert!(hold_storage::check_destination(&tx, &sde, 1, parent, FLAG_CARGO, &replacement, Replaced { items: &[first], reloads: &[] }, Some(0.0)).await.is_ok());
    let growth = [Addition { type_id: charge, singleton: false, quantity: (capacity / unit).floor() as i64 + 1 }];
    let error = hold_storage::check_destination(&tx, &sde, 1, parent, FLAG_CARGO, &growth, Replaced { items: &[first], reloads: &[] }, None).await.err().unwrap();
    match error {
        CallError::User { msg, dict } => {
            assert_eq!(msg, infinity_core::messages::MSG_NOT_ENOUGH_CARGO_SPACE);
            assert_eq!(dict, vec![("available".into(), Value::Float(capacity - (unit * 7.0 - unit * 2.0))), ("volume".into(), Value::Float(unit * growth[0].quantity as f64))]);
        }
        _ => panic!("expected cargo-space refusal"),
    }
    for ids in [&[first, first][..], &[nested][..], &[i64::MAX][..]] {
        assert!(hold_storage::check_destination(&tx, &sde, 1, parent, FLAG_CARGO, &replacement, Replaced { items: ids, reloads: &[] }, None).await.is_err());
    }
    let insufficient = [Addition { type_id: charge, singleton: false, quantity: 1 }];
    assert!(hold_storage::check_destination(&tx, &sde, 1, parent, FLAG_CARGO, &insufficient, Replaced { items: &[], reloads: &[pending] }, None).await.is_err());
    let overfull = (capacity / unit).floor() as i64 + 1;
    tx.execute("UPDATE game.items SET quantity = $2 WHERE item_id = $1", &[&second, &overfull]).await.unwrap();
    assert!(hold_storage::check_destination(&tx, &sde, 1, parent, FLAG_CARGO, &replacement, Replaced { items: &[first], reloads: &[] }, None).await.is_ok());
    let before = loading::capacity_snapshot(&tx, parent).await.unwrap();
    let used_before = hold_storage::container_room(&sde, before.parent.as_ref().unwrap(), &before.items, &before.reloads).unwrap().unwrap().used;
    let source = load_items(&tx, &sde, ItemQuery::Id(first)).await.unwrap().pop().unwrap();
    let mut outbox: Outbox = Outbox::default();
    let moved = prepared_move(&tx, &sde, &mut outbox, &source, &Destination::owned(1, parent, FLAG_HANGAR), Some(1), Custody::Preserve, 1).await.unwrap().unwrap();
    let remaining = load_items(&tx, &sde, ItemQuery::Id(first)).await.unwrap().pop().unwrap();
    let moved = load_items(&tx, &sde, ItemQuery::Id(moved)).await.unwrap().pop().unwrap();
    assert_eq!((remaining.flag_id, remaining.quantity, moved.flag_id, moved.quantity), (FLAG_CARGO, 1, FLAG_HANGAR, 1));
    let after = loading::capacity_snapshot(&tx, parent).await.unwrap();
    assert_eq!(hold_storage::container_room(&sde, after.parent.as_ref().unwrap(), &after.items, &after.reloads).unwrap().unwrap().used, used_before);
    let whole = prepared_move(&tx, &sde, &mut outbox, &moved, &Destination::owned(1, parent, FLAG_CARGO), None, Custody::Preserve, 1).await.unwrap().unwrap();
    let whole = load_items(&tx, &sde, ItemQuery::Id(whole)).await.unwrap().pop().unwrap();
    assert_eq!((whole.location_id, whole.flag_id, whole.quantity), (parent, FLAG_CARGO, 1));
    let after_whole = loading::capacity_snapshot(&tx, parent).await.unwrap();
    assert_eq!(hold_storage::container_room(&sde, after_whole.parent.as_ref().unwrap(), &after_whole.items, &after_whole.reloads).unwrap().unwrap().used, used_before);
    prepared_move(&tx, &sde, &mut outbox, &whole, &Destination::owned(1, 1, FLAG_HANGAR), None, Custody::Preserve, 1).await.unwrap();
    let extracted = load_items(&tx, &sde, ItemQuery::Id(whole.item_id)).await.unwrap().pop().unwrap();
    assert_eq!((extracted.location_id, extracted.flag_id, extracted.quantity), (1, FLAG_HANGAR, 1));
    let error = hold_storage::check_destination(&tx, &sde, 1, parent, FLAG_CARGO, &insufficient, Replaced::default(), Some(f64::MAX)).await.err().unwrap();
    match error {
        CallError::User { msg, dict } => {
            assert_eq!(msg, infinity_core::messages::MSG_NOT_ENOUGH_CARGO_SPACE);
            assert_eq!(dict, vec![("available".into(), Value::Float(0.0)), ("volume".into(), Value::Float(unit))]);
        }
        _ => panic!("expected cargo-space refusal"),
    }
    tx.execute("DELETE FROM game.items WHERE item_id = $1", &[&parent]).await.unwrap();
    assert!(item_capacity_reply(&tx, &sde, parent, Some(&expected)).await.is_err());
    tx.rollback().await.unwrap();
}


#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated database with SDE"]
async fn overfull_box_merge_and_restack_conserve_all_items() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let actor = crate::services::tests::fixture::create(&db).await.unwrap();
    let (mut client, _peer) = crate::services::tests::client(db.clone(), actor.character).await;
    let station = client.sde.map.stations().next().unwrap().station_id;
    client.session.apply(vec![("stationid", Value::Int(station)), ("corpid", Value::Int(1))]);
    for zero in [false, true] {
    let (box_type, charge, capacity, unit) = client.sde.types.iter()
        .filter(|ty| ty.group_id.is_some_and(|group| infinity_core::consts::CARGO_CONTAINER_GROUPS.contains(&group)))
        .find_map(|parent| {
            let capacity = parent.attribute(ATTR_CAPACITY).or(parent.capacity).filter(|v| v.is_finite() && if zero { *v == 0.0 } else { *v > 0.0 })?;
            let (charge, volume) = client.sde.types.iter().filter(|ty| ty.category_id == Some(infinity_core::consts::CATEGORY_CHARGE))
                .find_map(|ty| {
                    let volume = crate::services::item_volume::item_volume(&client.sde, ty.type_id, false, 1, None).ok()?;
                    (volume.is_finite() && volume > 0.0 && capacity / volume < 1_000_000.0).then_some((ty.type_id, volume))
                })?;
            Some((parent.type_id, charge, capacity, volume))
        }).expect("loaded SDE ordinary container and charge");
    let parent = transfers::insert(&db, box_type, actor.character, station, FLAG_HANGAR, 1).await.unwrap();
    db.execute("UPDATE game.items SET singleton = true WHERE item_id = $1", &[&parent]).await.unwrap();
    let quantity = (capacity / unit).floor() as i64 + 1;
    let a = transfers::insert(&db, charge, actor.character, parent, FLAG_CARGO, quantity).await.unwrap();
    let b = transfers::insert(&db, charge, actor.character, parent, FLAG_CARGO, 2).await.unwrap();
    let c = transfers::insert(&db, charge, actor.character, parent, FLAG_CARGO, 3).await.unwrap();
    let used = |snapshot: &loading::CapacitySnapshot| hold_storage::container_room(&client.sde, snapshot.parent.as_ref().unwrap(), &snapshot.items, &snapshot.reloads).unwrap().unwrap().used;
    let before = loading::capacity_snapshot(&db, parent).await.unwrap();
    let used_before = used(&before);
    assert!(used_before > capacity);
    let bind = Value::tuple([Value::Int(parent)]);
    let kwargs = Value::Dict(vec![]);
    let merge = [Value::list([Value::tuple([Value::Int(b), Value::Int(c), Value::Int(1)])])];
    let call = Call { service: INVENTORY, method: "MultiMerge", args: &merge, kwargs: &kwargs, bound: true, bind_params: Some(&bind) };
    inventory(&mut client, &call).await.unwrap();
    let after_merge = loading::capacity_snapshot(&db, parent).await.unwrap();
    assert_eq!(after_merge.items.iter().map(|item| i128::from(item.quantity)).sum::<i128>(), i128::from(quantity) + 5);
    assert_eq!(hold_storage::container_room(&client.sde, after_merge.parent.as_ref().unwrap(), &after_merge.items, &after_merge.reloads).unwrap().unwrap().used, used_before);
    let call = Call { service: INVENTORY, method: "StackAll", args: &[], kwargs: &kwargs, bound: true, bind_params: Some(&bind) };
    inventory(&mut client, &call).await.unwrap();
    let after = loading::capacity_snapshot(&db, parent).await.unwrap();
    assert_eq!(after.items.len(), 1);
    assert_eq!(after.items[0].quantity, quantity + 5);
    assert_eq!(hold_storage::container_room(&client.sde, after.parent.as_ref().unwrap(), &after.items, &after.reloads).unwrap().unwrap().used, used_before);
    assert!(load_items(&db, &client.sde, ItemQuery::Ids(&[a, b, c])).await.unwrap().len() == 1);
    }
    crate::services::tests::fixture::cleanup(&db, actor).await.unwrap();
    assert_eq!(crate::services::tests::fixture::remaining(&db, actor).await.unwrap(), 0);
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated database with SDE"]
async fn zero_box_moves_and_exact_fit_admission() {
    crate::tests::init_config();
    let config = crate::config::tests::test_database();
    let db = crate::db::Db::connect(&config).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let boxes: Vec<_> = sde.types.iter()
        .filter(|ty| ty.group_id.is_some_and(|group| infinity_core::consts::CARGO_CONTAINER_GROUPS.contains(&group)))
        .filter_map(|ty| hold_storage::static_capacity(&sde, ty.type_id).ok().map(|capacity| (ty.type_id, capacity)))
        .collect();
    let zero = boxes.iter().find(|(_, capacity)| *capacity == 0.0).expect("loaded SDE zero capacity container").0;
    let (positive, capacity) = *boxes.iter().find(|(_, capacity)| *capacity > 0.0 && capacity.fract() == 0.0 && *capacity < i32::MAX as f64).expect("loaded SDE integral capacity container");
    let charge = sde.types.iter().find(|ty| ty.category_id == Some(infinity_core::consts::CATEGORY_CHARGE)
        && crate::services::item_volume::item_volume(&sde, ty.type_id, false, 1, None).ok() == Some(1.0)).expect("loaded SDE unit-volume charge").type_id;
    let (mut client, connection) = tokio_postgres::connect(&config.url, tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let tx = client.transaction().await.unwrap();
    let zero_parent = transfers::insert(&tx, zero, 1, 1, FLAG_HANGAR, 1).await.unwrap();
    let source = transfers::insert(&tx, charge, 1, zero_parent, FLAG_CARGO, 3).await.unwrap();
    let initial = load_items(&tx, &sde, ItemQuery::Id(source)).await.unwrap().pop().unwrap();
    let mut outbox: Outbox = Outbox::default();
    let moved_id = prepared_move(&tx, &sde, &mut outbox, &initial, &Destination::owned(1, zero_parent, FLAG_HANGAR), Some(1), Custody::Preserve, 1).await.unwrap().unwrap();
    let remainder = load_items(&tx, &sde, ItemQuery::Id(source)).await.unwrap().pop().unwrap();
    let moved = load_items(&tx, &sde, ItemQuery::Id(moved_id)).await.unwrap().pop().unwrap();
    assert_eq!((remainder.quantity, remainder.flag_id, moved.quantity, moved.flag_id), (2, FLAG_CARGO, 1, FLAG_HANGAR));
    let snapshot = loading::capacity_snapshot(&tx, zero_parent).await.unwrap();
    let room = hold_storage::container_room(&sde, snapshot.parent.as_ref().unwrap(), &snapshot.items, &snapshot.reloads).unwrap().unwrap();
    assert_eq!((room.capacity, room.used), (0.0, 3.0));
    prepared_move(&tx, &sde, &mut outbox, &moved, &Destination::owned(1, zero_parent, FLAG_CARGO), None, Custody::Preserve, 1).await.unwrap();
    let moved = load_items(&tx, &sde, ItemQuery::Id(moved_id)).await.unwrap().pop().unwrap();
    assert_eq!((moved.quantity, moved.flag_id), (1, FLAG_CARGO));
    prepared_move(&tx, &sde, &mut outbox, &moved, &Destination::owned(1, 1, FLAG_HANGAR), None, Custody::Preserve, 1).await.unwrap();
    let extracted = load_items(&tx, &sde, ItemQuery::Id(moved_id)).await.unwrap().pop().unwrap();
    assert_eq!((extracted.location_id, extracted.quantity), (1, 1));
    let one = [Addition { type_id: charge, singleton: false, quantity: 1 }];
    assert!(hold_storage::check_destination(&tx, &sde, 1, zero_parent, FLAG_CARGO, &one, Replaced::default(), None).await.is_err());
    let parent = transfers::insert(&tx, positive, 1, 1, FLAG_HANGAR, 1).await.unwrap();
    let fit = [Addition { type_id: charge, singleton: false, quantity: capacity as i64 }];
    assert!(hold_storage::check_destination(&tx, &sde, 1, parent, FLAG_CARGO, &fit, Replaced::default(), None).await.is_ok());
    transfers::insert(&tx, charge, 1, parent, FLAG_CARGO, capacity as i64).await.unwrap();
    assert!(hold_storage::check_destination(&tx, &sde, 1, parent, FLAG_CARGO, &one, Replaced::default(), None).await.is_err());
    tx.rollback().await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated database with SDE"]
async fn legacy_negative_rows_count_for_display_and_deposit_admission() {
    use crate::dao::items::parent_locks::{LockRequest, LockStrength};
    use super::super::destination::lock_items;
    crate::tests::init_config();
    let config = crate::config::tests::test_database();
    let db = crate::db::Db::connect(&config).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let boxes: Vec<_> = sde.types.iter()
        .filter(|ty| ty.group_id.is_some_and(|group| infinity_core::consts::CARGO_CONTAINER_GROUPS.contains(&group)))
        .filter_map(|ty| hold_storage::static_capacity(&sde, ty.type_id).ok().map(|capacity| (ty.type_id, capacity)))
        .collect();
    let (box_type, capacity) = *boxes.iter().find(|(_, capacity)| *capacity > 5.0 && capacity.fract() == 0.0 && *capacity < i32::MAX as f64).expect("loaded SDE integral capacity container");
    let zero_type = boxes.iter().find(|(_, capacity)| *capacity == 0.0).expect("loaded SDE zero capacity container").0;
    let charge = sde.types.iter().find(|ty| ty.category_id == Some(infinity_core::consts::CATEGORY_CHARGE)
        && crate::services::item_volume::item_volume(&sde, ty.type_id, false, 1, None).ok() == Some(1.0)).expect("loaded SDE unit-volume charge").type_id;
    let (mut client, connection) = tokio_postgres::connect(&config.url, tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let tx = client.transaction().await.unwrap();
    let parent = transfers::insert(&tx, box_type, 1, 1, FLAG_HANGAR, 1).await.unwrap();
    let negative = transfers::insert(&tx, charge, 1, parent, FLAG_CARGO, 1).await.unwrap();
    let positive = transfers::insert(&tx, charge, 1, parent, FLAG_CARGO, capacity as i64 - 1).await.unwrap();
    // This is a legacy backend row encoding, not a claimed public producer.
    tx.execute("UPDATE game.items SET quantity = -5 WHERE item_id = $1", &[&negative]).await.unwrap();
    let expected = load_items(&tx, &sde, ItemQuery::Id(parent)).await.unwrap().pop().unwrap();
    let before = loading::capacity_snapshot(&tx, parent).await.unwrap();
    let fingerprint = |snapshot: &loading::CapacitySnapshot| snapshot.items.iter()
        .map(|item| (item.item_id, item.type_id, item.owner_id, item.location_id, item.flag_id, item.quantity, item.singleton))
        .collect::<Vec<_>>();
    let before_rows = fingerprint(&before);
    let room = hold_storage::container_room(&sde, before.parent.as_ref().unwrap(), &before.items, &before.reloads).unwrap().unwrap();
    assert_eq!((room.capacity, room.used), (capacity, capacity));
    assert_eq!(item_capacity_reply(&tx, &sde, parent, Some(&expected)).await.unwrap(), keyval([("capacity", Value::Float(capacity)), ("used", Value::Float(capacity))]));
    let mut outbox: Outbox = Outbox::default();
    let error = crate::services::items::deposit(&tx, &sde, &mut outbox, &Destination::owned(1, parent, FLAG_CARGO), charge, 1, 0).await.err().unwrap();
    assert!(matches!(error, CallError::User { msg, .. } if msg == infinity_core::messages::MSG_NOT_ENOUGH_CARGO_SPACE));
    let after = loading::capacity_snapshot(&tx, parent).await.unwrap();
    assert_eq!(fingerprint(&after), before_rows);
    assert!(outbox.into_entries().is_empty());

    let requests = [parent, negative, positive].map(|item_id| LockRequest { item_id, strength: LockStrength::Update });
    let locks = lock_items(&tx, &sde, &requests, &[]).await.unwrap();
    assert!([parent, negative, positive].iter().all(|id| locks.is_write_locked(*id)));
    let replacements = [negative, positive];
    let equivalent = [Addition { type_id: charge, singleton: false, quantity: capacity as i64 }];
    assert!(hold_storage::check_destination(&tx, &sde, 1, parent, FLAG_CARGO, &equivalent, Replaced { items: &replacements, reloads: &[] }, None).await.is_ok());
    tx.execute("UPDATE game.items SET quantity = $2 WHERE item_id = $1", &[&positive, &(capacity as i64)]).await.unwrap();
    let overfull = [Addition { type_id: charge, singleton: false, quantity: capacity as i64 + 1 }];
    assert!(hold_storage::check_destination(&tx, &sde, 1, parent, FLAG_CARGO, &overfull, Replaced { items: &replacements, reloads: &[] }, None).await.is_ok());
    drop(locks);

    let zero = transfers::insert(&tx, zero_type, 1, 1, FLAG_HANGAR, 1).await.unwrap();
    let zero_negative = transfers::insert(&tx, charge, 1, zero, FLAG_CARGO, 1).await.unwrap();
    let zero_positive = transfers::insert(&tx, charge, 1, zero, FLAG_CARGO, 5).await.unwrap();
    tx.execute("UPDATE game.items SET quantity = -5 WHERE item_id = $1", &[&zero_negative]).await.unwrap();
    let requests = [zero, zero_negative, zero_positive].map(|item_id| LockRequest { item_id, strength: LockStrength::Update });
    let locks = lock_items(&tx, &sde, &requests, &[]).await.unwrap();
    assert!([zero, zero_negative, zero_positive].iter().all(|id| locks.is_write_locked(*id)));
    let zero_snapshot = loading::capacity_snapshot(&tx, zero).await.unwrap();
    let zero_room = hold_storage::container_room(&sde, zero_snapshot.parent.as_ref().unwrap(), &zero_snapshot.items, &zero_snapshot.reloads).unwrap().unwrap();
    assert_eq!((zero_room.capacity, zero_room.used), (0.0, 6.0));
    let equivalent = [Addition { type_id: charge, singleton: false, quantity: 6 }];
    assert!(hold_storage::check_destination(&tx, &sde, 1, zero, FLAG_CARGO, &equivalent, Replaced { items: &[zero_negative, zero_positive], reloads: &[] }, None).await.is_ok());
    drop(locks);
    tx.rollback().await.unwrap();
    assert!(loading::load_items(&db, ItemQuery::Ids(&[parent, negative, positive, zero, zero_negative, zero_positive])).await.unwrap().is_empty());
}
