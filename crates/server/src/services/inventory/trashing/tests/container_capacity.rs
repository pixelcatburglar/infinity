//! Reservation release when a trashed ship has an external source.
use super::*;
use infinity_core::consts::{CATEGORY_CHARGE, CATEGORY_MODULE, CATEGORY_SHIP, FLAG_CARGO, FLAG_HI_SLOT_FIRST};
use crate::dao::items::{creation, loading, pending_reloads};
use crate::services::inventory::hold_storage;

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated database with SDE"]
async fn trash_ship_releases_external_zero_capacity_source_without_refund() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let owner = crate::services::tests::fixture::create(&db).await.unwrap();
    let other = crate::services::tests::fixture::create(&db).await.unwrap();
    let worker_db = db.clone();
    let result = tokio::spawn(async move {
    let db = worker_db;
    let (mut client, _peer) = crate::services::tests::client(db.clone(), owner.character).await;
    let station = client.sde.map.stations().next().unwrap().station_id;
    client.session.apply(vec![("stationid", marshal::Value::Int(station)), ("shipid", marshal::Value::Int(0))]);
    let hull_type = client.sde.types.iter().find(|ty| ty.category_id == Some(CATEGORY_SHIP)
        && !client.sde.typelists.is_untrashable(ty.type_id, ty.group_id.unwrap_or_default(), CATEGORY_SHIP)).unwrap().type_id;
    let module_type = client.sde.types.iter().find(|ty| ty.category_id == Some(CATEGORY_MODULE)
        && !client.sde.typelists.is_untrashable(ty.type_id, ty.group_id.unwrap_or_default(), CATEGORY_MODULE)).unwrap().type_id;
    let box_type = client.sde.types.iter().find(|ty| hold_storage::is_ordinary_container(&client.sde, ty.type_id)
        && hold_storage::static_capacity(&client.sde, ty.type_id).is_ok_and(|cap| cap == 0.0)).unwrap().type_id;
    let charge = client.sde.types.iter().find(|ty| ty.category_id == Some(CATEGORY_CHARGE)
        && ty.volume.is_some_and(|v| v.is_finite() && v > 0.0)).unwrap().type_id;
    let ship = creation::rookie_ship(&db, hull_type, owner.character, station, FLAG_HANGAR).await.unwrap();
    let weapon = creation::rookie_ship(&db, module_type, owner.character, ship, FLAG_HI_SLOT_FIRST).await.unwrap();
    let source_hull = creation::rookie_ship(&db, hull_type, other.character, station, FLAG_HANGAR).await.unwrap();
    let source = creation::rookie_ship(&db, box_type, other.character, source_hull, FLAG_CARGO).await.unwrap();
    let reservation = pending_reloads::insert(&db, owner.character, ship, weapon, FLAG_HI_SLOT_FIRST, charge, 1, FLAG_CARGO, source).await.unwrap();
    let before = loading::capacity_snapshot(&db, source).await.unwrap();
    let room = hold_storage::container_room(&client.sde, before.parent.as_ref().unwrap(), &before.items, &before.reloads).unwrap().unwrap();
    assert_eq!(room.capacity, 0.0);
    assert!(room.used > 0.0);
    assert_eq!(before.reloads.len(), 1);
    assert_eq!(before.reloads[0].reload_id, reservation);
    let source_rows: String = db.query_one("SELECT string_agg(row_to_json(i)::text, '' ORDER BY item_id) AS value FROM game.items i WHERE item_id=ANY($1) OR location_id=$2", &[&vec![source_hull, source], &source]).await.unwrap().get("value");
    let expected = crate::services::items::load_items(&db, &client.sde, loading::ItemQuery::Ids(&[ship, weapon])).await.unwrap();
    let (notify, mut events) = tokio::sync::mpsc::unbounded_channel();
    client.notify_tx = notify;
    let config = crate::config::tests::test_database();
    let (mut holder, connection) = tokio_postgres::connect(&config.url, tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let tx = holder.transaction().await.unwrap();
    let holder_pid = crate::dao::items::parent_locks::tests::backend_pid(&tx).await;
    tx.query_one("SELECT item_id FROM game.items WHERE item_id=$1 FOR UPDATE", &[&source]).await.unwrap();
    let operation = tokio::spawn(async move {
        let outcome = super::super::trash_items(&mut client, &[ship], station).await;
        (outcome, client)
    });
    crate::dao::items::parent_locks::tests::wait_blocked(&db, holder_pid).await;
    let hull_probe = db.query_one("SELECT item_id FROM game.items WHERE item_id=$1 FOR UPDATE NOWAIT", &[&source_hull]).await;
    let pending_probe = db.query_one("SELECT reload_id FROM game.pending_reloads WHERE reload_id=$1 FOR UPDATE NOWAIT", &[&reservation]).await;
    tx.commit().await.unwrap();
    let (outcome, client) = operation.await.unwrap();
    assert!(matches!(hull_probe, Err(crate::db::Error::Postgres(error)) if error.code() == Some(&tokio_postgres::error::SqlState::LOCK_NOT_AVAILABLE)));
    assert!(pending_probe.is_ok(), "reservation must not be locked before its source parent");
    assert_eq!(outcome.unwrap(), marshal::Value::list([]));
    assert!(client.outbox.is_empty());
    let notices: Vec<_> = std::iter::from_fn(|| events.try_recv().ok()).collect();
    for item in expected {
        let gone = Item { location_id: infinity_core::consts::LOCATION_JUNKYARD, ..item };
        assert!(notices.iter().any(|(event, args)| event == "OnItemChange" && args.first() == Some(&gone.row())));
    }
    let after_rows: String = db.query_one("SELECT string_agg(row_to_json(i)::text, '' ORDER BY item_id) AS value FROM game.items i WHERE item_id=ANY($1) OR location_id=$2", &[&vec![source_hull, source], &source]).await.unwrap().get("value");
    assert_eq!(source_rows, after_rows);
    let (mut client, _peer) = crate::services::tests::client(db.clone(), owner.character).await;
    client.session.apply(vec![("stationid", marshal::Value::Int(station)), ("shipid", marshal::Value::Int(0))]);
    let after = loading::capacity_snapshot(&db, source).await.unwrap();
    let after_room = hold_storage::container_room(&client.sde, after.parent.as_ref().unwrap(), &after.items, &after.reloads).unwrap().unwrap();
    assert_eq!(after_room.used, 0.0);
    assert!(after.reloads.is_empty());
    assert!(after.items.is_empty());
    assert_eq!(after.parent.unwrap().owner_id, other.character);
    assert!(pending_reloads::for_ship(&db, ship).await.unwrap().is_empty());
    let absent_ship = creation::rookie_ship(&db, hull_type, owner.character, station, FLAG_HANGAR).await.unwrap();
    let absent_weapon = creation::rookie_ship(&db, module_type, owner.character, absent_ship, FLAG_HI_SLOT_FIRST).await.unwrap();
    let absent_source = -absent_ship;
    pending_reloads::insert(&db, owner.character, absent_ship, absent_weapon, FLAG_HI_SLOT_FIRST, charge, 1, FLAG_CARGO, absent_source).await.unwrap();
    assert!(loading::capacity_snapshot(&db, absent_source).await.unwrap().parent.is_none());
    assert_eq!(super::super::trash_items(&mut client, &[absent_ship], station).await.unwrap(), marshal::Value::list([]));
    assert!(pending_reloads::for_ship(&db, absent_ship).await.unwrap().is_empty());
    assert!(loading::capacity_snapshot(&db, absent_source).await.unwrap().parent.is_none());
    }).await;
    db.execute("DELETE FROM game.pending_reloads WHERE owner_id=$1", &[&owner.character]).await.unwrap();
    crate::services::tests::fixture::cleanup(&db, owner).await.unwrap();
    crate::services::tests::fixture::cleanup(&db, other).await.unwrap();
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated database with SDE"]
async fn trash_weapon_releases_overfull_external_source_after_owner_change() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let owner = crate::services::tests::fixture::create(&db).await.unwrap();
    let other = crate::services::tests::fixture::create(&db).await.unwrap();
    let worker_db = db.clone();
    let result = tokio::spawn(async move {
        let (mut client, _peer) = crate::services::tests::client(worker_db.clone(), owner.character).await;
        let station = client.sde.map.stations().next().unwrap().station_id;
        client.session.apply(vec![("stationid", marshal::Value::Int(station)), ("shipid", marshal::Value::Int(0))]);
        let hull = client.sde.types.iter().find(|ty| ty.category_id == Some(CATEGORY_SHIP)).unwrap().type_id;
        let module = client.sde.types.iter().find(|ty| ty.category_id == Some(CATEGORY_MODULE)
            && !client.sde.typelists.is_untrashable(ty.type_id, ty.group_id.unwrap_or_default(), CATEGORY_MODULE)).unwrap().type_id;
        let charge = client.sde.types.iter().find(|ty| ty.category_id == Some(CATEGORY_CHARGE)
            && ty.volume.is_some_and(|v| v.is_finite() && v > 0.0)).unwrap().type_id;
        let box_type = client.sde.types.iter().find(|ty| hold_storage::is_ordinary_container(&client.sde, ty.type_id)
            && hold_storage::static_capacity(&client.sde, ty.type_id).is_ok_and(|cap| cap > 0.0)).unwrap().type_id;
        let ship = creation::rookie_ship(&worker_db, hull, owner.character, station, FLAG_HANGAR).await.unwrap();
        let weapon = creation::rookie_ship(&worker_db, module, owner.character, ship, FLAG_HI_SLOT_FIRST).await.unwrap();
        let source_hull = creation::rookie_ship(&worker_db, hull, other.character, station, FLAG_HANGAR).await.unwrap();
        let source = creation::rookie_ship(&worker_db, box_type, owner.character, source_hull, FLAG_CARGO).await.unwrap();
        worker_db.execute("UPDATE game.items SET owner_id=$2 WHERE item_id=$1", &[&source, &other.character]).await.unwrap();
        let child = crate::dao::items::transfers::insert(&worker_db, charge, other.character, source, FLAG_CARGO, 1).await.unwrap();
        let cap = hold_storage::static_capacity(&client.sde, box_type).unwrap();
        let volume = crate::services::item_volume::item_volume(&client.sde, charge, false, 1, None).unwrap();
        let quantity = (cap / volume).floor() + 1.0;
        assert!(quantity.is_finite() && quantity <= f64::from(i32::MAX), "fixture needs a representable overfull reservation");
        let reservation = pending_reloads::insert(&worker_db, owner.character, ship, weapon, FLAG_HI_SLOT_FIRST, charge, quantity as i64, FLAG_CARGO, source).await.unwrap();
        let before = loading::capacity_snapshot(&worker_db, source).await.unwrap();
        let room = hold_storage::container_room(&client.sde, before.parent.as_ref().unwrap(), &before.items, &before.reloads).unwrap().unwrap();
        assert!(room.capacity > 0.0 && room.used > room.capacity);
        let source_rows = worker_db.query_one("SELECT string_agg(row_to_json(i)::text, '' ORDER BY item_id) AS value FROM game.items i WHERE item_id=$1 OR location_id=$1", &[&source]).await.unwrap().get::<_, String>("value");
        let expected = crate::services::items::load_items(&worker_db, &client.sde, loading::ItemQuery::Id(weapon)).await.unwrap().pop().unwrap();
        let gone = Item { location_id: infinity_core::consts::LOCATION_JUNKYARD, ..expected };
        let (notify, mut events) = tokio::sync::mpsc::unbounded_channel();
        client.notify_tx = notify;
        let outcome = super::super::trash_items(&mut client, &[weapon], station).await;
        let after = loading::capacity_snapshot(&worker_db, source).await.unwrap();
        let after_room = hold_storage::container_room(&client.sde, after.parent.as_ref().unwrap(), &after.items, &after.reloads).unwrap().unwrap();
        let after_rows = worker_db.query_one("SELECT string_agg(row_to_json(i)::text, '' ORDER BY item_id) AS value FROM game.items i WHERE item_id=$1 OR location_id=$1", &[&source]).await.unwrap().get::<_, String>("value");
        let notices: Vec<_> = std::iter::from_fn(|| events.try_recv().ok()).collect();
        assert_eq!(outcome.unwrap(), marshal::Value::list([]));
        assert_eq!(source_rows, after_rows);
        assert_eq!(after.items.len(), 1);
        assert_eq!(after.items[0].item_id, child);
        assert_eq!(after_room.used, crate::services::item_volume::item_volume(&client.sde, charge, false, 1, None).unwrap());
        assert_eq!(room.used, crate::services::item_volume::item_volume(&client.sde, charge, false, quantity as i64 + 1, None).unwrap());
        assert!(after.reloads.is_empty());
        assert!(pending_reloads::for_ship(&worker_db, ship).await.unwrap().iter().all(|row| row.reload_id != reservation));
        assert!(worker_db.query_opt("SELECT item_id FROM game.items WHERE item_id=$1", &[&ship]).await.unwrap().is_some());
        assert!(worker_db.query_opt("SELECT item_id FROM game.items WHERE item_id=$1", &[&weapon]).await.unwrap().is_none());
        assert!(notices.iter().any(|(event, args)| event == "OnItemChange" && args.first() == Some(&gone.row())));
        assert!(client.outbox.is_empty());
    }).await;
    db.execute("DELETE FROM game.pending_reloads WHERE owner_id=$1", &[&owner.character]).await.unwrap();
    crate::services::tests::fixture::cleanup(&db, owner).await.unwrap();
    crate::services::tests::fixture::cleanup(&db, other).await.unwrap();
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated database with SDE"]
async fn revoked_corporate_hangar_authority_refuses_trash_with_pending_source() {
    use infinity_core::consts::{FLAG_CORP_SAG1, ROLE_HANGAR_CAN_TAKE_1};
    crate::tests::init_config();
    let config = crate::config::tests::test_database();
    let db = crate::db::Db::connect(&config).await.unwrap();
    let actor = crate::services::tests::fixture::create(&db).await.unwrap();
    let ceo = crate::services::tests::fixture::create(&db).await.unwrap();
    let (mut client, _peer) = crate::services::tests::client(db.clone(), actor.character).await;
    let station = client.sde.map.stations().next().unwrap().station_id;
    let corp = crate::services::tests::fixture::create_corporation(&db, ceo.character, station).await.unwrap();
    crate::services::tests::fixture::assign_corporation(&db, actor.character, corp, ROLE_HANGAR_CAN_TAKE_1).await.unwrap();
    client.session.apply(vec![("stationid", marshal::Value::Int(station)), ("shipid", marshal::Value::Int(0)), ("corpid", marshal::Value::Int(corp))]);
    let box_type = client.sde.types.iter().find(|ty| hold_storage::is_ordinary_container(&client.sde, ty.type_id)).unwrap().type_id;
    let office = crate::dao::items::transfers::insert(&db, box_type, corp, station, FLAG_HANGAR, 1).await.unwrap();
    crate::services::tests::fixture::create_office(&db, office, corp, station).await.unwrap();
    db.execute("UPDATE game.items SET singleton=true WHERE item_id=$1", &[&office]).await.unwrap();
    let worker_db = db.clone();
    let result = tokio::spawn(async move {
        let hull = client.sde.types.iter().find(|ty| ty.category_id == Some(CATEGORY_SHIP)
            && !client.sde.typelists.is_untrashable(ty.type_id, ty.group_id.unwrap_or_default(), CATEGORY_SHIP)).unwrap().type_id;
        let module = client.sde.types.iter().find(|ty| ty.category_id == Some(CATEGORY_MODULE)
            && !client.sde.typelists.is_untrashable(ty.type_id, ty.group_id.unwrap_or_default(), CATEGORY_MODULE)).unwrap().type_id;
        let charge = client.sde.types.iter().find(|ty| ty.category_id == Some(CATEGORY_CHARGE)
            && ty.volume.is_some_and(|v| v.is_finite() && v > 0.0)).unwrap().type_id;
        let ship = creation::rookie_ship(&worker_db, hull, corp, office, FLAG_CORP_SAG1).await.unwrap();
        let weapon = creation::rookie_ship(&worker_db, module, corp, ship, FLAG_HI_SLOT_FIRST).await.unwrap();
        let source_hull = creation::rookie_ship(&worker_db, hull, actor.character, station, FLAG_HANGAR).await.unwrap();
        let source = creation::rookie_ship(&worker_db, box_type, actor.character, source_hull, FLAG_CARGO).await.unwrap();
        pending_reloads::insert(&worker_db, actor.character, ship, weapon, FLAG_HI_SLOT_FIRST, charge, 1, FLAG_CARGO, source).await.unwrap();
        let before = crate::services::tests::fixture::market_fingerprint(&worker_db, &[actor.character, corp], &[actor.character]).await.unwrap();
        let pending = pending_reloads::for_ship(&worker_db, ship).await.unwrap();
        let (notify, mut events) = tokio::sync::mpsc::unbounded_channel();
        client.notify_tx = notify;
        let (mut pg, connection) = tokio_postgres::connect(&config.url, tokio_postgres::NoTls).await.unwrap();
        tokio::spawn(async move { connection.await.unwrap(); });
        let holder = pg.transaction().await.unwrap();
        holder.query_one("SELECT item_id FROM game.items WHERE item_id=$1 FOR UPDATE", &[&ship]).await.unwrap();
        let pid = crate::dao::items::parent_locks::tests::backend_pid(&holder).await;
        let operation = tokio::spawn(async move {
            let outcome = super::super::trash_items(&mut client, &[ship], station).await;
            (outcome, client)
        });
        crate::dao::items::parent_locks::tests::wait_blocked(&worker_db, pid).await;
        holder.execute("UPDATE game.characters SET corp_roles_hq=0 WHERE character_id=$1", &[&actor.character]).await.unwrap();
        holder.commit().await.unwrap();
        let (outcome, client) = operation.await.unwrap();
        worker_db.execute("UPDATE game.characters SET corp_roles_hq=$2 WHERE character_id=$1", &[&actor.character, &ROLE_HANGAR_CAN_TAKE_1]).await.unwrap();
        assert!(outcome.is_err());
        assert!(pending_reloads::for_ship(&worker_db, ship).await.unwrap() == pending);
        assert_eq!(crate::services::tests::fixture::market_fingerprint(&worker_db, &[actor.character, corp], &[actor.character]).await.unwrap(), before);
        assert!(events.try_recv().is_err());
        assert!(client.outbox.is_empty());
    }).await;
    db.execute("DELETE FROM game.pending_reloads WHERE owner_id=$1", &[&actor.character]).await.unwrap();
    crate::services::tests::fixture::cleanup_corporation(&db, corp, &[actor.character], office).await.unwrap();
    crate::services::tests::fixture::cleanup(&db, actor).await.unwrap();
    crate::services::tests::fixture::cleanup(&db, ceo).await.unwrap();
    result.unwrap();
}
