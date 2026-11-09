//! Opposing ordinary-container movements with real transaction locks.

use super::*;
use crate::dao::items::{parent_locks::tests::{backend_pid, wait_blocked}, transfers};
use std::sync::{Arc, atomic::{AtomicUsize, Ordering}, Mutex};

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated database with SDE"]
async fn opposing_box_moves_commit_without_retry() {
    crate::tests::init_config();
    let config = crate::config::tests::test_database();
    let db = crate::db::Db::connect(&config).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    for nested in [false, true] {
    let actor = crate::services::tests::fixture::create(&db).await.unwrap();
    let station = sde.map.stations().next().unwrap().station_id;
    let (box_type, charge) = sde.types.iter()
        .filter(|ty| super::super::hold_storage::is_ordinary_container(&sde, ty.type_id))
        .find_map(|parent| {
            let cap = super::super::hold_storage::static_capacity(&sde, parent.type_id).ok()?;
            sde.types.iter().filter(|ty| ty.category_id == Some(infinity_core::consts::CATEGORY_CHARGE)).find_map(|ty| {
                let volume = crate::services::item_volume::item_volume(&sde, ty.type_id, false, 1, None).ok()?;
                (volume > 0.0 && volume * 2.0 <= cap).then_some((parent.type_id, ty.type_id))
            })
        }).unwrap();
    let b = if nested {
        let box_volume = crate::services::item_volume::item_volume(&sde, box_type, true, 1, None).unwrap();
        let charge_volume = crate::services::item_volume::item_volume(&sde, charge, false, 1, None).unwrap();
        let hull_type = sde.types.iter().find(|ty| ty.category_id == Some(CATEGORY_SHIP)
            && ty.attribute(ATTR_CAPACITY).or(ty.capacity).is_some_and(|capacity| capacity > box_volume + 2.0 * charge_volume)).unwrap().type_id;
        crate::dao::items::creation::rookie_ship(&db, hull_type, actor.character, station, FLAG_HANGAR).await.unwrap()
    } else {
        transfers::insert(&db, box_type, actor.character, station, FLAG_HANGAR, 1).await.unwrap()
    };
    let a = transfers::insert(&db, box_type, actor.character, if nested { b } else { station }, if nested { FLAG_CARGO } else { FLAG_HANGAR }, 1).await.unwrap();
    if nested { db.execute("UPDATE game.items SET singleton = true WHERE item_id = $1", &[&a]).await.unwrap(); }
    let x = transfers::insert(&db, charge, actor.character, a, FLAG_CARGO, 1).await.unwrap();
    let y = transfers::insert(&db, charge, actor.character, b, FLAG_CARGO, 1).await.unwrap();
    let x_item = load_items(&db, &sde, ItemQuery::Id(x)).await.unwrap().pop().unwrap();
    let y_item = load_items(&db, &sde, ItemQuery::Id(y)).await.unwrap().pop().unwrap();
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let ready = Arc::new(Mutex::new(Some(ready_tx)));
    let release = Arc::new(tokio::sync::Mutex::new(Some(release_rx)));
    let first_count = Arc::new(AtomicUsize::new(0));
    let second_count = Arc::new(AtomicUsize::new(0));
    let first_db = db.clone();
    let first_sde = Arc::clone(&sde);
    let count = Arc::clone(&first_count);
    let character = actor.character;
    let first = tokio::spawn(async move {
        first_db.transaction(|tx, outbox: &mut Outbox| {
            count.fetch_add(1, Ordering::SeqCst);
            let item = x_item.clone();
            let sde = Arc::clone(&first_sde);
            let ready = Arc::clone(&ready);
            let release = Arc::clone(&release);
            Box::pin(async move {
                let value = prepared_move(tx, &sde, outbox, &item, &Destination::owned(character, b, FLAG_CARGO), None, Custody::Preserve, character).await?;
                let pid = backend_pid(tx).await;
                if let Some(sender) = ready.lock().unwrap().take() { let _ = sender.send(pid); }
                if let Some(receiver) = release.lock().await.take() { let _ = receiver.await; }
                Ok::<_, crate::services::CallError>(value)
            })
        }, |value, outbox| async move { Ok((value, outbox.into_entries().len())) }).await
    });
    let pid = ready_rx.await.unwrap();
    let second_db = db.clone();
    let second_sde = Arc::clone(&sde);
    let count = Arc::clone(&second_count);
    let second = tokio::spawn(async move {
        second_db.transaction(|tx, outbox: &mut Outbox| {
            count.fetch_add(1, Ordering::SeqCst);
            let item = y_item.clone();
            let sde = Arc::clone(&second_sde);
            Box::pin(async move {
                prepared_move(tx, &sde, outbox, &item, &Destination::owned(character, a, FLAG_CARGO), None, Custody::Preserve, character).await
            })
        }, |value, outbox| async move { Ok((value, outbox.into_entries().len())) }).await
    });
    wait_blocked(&db, pid).await;
    release_tx.send(()).unwrap();
    let first_result = first.await.unwrap();
    let second_result = second.await.unwrap();
    let final_rows = load_items(&db, &sde, ItemQuery::Ids(&[x, y])).await.unwrap();
    crate::services::tests::fixture::cleanup(&db, actor).await.unwrap();
    assert_eq!(crate::services::tests::fixture::remaining(&db, actor).await.unwrap(), 0);
    assert_eq!(first_count.load(Ordering::SeqCst), 1);
    assert_eq!(second_count.load(Ordering::SeqCst), 1);
    assert_eq!(first_result.unwrap(), (Some(x), 1));
    assert_eq!(second_result.unwrap(), (Some(y), 1));
    assert_eq!(final_rows.iter().find(|row| row.item_id == x).map(|row| (row.location_id, row.quantity)), Some((b, 1)));
    assert_eq!(final_rows.iter().find(|row| row.item_id == y).map(|row| (row.location_id, row.quantity)), Some((a, 1)));
    }
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated database with SDE"]
async fn hull_then_older_stack_crosses_mixed_move_without_retry() {
    use crate::dao::items::{creation, quantities::{self, QuantityChange}, ship};
    use std::collections::BTreeMap;
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let actor = crate::services::tests::fixture::create(&db).await.unwrap();
    let station = sde.map.stations().next().unwrap().station_id;
    let (box_type, charge) = sde.types.iter()
        .filter(|ty| super::super::hold_storage::is_ordinary_container(&sde, ty.type_id))
        .find_map(|parent| {
            let cap = super::super::hold_storage::static_capacity(&sde, parent.type_id).ok()?;
            sde.types.iter().filter(|ty| ty.category_id == Some(infinity_core::consts::CATEGORY_CHARGE)).find_map(|ty| {
                let volume = crate::services::item_volume::item_volume(&sde, ty.type_id, false, 1, None).ok()?;
                (volume > 0.0 && volume <= cap).then_some((parent.type_id, ty.type_id))
            })
        }).unwrap();
    let stack = transfers::insert(&db, charge, actor.character, station, FLAG_HANGAR, 1).await.unwrap();
    let hull_type = sde.types.iter().find(|ty| ty.category_id == Some(CATEGORY_SHIP)).unwrap().type_id;
    let hull = creation::rookie_ship(&db, hull_type, actor.character, station, FLAG_HANGAR).await.unwrap();
    db.execute("UPDATE game.items SET location_id=$2, flag_id=$3 WHERE item_id=$1", &[&stack, &hull, &FLAG_CARGO]).await.unwrap();
    let target = transfers::insert(&db, box_type, actor.character, station, FLAG_HANGAR, 1).await.unwrap();
    let item = load_items(&db, &sde, ItemQuery::Id(stack)).await.unwrap().pop().unwrap();
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let ready = Arc::new(Mutex::new(Some(ready_tx)));
    let release = Arc::new(tokio::sync::Mutex::new(Some(release_rx)));
    let first_count = Arc::new(AtomicUsize::new(0));
    let second_count = Arc::new(AtomicUsize::new(0));
    let count = Arc::clone(&first_count);
    let first_db = db.clone();
    let fuel_item = item.clone();
    let first = tokio::spawn(async move {
        first_db.transaction(|tx, _: &mut Outbox| {
            count.fetch_add(1, Ordering::SeqCst);
            let ready = Arc::clone(&ready);
            let release = Arc::clone(&release);
            let item = fuel_item.clone();
            Box::pin(async move {
                if !ship::lock_reload_ship(tx, hull).await.map_err(db_err)? {
                    return Err(crate::services::notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION));
                }
                let pid = backend_pid(tx).await;
                if let Some(sender) = ready.lock().unwrap().take() { let _ = sender.send(pid); }
                if let Some(receiver) = release.lock().await.take() { let _ = receiver.await; }
                let changes = BTreeMap::from([(stack, QuantityChange::new(&item, Some(item.quantity)))]);
                quantities::apply(tx, &changes).await.map_err(db_err)
            })
        }, |value, outbox| async move { Ok((value, outbox.into_entries().len())) }).await
    });
    let pid = ready_rx.await.unwrap();
    let second_db = db.clone();
    let second_sde = Arc::clone(&sde);
    let count = Arc::clone(&second_count);
    let character = actor.character;
    let second = tokio::spawn(async move {
        second_db.transaction(|tx, outbox: &mut Outbox| {
            count.fetch_add(1, Ordering::SeqCst);
            let item = item.clone();
            let sde = Arc::clone(&second_sde);
            Box::pin(async move {
                prepared_move(tx, &sde, outbox, &item, &Destination::owned(character, target, FLAG_CARGO), None, Custody::Preserve, character).await
            })
        }, |value, outbox| async move { Ok((value, outbox.into_entries().len())) }).await
    });
    wait_blocked(&db, pid).await;
    let stack_unlocked = db.query_opt("SELECT item_id FROM game.items WHERE item_id=$1 FOR UPDATE NOWAIT", &[&stack]).await.is_ok_and(|row| row.is_some());
    release_tx.send(()).unwrap();
    let first_result = first.await.unwrap();
    let second_result = second.await.unwrap();
    let final_item = load_items(&db, &sde, ItemQuery::Id(stack)).await.unwrap().pop();
    crate::services::tests::fixture::cleanup(&db, actor).await.unwrap();
    assert_eq!(crate::services::tests::fixture::remaining(&db, actor).await.unwrap(), 0);
    assert!(stack < hull);
    assert!(stack_unlocked);
    assert_eq!(first_count.load(Ordering::SeqCst), 1);
    assert_eq!(second_count.load(Ordering::SeqCst), 1);
    assert_eq!(first_result.unwrap(), (true, 0));
    assert_eq!(second_result.unwrap(), (Some(stack), 1));
    assert_eq!(final_item.map(|row| (row.location_id, row.quantity)), Some((target, 1)));
}
