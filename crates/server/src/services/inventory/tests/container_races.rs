//! Source-container metadata races during inventory movement.

use super::*;
use crate::dao::items::{parent_locks::tests::{backend_pid, wait_blocked}, transfers};
use std::sync::Arc;

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated database with SDE"]
async fn source_parent_drift_after_capture_refuses_move() {
    crate::tests::init_config();
    let config = crate::config::tests::test_database();
    let db = crate::db::Db::connect(&config).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    for drift in 0..4 {
    let actor = crate::services::tests::fixture::create(&db).await.unwrap();
    let station = sde.map.stations().next().unwrap().station_id;
    let box_type = sde.types.iter().find(|ty| super::super::hold_storage::is_ordinary_container(&sde, ty.type_id)).unwrap().type_id;
    let charge = sde.types.iter().find(|ty| ty.category_id == Some(infinity_core::consts::CATEGORY_CHARGE)).unwrap().type_id;
    let parent = transfers::insert(&db, box_type, actor.character, station, FLAG_HANGAR, 1).await.unwrap();
    let source = transfers::insert(&db, charge, actor.character, parent, FLAG_CARGO, 1).await.unwrap();
    let original = load_items(&db, &sde, ItemQuery::Id(source)).await.unwrap().pop().unwrap();
    let (mut client, connection) = tokio_postgres::connect(&config.url, tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let holder = client.transaction().await.unwrap();
    let pid = backend_pid(&holder).await;
    match drift {
        0 => { holder.execute("UPDATE game.items SET owner_id = $2 WHERE item_id = $1", &[&parent, &1_i64]).await.unwrap(); }
        1 => { holder.execute("UPDATE game.items SET location_id = $2 WHERE item_id = $1", &[&parent, &1_i64]).await.unwrap(); }
        2 => { holder.execute("UPDATE game.items SET type_id = $2 WHERE item_id = $1", &[&parent, &charge]).await.unwrap(); }
        _ => { holder.execute("DELETE FROM game.items WHERE item_id = $1", &[&parent]).await.unwrap(); }
    }
    let url = config.url.clone();
    let worker_sde = Arc::clone(&sde);
    let character = actor.character;
    let operation = tokio::spawn(async move {
        let (mut client, connection) = tokio_postgres::connect(&url, tokio_postgres::NoTls).await.unwrap();
        tokio::spawn(async move { connection.await.unwrap(); });
        let tx = client.transaction().await.unwrap();
        let mut outbox: Outbox = Outbox::default();
        let result = prepared_move(&tx, &worker_sde, &mut outbox, &original,
            &Destination::owned(character, station, FLAG_HANGAR), None, Custody::Preserve, character).await;
        let effects = outbox.into_entries().len();
        tx.rollback().await.unwrap();
        (result, effects)
    });
    wait_blocked(&db, pid).await;
    let source_unlocked = db.query_opt("SELECT item_id FROM game.items WHERE item_id=$1 FOR UPDATE NOWAIT", &[&source]).await.is_ok_and(|row| row.is_some());
    holder.commit().await.unwrap();
    let (result, effects) = operation.await.unwrap();
    let row = load_items(&db, &sde, ItemQuery::Id(source)).await.unwrap().pop();
    db.execute("DELETE FROM game.items WHERE item_id = ANY($1)", &[&&[source, parent][..]]).await.unwrap();
    crate::services::tests::fixture::cleanup(&db, actor).await.unwrap();
    assert_eq!(crate::services::tests::fixture::remaining(&db, actor).await.unwrap(), 0);
    assert!(result.is_err());
    assert_eq!(effects, 0);
    assert!(source_unlocked);
    let row = row.unwrap();
    assert_eq!((row.location_id, row.quantity, row.owner_id), (parent, 1, character));
    }
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated database with SDE"]
async fn canonical_deposit_wins_last_space_against_move() {
    crate::tests::init_config();
    let config = crate::config::tests::test_database();
    let db = crate::db::Db::connect(&config).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let actor = crate::services::tests::fixture::create(&db).await.unwrap();
    let station = sde.map.stations().next().unwrap().station_id;
    let (box_type, capacity) = sde.types.iter()
        .filter(|ty| super::super::hold_storage::is_ordinary_container(&sde, ty.type_id))
        .find_map(|ty| {
            let capacity = super::super::hold_storage::static_capacity(&sde, ty.type_id).ok()?;
            (capacity > 0.0 && capacity.fract() == 0.0 && capacity < i32::MAX as f64).then_some((ty.type_id, capacity as i64))
        }).unwrap();
    let charge = sde.types.iter().find(|ty| ty.category_id == Some(infinity_core::consts::CATEGORY_CHARGE)
        && crate::services::item_volume::item_volume(&sde, ty.type_id, false, 1, None).ok() == Some(1.0)).unwrap().type_id;
    let parent = transfers::insert(&db, box_type, actor.character, station, FLAG_HANGAR, 1).await.unwrap();
    let source = transfers::insert(&db, charge, actor.character, station, FLAG_HANGAR, 1).await.unwrap();
    let original = load_items(&db, &sde, ItemQuery::Id(source)).await.unwrap().pop().unwrap();
    let (mut client, connection) = tokio_postgres::connect(&config.url, tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let holder = client.transaction().await.unwrap();
    let pid = backend_pid(&holder).await;
    let mut winner_outbox: Outbox = Outbox::default();
    let deposited = crate::services::items::deposit(&holder, &sde, &mut winner_outbox,
        &Destination::owned(actor.character, parent, FLAG_CARGO), charge, capacity, station).await.unwrap();
    let winner_entries = winner_outbox.into_entries().len();
    let url = config.url.clone();
    let worker_sde = Arc::clone(&sde);
    let character = actor.character;
    let worker = tokio::spawn(async move {
        let (mut client, connection) = tokio_postgres::connect(&url, tokio_postgres::NoTls).await.unwrap();
        tokio::spawn(async move { connection.await.unwrap(); });
        let tx = client.transaction().await.unwrap();
        let mut outbox: Outbox = Outbox::default();
        let result = prepared_move(&tx, &worker_sde, &mut outbox, &original,
            &Destination::owned(character, parent, FLAG_CARGO), None, Custody::Preserve, character).await;
        let effects = outbox.into_entries().len();
        tx.rollback().await.unwrap();
        (result, effects)
    });
    wait_blocked(&db, pid).await;
    holder.commit().await.unwrap();
    let (result, effects) = worker.await.unwrap();
    let source_after = load_items(&db, &sde, ItemQuery::Id(source)).await.unwrap().pop();
    let contents = load_items(&db, &sde, ItemQuery::Location(parent)).await.unwrap();
    crate::services::tests::fixture::cleanup(&db, actor).await.unwrap();
    assert_eq!(crate::services::tests::fixture::remaining(&db, actor).await.unwrap(), 0);
    assert!(result.is_err());
    assert_eq!(effects, 0);
    assert_eq!(winner_entries, 1);
    let source_after = source_after.unwrap();
    assert_eq!((source_after.location_id, source_after.quantity), (station, 1));
    assert_eq!(contents.len(), 1);
    assert_eq!((contents[0].item_id, contents[0].quantity), (deposited.last().item_id, capacity));
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated database with SDE"]
async fn revoked_outer_hangar_role_refuses_captured_move() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    crate::tests::init_config();
    let config = crate::config::tests::test_database();
    let db = crate::db::Db::connect(&config).await.unwrap();
    let actor = crate::services::tests::fixture::create(&db).await.unwrap();
    let ceo = crate::services::tests::fixture::create(&db).await.unwrap();
    let (mut client, _peer) = crate::services::tests::client(db.clone(), actor.character).await;
    let station = client.sde.map.stations().next().unwrap().station_id;
    let corp = crate::services::tests::fixture::create_corporation(&db, ceo.character, station).await.unwrap();
    crate::services::tests::fixture::assign_corporation(&db, actor.character, corp, ROLE_HANGAR_CAN_TAKE_1).await.unwrap();
    client.session.apply(vec![("stationid", Value::Int(station)), ("corpid", Value::Int(corp))]);
    let box_type = client.sde.types.iter().find(|ty| super::super::hold_storage::is_ordinary_container(&client.sde, ty.type_id)).unwrap().type_id;
    let charge = client.sde.types.iter().find(|ty| ty.category_id == Some(infinity_core::consts::CATEGORY_CHARGE)).unwrap().type_id;
    let office = transfers::insert(&db, box_type, corp, station, FLAG_HANGAR, 1).await.unwrap();
    crate::services::tests::fixture::create_office(&db, office, corp, station).await.unwrap();
    let parent = transfers::insert(&db, box_type, corp, office, FLAG_CORP_SAG1, 1).await.unwrap();
    let source = transfers::insert(&db, charge, corp, parent, FLAG_CARGO, 1).await.unwrap();
    db.execute("UPDATE game.items SET singleton = true WHERE item_id = ANY($1)", &[&&[office, parent][..]]).await.unwrap();
    let (mut pg, connection) = tokio_postgres::connect(&config.url, tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let holder = pg.transaction().await.unwrap();
    holder.query_one("SELECT item_id FROM game.items WHERE item_id=$1 FOR UPDATE", &[&parent]).await.unwrap();
    let pid = backend_pid(&holder).await;
    let identity = crate::services::identity::Identity::of_client(&client);
    let world = client.world.clone();
    let sde = client.sde.clone();
    let sender = client.notify_tx.clone();
    let worker_db = db.clone();
    let notices = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&notices);
    let character = actor.character;
    let operation = tokio::spawn(async move {
        let destination = Container::Hangar { station_id: station, owner_id: character };
        move_item(&identity, &worker_db, &world, &sde, None, sender, &destination, source, None, None, None,
            |_| { observed.fetch_add(1, Ordering::SeqCst); Ok(()) }).await
    });
    wait_blocked(&db, pid).await;
    holder.execute("UPDATE game.characters SET corp_roles_hq=0 WHERE character_id=$1", &[&actor.character]).await.unwrap();
    holder.commit().await.unwrap();
    let result = operation.await.unwrap();
    let row = load_items(&db, &client.sde, ItemQuery::Id(source)).await.unwrap().pop();
    crate::services::tests::fixture::cleanup_corporation(&db, corp, &[actor.character], office).await.unwrap();
    crate::services::tests::fixture::cleanup(&db, actor).await.unwrap();
    crate::services::tests::fixture::cleanup(&db, ceo).await.unwrap();
    assert_eq!(crate::services::tests::fixture::remaining(&db, actor).await.unwrap(), 0);
    assert_eq!(crate::services::tests::fixture::remaining(&db, ceo).await.unwrap(), 0);
    assert!(result.is_err());
    assert_eq!(notices.load(Ordering::SeqCst), 0);
    assert_eq!(row.map(|row| (row.location_id, row.owner_id, row.quantity)), Some((parent, corp, 1)));
}
