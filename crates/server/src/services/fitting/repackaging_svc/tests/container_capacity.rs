//! Multi-root repackaging with a surviving external ammunition source.
use super::*;
use infinity_core::consts::{CATEGORY_CHARGE, FLAG_CARGO, FLAG_HI_SLOT_FIRST};
use crate::dao::items::{creation, loading, pending_reloads};
use crate::services::inventory::hold_storage;

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database with SDE"]
async fn two_roots_refund_into_surviving_overfull_nested_box() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let fixture = crate::services::tests::fixture::create(&db).await.unwrap();
    let worker_db = db.clone();
    let result = tokio::spawn(async move {
    let db = worker_db;
    let (mut client, _peer) = crate::services::tests::client(db.clone(), fixture.character).await;
    let station = client.sde.map.stations().next().unwrap().station_id;
    client.session.apply(vec![("stationid", Value::Int(station)), ("shipid", Value::Int(0))]);
    let hull = client.sde.types.iter().find(|ty| ty.category_id == Some(CATEGORY_SHIP)
        && ty.group_id.map(i64::from) != Some(GROUP_STRATEGIC_CRUISER)).unwrap().type_id;
    let module = client.sde.types.iter().find(|ty| ty.category_id == Some(CATEGORY_MODULE)).unwrap().type_id;
    let charge = client.sde.types.iter().find(|ty| ty.category_id == Some(CATEGORY_CHARGE)
        && ty.volume.is_some_and(|v| v.is_finite() && v > 0.0)).unwrap().type_id;
    let box_type = client.sde.types.iter().find(|ty| hold_storage::is_ordinary_container(&client.sde, ty.type_id)
        && hold_storage::static_capacity(&client.sde, ty.type_id).is_ok_and(|cap| cap == 0.0)).unwrap().type_id;
    let owner = fixture.character;
    let source_hull = creation::rookie_ship(&db, hull, owner, station, FLAG_HANGAR).await.unwrap();
    let source = creation::rookie_ship(&db, box_type, owner, source_hull, FLAG_CARGO).await.unwrap();
    let mut roots = Vec::new();
    for _ in 0..2 {
        let ship = creation::rookie_ship(&db, hull, owner, station, FLAG_HANGAR).await.unwrap();
        let weapon = creation::rookie_ship(&db, module, owner, ship, FLAG_HI_SLOT_FIRST).await.unwrap();
        pending_reloads::insert(&db, owner, ship, weapon, FLAG_HI_SLOT_FIRST, charge, 1, FLAG_CARGO, source).await.unwrap();
        roots.push(ship);
    }
    let before = loading::capacity_snapshot(&db, source).await.unwrap();
    let room = hold_storage::container_room(&client.sde, before.parent.as_ref().unwrap(), &before.items, &before.reloads).unwrap().unwrap();
    assert_eq!(room.capacity, 0.0);
    assert!(room.used > 0.0);
    assert_eq!(before.reloads.len(), 2);
    let (notify, mut events) = tokio::sync::mpsc::unbounded_channel();
    client.notify_tx = notify;
    let config = crate::config::tests::test_database();
    let (mut holder, connection) = tokio_postgres::connect(&config.url, tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let tx = holder.transaction().await.unwrap();
    let holder_pid = crate::dao::items::parent_locks::tests::backend_pid(&tx).await;
    tx.query_one("SELECT item_id FROM game.items WHERE item_id=$1 FOR UPDATE", &[&source]).await.unwrap();
    let selected = roots.clone();
    let operation = tokio::spawn(async move {
        let result = super::scenarios::many(&mut client, station, &selected, Some(Value::Bool(true))).await;
        (result, client)
    });
    crate::dao::items::parent_locks::tests::wait_blocked(&db, holder_pid).await;
    let hull_probe = db.query_one("SELECT item_id FROM game.items WHERE item_id=$1 FOR UPDATE NOWAIT", &[&source_hull]).await;
    let mut pending_probes = Vec::new();
    for pending in &before.reloads {
        pending_probes.push(db.query_one("SELECT reload_id FROM game.pending_reloads WHERE reload_id=$1 FOR UPDATE NOWAIT", &[&pending.reload_id]).await);
    }
    tx.execute("UPDATE game.items SET owner_id=$2 WHERE item_id=$1", &[&source, &(-owner)]).await.unwrap();
    tx.commit().await.unwrap();
    let (result, mut client) = operation.await.unwrap();
    assert!(matches!(hull_probe, Err(crate::db::Error::Postgres(error)) if error.code() == Some(&tokio_postgres::error::SqlState::LOCK_NOT_AVAILABLE)));
    assert!(pending_probes.iter().all(Result::is_ok));
    assert!(result.is_err());
    assert!(events.try_recv().is_err());
    assert!(client.outbox.is_empty());
    for &root in &roots {
        assert_eq!(pending_reloads::for_ship(&db, root).await.unwrap().len(), 1);
        assert!(db.query_one("SELECT singleton FROM game.items WHERE item_id=$1", &[&root]).await.unwrap().get::<_, bool>("singleton"));
    }
    db.execute("UPDATE game.items SET owner_id=$2 WHERE item_id=$1", &[&source, &owner]).await.unwrap();
    let unchanged = fingerprint(&db).await;
    for field in ["location_id", "owner_id"] {
        let (mut holder, connection) = tokio_postgres::connect(&config.url, tokio_postgres::NoTls).await.unwrap();
        tokio::spawn(async move { connection.await.unwrap(); });
        let tx = holder.transaction().await.unwrap();
        let holder_pid = crate::dao::items::parent_locks::tests::backend_pid(&tx).await;
        tx.query_one("SELECT item_id FROM game.items WHERE item_id=$1 FOR UPDATE", &[&source_hull]).await.unwrap();
        let selected = roots.clone();
        let operation = tokio::spawn(async move {
            let result = super::scenarios::many(&mut client, station, &selected, Some(Value::Bool(true))).await;
            (result, client)
        });
        crate::dao::items::parent_locks::tests::wait_blocked(&db, holder_pid).await;
        let old = if field == "location_id" { station } else { owner };
        tx.execute(&format!("UPDATE game.items SET {field}=$2 WHERE item_id=$1"), &[&source_hull, &(-old)]).await.unwrap();
        tx.commit().await.unwrap();
        let (result, returned_client) = operation.await.unwrap();
        client = returned_client;
        db.execute(&format!("UPDATE game.items SET {field}=$2 WHERE item_id=$1"), &[&source_hull, &old]).await.unwrap();
        assert!(result.is_err());
        assert_eq!(fingerprint(&db).await, unchanged);
        assert!(events.try_recv().is_err());
        assert!(client.outbox.is_empty());
    }
    assert!(matches!(super::scenarios::many(&mut client, station, &roots, Some(Value::Bool(true))).await, Ok(Value::None)));
    let after = loading::capacity_snapshot(&db, source).await.unwrap();
    let after_room = hold_storage::container_room(&client.sde, after.parent.as_ref().unwrap(), &after.items, &after.reloads).unwrap().unwrap();
    assert_eq!(room.used, after_room.used);
    assert!(after.reloads.is_empty());
    assert_eq!(after.items.iter().filter(|item| item.type_id == charge).map(|item| i128::from(item.quantity)).sum::<i128>(), 2);
    let notices: Vec<_> = std::iter::from_fn(|| events.try_recv().ok()).collect();
    for root in roots {
        assert!(pending_reloads::for_ship(&db, root).await.unwrap().is_empty());
        let row = db.query_one("SELECT location_id,flag_id,singleton FROM game.items WHERE item_id=$1", &[&root]).await.unwrap();
        assert_eq!(row.get::<_, i64>("location_id"), station);
        assert_eq!(row.get::<_, i32>("flag_id"), FLAG_HANGAR);
        assert!(!row.get::<_, bool>("singleton"));
        let item = crate::services::items::load_items(&db, &client.sde, loading::ItemQuery::Id(root)).await.unwrap().pop().unwrap();
        assert!(notices.iter().any(|(name, args)| name == "OnItemChange"
            && args.first() == Some(&item.row())
            && args.get(1) == Some(&Value::Dict(vec![(Value::Int(infinity_core::consts::IX_SINGLETON), Value::Bool(true))]))));
    }
    let charge_volume = crate::services::item_volume::item_volume(&client.sde, charge, false, 1, None).unwrap();
    let fallback_hull = client.sde.types.iter().find(|ty| ty.category_id == Some(CATEGORY_SHIP)
        && ty.group_id.map(i64::from) != Some(GROUP_STRATEGIC_CRUISER)
        && ty.fit_attribute(infinity_core::consts::dogma::ATTR_CAPACITY).is_some_and(|capacity| capacity >= charge_volume))
        .expect("ship with enough cargo for one charge").type_id;
    let fallback = creation::rookie_ship(&db, fallback_hull, owner, station, FLAG_HANGAR).await.unwrap();
    let fallback_weapon = creation::rookie_ship(&db, module, owner, fallback, FLAG_HI_SLOT_FIRST).await.unwrap();
    pending_reloads::insert(&db, owner, fallback, fallback_weapon, FLAG_HI_SLOT_FIRST, charge, 1, FLAG_CARGO, source).await.unwrap();
    db.execute("UPDATE game.items SET owner_id=$2 WHERE item_id=$1", &[&source, &(-owner)]).await.unwrap();
    let source_before: String = db.query_one("SELECT string_agg(row_to_json(i)::text, '' ORDER BY item_id) AS value FROM game.items i WHERE item_id=$1 OR location_id=$1", &[&source]).await.unwrap().get("value");
    assert!(matches!(super::scenarios::many(&mut client, station, &[fallback], Some(Value::Bool(true))).await, Ok(Value::None)));
    let source_after: String = db.query_one("SELECT string_agg(row_to_json(i)::text, '' ORDER BY item_id) AS value FROM game.items i WHERE item_id=$1 OR location_id=$1", &[&source]).await.unwrap().get("value");
    assert_eq!(source_before, source_after);
    assert!(pending_reloads::for_ship(&db, fallback).await.unwrap().is_empty());
    let output = db.query("SELECT item_id,quantity FROM game.items WHERE owner_id=$1 AND location_id=$2 AND flag_id=$3 AND type_id=$4 AND NOT singleton", &[&owner, &station, &FLAG_HANGAR, &charge]).await.unwrap();
    assert_eq!(output.len(), 1);
    assert_eq!(output[0].get::<_, i64>("quantity"), 1);
    let output_id: i64 = output[0].get("item_id");
    let item = crate::services::items::load_items(&db, &client.sde, loading::ItemQuery::Id(output_id)).await.unwrap().pop().unwrap();
    let fallback_notices: Vec<_> = std::iter::from_fn(|| events.try_recv().ok()).collect();
    assert!(fallback_notices.iter().any(|(name, args)| name == "OnItemChange" && args.first() == Some(&item.row())));
    assert!(client.outbox.is_empty());
    db.execute("UPDATE game.items SET owner_id=$2 WHERE item_id=$1", &[&source, &owner]).await.unwrap();
    }).await;
    db.execute("UPDATE game.items SET owner_id=$1 WHERE owner_id=-($1::bigint)", &[&fixture.character]).await.unwrap();
    db.execute("DELETE FROM game.pending_reloads WHERE owner_id=$1", &[&fixture.character]).await.unwrap();
    crate::services::tests::fixture::cleanup(&db, fixture).await.unwrap();
    result.unwrap();
}

async fn pending_order_and_unload(overflow: bool) {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let fixture = crate::services::tests::fixture::create(&db).await.unwrap();
    let worker_db = db.clone();
    let result = tokio::spawn(async move {
        let (mut client, _peer) = crate::services::tests::client(worker_db.clone(), fixture.character).await;
        let station = client.sde.map.stations().next().unwrap().station_id;
        client.session.apply(vec![("stationid", Value::Int(station)), ("shipid", Value::Int(0))]);
        let hull = client.sde.types.iter().find(|ty| ty.category_id == Some(CATEGORY_SHIP)
            && ty.group_id.map(i64::from) != Some(GROUP_STRATEGIC_CRUISER)).unwrap().type_id;
        let module = client.sde.types.iter().find(|ty| ty.category_id == Some(CATEGORY_MODULE)).unwrap().type_id;
        let charge = client.sde.types.iter().find(|ty| ty.category_id == Some(CATEGORY_CHARGE)
            && ty.volume.is_some_and(|v| v.is_finite() && v > 0.0)).unwrap().type_id;
        let box_type = client.sde.types.iter().find(|ty| hold_storage::is_ordinary_container(&client.sde, ty.type_id)
            && hold_storage::static_capacity(&client.sde, ty.type_id).is_ok_and(|cap| cap == 0.0)).unwrap().type_id;
        let owner = fixture.character;
        let source_hull = creation::rookie_ship(&worker_db, hull, owner, station, FLAG_HANGAR).await.unwrap();
        let source = creation::rookie_ship(&worker_db, box_type, owner, source_hull, FLAG_CARGO).await.unwrap();
        let mut roots = Vec::new();
        let mut weapons = Vec::new();
        for _ in 0..2 {
            let root = creation::rookie_ship(&worker_db, hull, owner, station, FLAG_HANGAR).await.unwrap();
            let weapon = creation::rookie_ship(&worker_db, module, owner, root, FLAG_HI_SLOT_FIRST).await.unwrap();
            roots.push(root);
            weapons.push(weapon);
        }
        // Reverse creation relative to root processing order.
        let low = pending_reloads::insert(&worker_db, owner, roots[1], weapons[1], FLAG_HI_SLOT_FIRST, charge, 1, FLAG_CARGO, source).await.unwrap();
        let high = pending_reloads::insert(&worker_db, owner, roots[0], weapons[0], FLAG_HI_SLOT_FIRST, charge, 1, FLAG_CARGO, source).await.unwrap();
        assert!(roots[0] < roots[1] && low < high);
        if overflow {
            crate::dao::items::transfers::insert(&worker_db, charge, owner, station, FLAG_HANGAR, i64::MAX - 1).await.unwrap();
            for (&root, &weapon) in roots.iter().zip(&weapons) {
                crate::dao::items::combat::apply_loads(&worker_db, root, &[(weapon, FLAG_HI_SLOT_FIRST, charge, 1, 0)]).await.unwrap();
            }
            assert!(matches!(super::scenarios::many(&mut client, station, &roots, Some(Value::Bool(true))).await, Ok(Value::None)));
            let stacks = loading::load_items(&worker_db, loading::ItemQuery::Location(station)).await.unwrap();
            let quantities: Vec<_> = stacks.iter().filter(|item| item.owner_id == owner && item.flag_id == FLAG_HANGAR && item.type_id == charge && !item.singleton).map(|item| item.quantity).collect();
            assert_eq!(quantities.len(), 2);
            assert_eq!(quantities.iter().map(|&quantity| i128::from(quantity)).sum::<i128>(), i128::from(i64::MAX) + 1);
        } else {
            let before = fingerprint(&worker_db).await;
            let config = crate::config::tests::test_database();
            let (mut holder, connection) = tokio_postgres::connect(&config.url, tokio_postgres::NoTls).await.unwrap();
            tokio::spawn(async move { connection.await.unwrap(); });
            let tx = holder.transaction().await.unwrap();
            let pid = crate::dao::items::parent_locks::tests::backend_pid(&tx).await;
            tx.query_one("SELECT reload_id FROM game.pending_reloads WHERE reload_id=$1 FOR UPDATE", &[&high]).await.unwrap();
            let selected = roots.clone();
            let operation = tokio::spawn(async move {
                let result = super::scenarios::many(&mut client, station, &selected, Some(Value::Bool(true))).await;
                (result, client)
            });
            crate::dao::items::parent_locks::tests::wait_blocked(&worker_db, pid).await;
            let probe = worker_db.query_one("SELECT reload_id FROM game.pending_reloads WHERE reload_id=$1 FOR UPDATE NOWAIT", &[&low]).await;
            let unwritten = fingerprint(&worker_db).await;
            tx.commit().await.unwrap();
            let (outcome, client) = operation.await.unwrap();
            match probe {
                Err(crate::db::Error::Postgres(error)) => assert_eq!(error.code(), Some(&tokio_postgres::error::SqlState::LOCK_NOT_AVAILABLE)),
                _ => panic!("lower reservation must already be locked"),
            }
            assert_eq!(unwritten, before);
            assert!(matches!(outcome, Ok(Value::None)));
            let after = loading::capacity_snapshot(&worker_db, source).await.unwrap();
            assert!(after.reloads.is_empty());
            assert_eq!(after.items.iter().filter(|item| item.type_id == charge).map(|item| i128::from(item.quantity)).sum::<i128>(), 2);
            for root in roots {
                assert!(pending_reloads::for_ship(&worker_db, root).await.unwrap().is_empty());
                let row = worker_db.query_one("SELECT location_id,flag_id,singleton FROM game.items WHERE item_id=$1", &[&root]).await.unwrap();
                assert_eq!(row.get::<_, i64>("location_id"), station);
                assert_eq!(row.get::<_, i32>("flag_id"), FLAG_HANGAR);
                assert!(!row.get::<_, bool>("singleton"));
            }
            assert!(client.outbox.is_empty());
        }
    }).await;
    db.execute("DELETE FROM game.pending_reloads WHERE owner_id=$1", &[&fixture.character]).await.unwrap();
    db.execute("DELETE FROM game.module_charges WHERE ship_id IN (SELECT item_id FROM game.items WHERE owner_id=$1)", &[&fixture.character]).await.unwrap();
    crate::services::tests::fixture::cleanup(&db, fixture).await.unwrap();
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database with SDE"]
async fn multi_root_pending_locks_precede_writes_in_global_order() {
    pending_order_and_unload(false).await;
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database with SDE"]
async fn multi_root_station_unload_spills_to_new_stack() {
    pending_order_and_unload(true).await;
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database with SDE"]
async fn source_that_is_also_refund_candidate_remains_mandatory() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let fixture = crate::services::tests::fixture::create(&db).await.unwrap();
    let worker_db = db.clone();
    let result = tokio::spawn(async move {
        let (mut client, _peer) = crate::services::tests::client(worker_db.clone(), fixture.character).await;
        let station = client.sde.map.stations().next().unwrap().station_id;
        client.session.apply(vec![("stationid", Value::Int(station)), ("shipid", Value::Int(0))]);
        let hull = client.sde.types.iter().find(|ty| ty.category_id == Some(CATEGORY_SHIP)
            && ty.group_id.map(i64::from) != Some(GROUP_STRATEGIC_CRUISER)).unwrap().type_id;
        let module = client.sde.types.iter().find(|ty| ty.category_id == Some(CATEGORY_MODULE)).unwrap().type_id;
        let charge = client.sde.types.iter().find(|ty| ty.category_id == Some(CATEGORY_CHARGE)
            && ty.volume.is_some_and(|v| v.is_finite() && v > 0.0)).unwrap().type_id;
        let box_type = client.sde.types.iter().find(|ty| hold_storage::is_ordinary_container(&client.sde, ty.type_id)
            && hold_storage::static_capacity(&client.sde, ty.type_id).is_ok_and(|cap| cap == 0.0)
            && crate::services::item_volume::item_volume(&client.sde, ty.type_id, false, 1, None).is_ok_and(|v| v.is_finite() && v > 0.0))
            .expect("ordinary zero-capacity type with positive packaged volume").type_id;
        let owner = fixture.character;
        let source_hull = creation::rookie_ship(&worker_db, hull, owner, station, FLAG_HANGAR).await.unwrap();
        let outer = creation::rookie_ship(&worker_db, box_type, owner, source_hull, FLAG_CARGO).await.unwrap();
        // Legacy backend state: a stackable ordinary container is both a source and a merge candidate.
        let inner = crate::dao::items::transfers::insert(&worker_db, box_type, owner, outer, FLAG_CARGO, 1).await.unwrap();
        let first = creation::rookie_ship(&worker_db, hull, owner, station, FLAG_HANGAR).await.unwrap();
        let first_weapon = creation::rookie_ship(&worker_db, module, owner, first, FLAG_HI_SLOT_FIRST).await.unwrap();
        let second = creation::rookie_ship(&worker_db, hull, owner, station, FLAG_HANGAR).await.unwrap();
        let second_weapon = creation::rookie_ship(&worker_db, module, owner, second, FLAG_HI_SLOT_FIRST).await.unwrap();
        pending_reloads::insert(&worker_db, owner, first, first_weapon, FLAG_HI_SLOT_FIRST, box_type, 1, FLAG_CARGO, outer).await.unwrap();
        pending_reloads::insert(&worker_db, owner, second, second_weapon, FLAG_HI_SLOT_FIRST, charge, 1, FLAG_CARGO, inner).await.unwrap();
        let mut pending = pending_reloads::for_ship(&worker_db, first).await.unwrap();
        pending.extend(pending_reloads::for_ship(&worker_db, second).await.unwrap());
        let preparation = crate::services::combat::reload_recovery::RefundPreparation::capture_pending(&worker_db, &client.sde, &pending).await.unwrap();
        assert!(preparation.candidate_ids().contains(&inner));
        assert!(preparation.mandatory_ids().contains(&inner));
        assert!(preparation.requests().iter().any(|request| request.item_id == inner
            && request.strength == crate::dao::items::parent_locks::LockStrength::Update));
        let before = fingerprint(&worker_db).await;
        let (notify, mut events) = tokio::sync::mpsc::unbounded_channel();
        client.notify_tx = notify;
        let config = crate::config::tests::test_database();
        let (mut holder, connection) = tokio_postgres::connect(&config.url, tokio_postgres::NoTls).await.unwrap();
        tokio::spawn(async move { connection.await.unwrap(); });
        let tx = holder.transaction().await.unwrap();
        let pid = crate::dao::items::parent_locks::tests::backend_pid(&tx).await;
        tx.query_one("SELECT item_id FROM game.items WHERE item_id=$1 FOR UPDATE", &[&inner]).await.unwrap();
        let operation = tokio::spawn(async move {
            let outcome = super::scenarios::many(&mut client, station, &[first, second], Some(Value::Bool(true))).await;
            (outcome, client)
        });
        crate::dao::items::parent_locks::tests::wait_blocked(&worker_db, pid).await;
        tx.execute("UPDATE game.items SET owner_id=$2 WHERE item_id=$1", &[&inner, &(-owner)]).await.unwrap();
        tx.commit().await.unwrap();
        let (outcome, mut client) = operation.await.unwrap();
        worker_db.execute("UPDATE game.items SET owner_id=$2 WHERE item_id=$1", &[&inner, &owner]).await.unwrap();
        assert!(outcome.is_err());
        assert_eq!(fingerprint(&worker_db).await, before);
        assert!(events.try_recv().is_err());
        assert!(client.outbox.is_empty());
        assert!(matches!(super::scenarios::many(&mut client, station, &[first, second], Some(Value::Bool(true))).await, Ok(Value::None)));
        let inner_row = worker_db.query_one("SELECT quantity,owner_id,location_id FROM game.items WHERE item_id=$1", &[&inner]).await.unwrap();
        assert_eq!(inner_row.get::<_, i64>("quantity"), 2);
        assert_eq!(inner_row.get::<_, i64>("owner_id"), owner);
        assert_eq!(inner_row.get::<_, i64>("location_id"), outer);
        let inner_contents = loading::capacity_snapshot(&worker_db, inner).await.unwrap();
        assert!(inner_contents.reloads.is_empty());
        assert_eq!(inner_contents.items.iter().filter(|item| item.type_id == charge).map(|item| i128::from(item.quantity)).sum::<i128>(), 1);
        assert!(pending_reloads::for_ship(&worker_db, first).await.unwrap().is_empty());
        assert!(pending_reloads::for_ship(&worker_db, second).await.unwrap().is_empty());
        let notices: Vec<_> = std::iter::from_fn(|| events.try_recv().ok()).collect();
        for id in [first, second, first_weapon, second_weapon] {
            let item = crate::services::items::load_items(&worker_db, &client.sde, loading::ItemQuery::Id(id)).await.unwrap().pop().unwrap();
            assert_eq!((item.owner_id, item.location_id, item.flag_id), (owner, station, FLAG_HANGAR));
            assert!(notices.iter().any(|(name, args)| name == "OnItemChange" && args.first() == Some(&item.row())));
        }
        assert!(client.outbox.is_empty());
    }).await;
    db.execute("UPDATE game.items SET owner_id=$1 WHERE owner_id=-($1::bigint)", &[&fixture.character]).await.unwrap();
    db.execute("DELETE FROM game.pending_reloads WHERE owner_id=$1", &[&fixture.character]).await.unwrap();
    crate::services::tests::fixture::cleanup(&db, fixture).await.unwrap();
    result.unwrap();
}
