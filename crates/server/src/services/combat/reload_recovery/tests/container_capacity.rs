//! Refund admission against real loaded static data and transactional reservations.

use super::*;
use crate::services::inventory::hold_storage::{self, Addition, Replaced};

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated database with SDE"]
async fn mixed_source_refunds_are_one_atomic_group() {
    crate::tests::init_config();
    let config = crate::config::tests::test_database();
    let db = Db::connect(&config).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let (charge, hull) = sde.types.iter().filter(|ty| ty.category_id == Some(CATEGORY_CHARGE)
        && ty.volume.is_some_and(|v| v.is_finite() && v > 0.0)).find_map(|charge| {
        let volume = charge.volume.unwrap();
        sde.types.iter().find(|ty| ty.category_id == Some(CATEGORY_SHIP)
            && ty.capacity.is_some_and(|cap| cap >= volume * 2.0 && cap < volume * f64::from(i32::MAX - 1)))
            .map(|hull| (charge.type_id, hull.type_id))
    }).unwrap();
    let (mut client, connection) = tokio_postgres::connect(&config.url, tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let tx = client.transaction().await.unwrap();
    let owner = 101_i64;
    // Every iteration owns a separate hull and rolls back with the enclosing transaction.
    for (a, b, b_quantity, succeeds) in [
        (true, false, 0, true), (false, true, 1, true),
        (false, true, i64::from(i32::MAX) - 1, false), (true, true, 1, true),
        (true, true, i64::from(i32::MAX) - 1, false),
    ] {
        let ship = crate::dao::items::creation::rookie_ship(&tx, hull, owner, 1, 0).await.unwrap();
        let magazine_flag = infinity_core::consts::FLAG_FITTING_FIRST;
        crate::dao::items::combat::apply_loads(&tx, ship, &[(1, magazine_flag, charge, 1, 0)]).await.unwrap();
        let magazine_before = crate::dao::items::charges::loaded(&tx, ship, magazine_flag).await.unwrap().unwrap();
        let mut ids = Vec::new();
        if a { ids.push(pending_reloads::insert(&tx, owner, ship, 1, 1, charge, 1, FLAG_CARGO, ship).await.unwrap()); }
        if b { ids.push(pending_reloads::insert(&tx, owner, ship, 2, 2, charge, b_quantity, FLAG_CARGO, i64::MAX).await.unwrap()); }
        let before = pending_reloads::for_ship(&tx, ship).await.unwrap();
        let before_items = loading::load_items(&tx, ItemQuery::Location(ship)).await.unwrap();
        let mut refund = RefundPreparation::capture_pending(&tx, &sde, &before).await.unwrap();
        let mut requests = vec![LockRequest { item_id: ship, strength: LockStrength::Update }];
        requests.extend_from_slice(refund.requests());
        let mandatory = refund.mandatory_ids();
        let optional: Vec<_> = refund.candidate_ids().into_iter().filter(|id| *id != ship && !mandatory.contains(id)).collect();
        let locks = crate::services::inventory::destination::lock_items(&tx, &sde, &requests, &optional).await.unwrap();
        let mut outbox: Outbox = Outbox::default();
        let result = return_pending(&tx, &sde, &mut outbox, &ids, true, &locks, &mut refund).await;
        drop(locks);
        let after = pending_reloads::for_ship(&tx, ship).await.unwrap();
        let items = loading::load_items(&tx, ItemQuery::Location(ship)).await.unwrap();
        let magazine_after = crate::dao::items::charges::loaded(&tx, ship, magazine_flag).await.unwrap().unwrap();
        assert_eq!((magazine_after.type_id, magazine_after.quantity), (magazine_before.type_id, magazine_before.quantity));
        let (effects, events) = crate::db::outbox::tests::split(outbox);
        assert!(events.is_empty());
        if succeeds {
            let receipts = result.unwrap();
            assert_eq!(receipts.len(), 1);
            assert!(receipts[0].created.is_some());
            assert_eq!(receipts[0].last().quantity, i64::from(a) + b_quantity);
            assert!(after.is_empty());
            assert_eq!(items.len(), before_items.len() + 1);
            assert_eq!(items[0].item_id, receipts[0].last().item_id);
            assert_eq!(items[0].quantity, i64::from(a) + b_quantity);
            assert_eq!((items[0].owner_id, items[0].location_id, items[0].flag_id, items[0].type_id),
                (owner, ship, FLAG_CARGO, charge));
            assert!(matches!(&effects[0], crate::db::outbox::Effect::Notification(
                crate::db::outbox::Notification::DirectClient { event, .. }) if event == "OnItemChange"));
            assert_eq!(effects.len(), 1);
        } else {
            assert!(result.is_err());
            assert!(before == after);
            assert_eq!(items.len(), before_items.len());
            assert!(effects.is_empty());
        }
    }
    // The checker itself must reject an actual reservation larger than the incoming stack.
    let ship = crate::dao::items::creation::rookie_ship(&tx, hull, owner, 1, 0).await.unwrap();
    let id = pending_reloads::insert(&tx, owner, ship, 1, 1, charge, 2, FLAG_CARGO, ship).await.unwrap();
    let requests = [LockRequest { item_id: ship, strength: LockStrength::Update }];
    let locks = crate::services::inventory::destination::lock_items(&tx, &sde, &requests, &[]).await.unwrap();
    let pending = pending_reloads::lock_ids(&tx, &[id]).await.unwrap();
    assert_eq!(pending.len(), 1);
    let addition = [Addition { type_id: charge, singleton: false, quantity: 1 }];
    assert!(hold_storage::check_destination(&tx, &sde, owner, ship, FLAG_CARGO, &addition,
        Replaced { items: &[], reloads: &[id] }, None).await.is_err());
    drop(locks);
    assert!(pending == pending_reloads::for_ship(&tx, ship).await.unwrap());
    assert!(loading::load_items(&tx, ItemQuery::Location(ship)).await.unwrap().is_empty());
    tx.rollback().await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated database with SDE"]
async fn refund_rejects_duplicate_and_changed_facts_but_skips_completed() {
    crate::tests::init_config();
    let config = crate::config::tests::test_database();
    let db = Db::connect(&config).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let charge = sde.types.iter().find(|ty| ty.category_id == Some(CATEGORY_CHARGE)
        && ty.volume.is_some_and(|v| v > 0.0)).unwrap();
    let hull = sde.types.iter().find(|ty| ty.category_id == Some(CATEGORY_SHIP)
        && ty.capacity.is_some_and(|v| v >= charge.volume.unwrap() * 2.0)).unwrap().type_id;
    let (mut client, connection) = tokio_postgres::connect(&config.url, tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let tx = client.transaction().await.unwrap();
    let ship = crate::dao::items::creation::rookie_ship(&tx, hull, 101, 1, 0).await.unwrap();
    let id = pending_reloads::insert(&tx, 101, ship, 1, 1, charge.type_id, 1, FLAG_CARGO, ship).await.unwrap();
    let pending = pending_reloads::for_ship(&tx, ship).await.unwrap();
    let mut refund = RefundPreparation::capture_pending(&tx, &sde, &pending).await.unwrap();
    let requests = [LockRequest { item_id: ship, strength: LockStrength::Update }];
    let locks = crate::services::inventory::destination::lock_items(&tx, &sde, &requests, &[]).await.unwrap();
    let mut outbox: Outbox = Outbox::default();
    assert!(return_pending(&tx, &sde, &mut outbox, &[id, id], true, &locks, &mut refund).await.is_err());
    for field in ["owner_id", "ship_id", "module_id", "flag_id", "type_id", "quantity", "source_flag", "source_location_id"] {
        let change = format!("UPDATE game.pending_reloads SET {field} = {field} + 1 WHERE reload_id = $1");
        let restore = format!("UPDATE game.pending_reloads SET {field} = {field} - 1 WHERE reload_id = $1");
        tx.execute(&change, &[&id]).await.unwrap();
        assert!(return_pending(&tx, &sde, &mut outbox, &[id], true, &locks, &mut refund).await.is_err(), "{field}");
        tx.execute(&restore, &[&id]).await.unwrap();
    }
    pending_reloads::take(&tx, id).await.unwrap();
    assert!(return_pending(&tx, &sde, &mut outbox, &[id], true, &locks, &mut refund).await.unwrap().is_empty());
    let (effects, events) = crate::db::outbox::tests::split(outbox);
    assert!(effects.is_empty() && events.is_empty());
    assert!(loading::load_items(&tx, ItemQuery::Location(ship)).await.unwrap().is_empty());
    drop(locks);
    tx.rollback().await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated database with SDE"]
async fn overfull_box_refunds_reservation_and_missing_ship_discards_under_source_fence() {
    crate::tests::init_config();
    let config = crate::config::tests::test_database();
    let db = Db::connect(&config).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let charge = sde.types.iter().find(|ty| ty.category_id == Some(CATEGORY_CHARGE)
        && ty.volume.is_some_and(|v| v.is_finite() && v > 0.0)).unwrap();
    let hull = sde.types.iter().find(|ty| ty.category_id == Some(CATEGORY_SHIP)).unwrap().type_id;
    let box_type = sde.types.iter().find(|ty| hold_storage::is_ordinary_container(&sde, ty.type_id)
        && hold_storage::static_capacity(&sde, ty.type_id).is_ok_and(|cap|
            cap > 0.0 && cap / charge.volume.unwrap() < f64::from(i32::MAX - 2))).unwrap().type_id;
    let quantity = (hold_storage::static_capacity(&sde, box_type).unwrap() / charge.volume.unwrap()).floor() as i64 + 1;
    let (mut client, connection) = tokio_postgres::connect(&config.url, tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let tx = client.transaction().await.unwrap();
    let ship = crate::dao::items::creation::rookie_ship(&tx, hull, 101, 1, 0).await.unwrap();
    let source = crate::dao::items::creation::rookie_ship(&tx, box_type, 101, ship, FLAG_CARGO).await.unwrap();
    let id = pending_reloads::insert(&tx, 101, ship, 1, 1, charge.type_id, quantity, FLAG_CARGO, source).await.unwrap();
    let pending = pending_reloads::for_ship(&tx, ship).await.unwrap();
    let snapshot = loading::capacity_snapshot(&tx, source).await.unwrap();
    let room = hold_storage::container_room(&sde, snapshot.parent.as_ref().unwrap(), &snapshot.items, &snapshot.reloads).unwrap().unwrap();
    assert!(room.used > room.capacity);
    let mut refund = RefundPreparation::capture_pending(&tx, &sde, &pending).await.unwrap();
    assert!(refund.requests().iter().any(|r| r.item_id == source && r.strength == LockStrength::Update));
    assert!(refund.requests().iter().any(|r| r.item_id == ship && r.strength == LockStrength::Update));
    let locks = crate::services::inventory::destination::lock_items(&tx, &sde, refund.requests(), &refund.candidate_ids()).await.unwrap();
    let mut outbox: Outbox = Outbox::default();
    let receipts = return_pending(&tx, &sde, &mut outbox, &[id], true, &locks, &mut refund).await.unwrap();
    assert_eq!(receipts.len(), 1);
    assert_eq!(receipts[0].last().quantity, quantity);
    drop(locks);
    let snapshot = loading::capacity_snapshot(&tx, source).await.unwrap();
    let after = hold_storage::container_room(&sde, snapshot.parent.as_ref().unwrap(), &snapshot.items, &snapshot.reloads).unwrap().unwrap();
    assert_eq!(room.used, after.used);
    assert!(snapshot.reloads.is_empty());
    let (effects, events) = crate::db::outbox::tests::split(outbox);
    assert_eq!(effects.len(), 1);
    assert!(events.is_empty());

    let missing_ship = -source;
    let lost = pending_reloads::insert(&tx, 101, missing_ship, 2, 2, charge.type_id, 1, FLAG_CARGO, source).await.unwrap();
    let pending: Vec<_> = pending_reloads::for_ship(&tx, missing_ship).await.unwrap().into_iter().filter(|row| row.reload_id == lost).collect();
    let mut discard = RefundPreparation::discard(&tx, &sde, &pending).await.unwrap();
    assert!(discard.requests().iter().any(|r| r.item_id == source && r.strength == LockStrength::Update));
    assert!(discard.requests().iter().any(|r| r.item_id == ship && r.strength == LockStrength::Update));
    let locks = crate::services::inventory::destination::lock_items(&tx, &sde, discard.requests(), &[]).await.unwrap();
    let mut outbox: Outbox = Outbox::default();
    assert!(return_pending(&tx, &sde, &mut outbox, &[lost], false, &locks, &mut discard).await.unwrap().is_empty());
    assert!(pending_reloads::for_ship(&tx, missing_ship).await.unwrap().is_empty());
    let (effects, events) = crate::db::outbox::tests::split(outbox);
    assert!(effects.is_empty() && events.is_empty());
    drop(locks);
    let items = loading::load_items(&tx, ItemQuery::Location(source)).await.unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].quantity, quantity);
    tx.rollback().await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated database with SDE"]
async fn changed_source_owner_falls_back_without_credit_and_keeps_source_fenced() {
    crate::tests::init_config();
    let config = crate::config::tests::test_database();
    let db = Db::connect(&config).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let charge = sde.types.iter().find(|ty| ty.category_id == Some(CATEGORY_CHARGE)
        && ty.volume.is_some_and(|v| v > 0.0)).unwrap();
    let hull = sde.types.iter().find(|ty| ty.category_id == Some(CATEGORY_SHIP)
        && ty.capacity.is_some_and(|v| v >= charge.volume.unwrap())).unwrap().type_id;
    let box_type = sde.types.iter().find(|ty| hold_storage::is_ordinary_container(&sde, ty.type_id)).unwrap().type_id;
    let (mut client, connection) = tokio_postgres::connect(&config.url, tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let tx = client.transaction().await.unwrap();
    let ship = crate::dao::items::creation::rookie_ship(&tx, hull, 101, 1, 0).await.unwrap();
    let other = crate::dao::items::creation::rookie_ship(&tx, hull, 102, 1, 0).await.unwrap();
    let source = crate::dao::items::creation::rookie_ship(&tx, box_type, 102, other, FLAG_CARGO).await.unwrap();
    let id = pending_reloads::insert(&tx, 101, ship, 1, 1, charge.type_id, 1, FLAG_CARGO, source).await.unwrap();
    let pending = pending_reloads::for_ship(&tx, ship).await.unwrap();
    let mut refund = RefundPreparation::capture_pending(&tx, &sde, &pending).await.unwrap();
    assert!(refund.requests().iter().any(|r| r.item_id == source && r.strength == LockStrength::Update));
    assert!(refund.requests().iter().any(|r| r.item_id == other && r.strength == LockStrength::Update));
    let mut requests = refund.requests().to_vec();
    requests.push(LockRequest { item_id: ship, strength: LockStrength::Update });
    let locks = crate::services::inventory::destination::lock_items(&tx, &sde, &requests, &[]).await.unwrap();
    let mut outbox: Outbox = Outbox::default();
    let receipts = return_pending(&tx, &sde, &mut outbox, &[id], true, &locks, &mut refund).await.unwrap();
    assert_eq!(receipts.len(), 1);
    assert_eq!((receipts[0].last().location_id, receipts[0].last().quantity), (ship, 1));
    assert!(pending_reloads::for_ship(&tx, ship).await.unwrap().is_empty());
    assert!(loading::load_items(&tx, ItemQuery::Location(source)).await.unwrap().is_empty());
    let (effects, events) = crate::db::outbox::tests::split(outbox);
    assert_eq!(effects.len(), 1);
    assert!(events.is_empty());
    drop(locks);
    tx.rollback().await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated database with SDE"]
async fn credited_refund_spills_into_new_stack() {
    crate::tests::init_config();
    let config = crate::config::tests::test_database();
    let db = Db::connect(&config).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let charge = sde.types.iter().find(|ty| ty.category_id == Some(CATEGORY_CHARGE)
        && ty.volume.is_some_and(|v| v.is_finite() && v > 0.0)).unwrap().type_id;
    let hull = sde.types.iter().find(|ty| ty.category_id == Some(CATEGORY_SHIP)).unwrap().type_id;
    let box_type = sde.types.iter().find(|ty| hold_storage::is_ordinary_container(&sde, ty.type_id)
        && hold_storage::static_capacity(&sde, ty.type_id).is_ok()).unwrap().type_id;
    let (mut client, connection) = tokio_postgres::connect(&config.url, tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let tx = client.transaction().await.unwrap();
    let ship = crate::dao::items::creation::rookie_ship(&tx, hull, 101, 1, 0).await.unwrap();
    let source = crate::dao::items::creation::rookie_ship(&tx, box_type, 101, ship, FLAG_CARGO).await.unwrap();
    tx.execute("INSERT INTO game.items (type_id, owner_id, location_id, flag_id, quantity, singleton) VALUES ($1, 101, $2, $3, $4, false)",
        &[&charge, &source, &FLAG_CARGO, &(i64::MAX - 3)]).await.unwrap();
    let a = pending_reloads::insert(&tx, 101, ship, 1, 1, charge, 4, FLAG_CARGO, source).await.unwrap();
    let b = pending_reloads::insert(&tx, 101, ship, 2, 2, charge, 6, FLAG_CARGO, source).await.unwrap();
    let pending = pending_reloads::for_ship(&tx, ship).await.unwrap();
    let mut refund = RefundPreparation::capture_pending(&tx, &sde, &pending).await.unwrap();
    let mandatory = refund.mandatory_ids();
    let optional: Vec<_> = refund.candidate_ids().into_iter().filter(|id| !mandatory.contains(id)).collect();
    let locks = crate::services::inventory::destination::lock_items(&tx, &sde, refund.requests(), &optional).await.unwrap();
    let mut outbox: Outbox = Outbox::default();
    let receipts = return_pending(&tx, &sde, &mut outbox, &[a, b], true, &locks, &mut refund).await.unwrap();
    assert_eq!(receipts.len(), 1);
    assert_eq!(receipts[0].merged.as_ref().unwrap().item.quantity, i64::MAX);
    assert_eq!(receipts[0].created.as_ref().unwrap().item.quantity, 7);
    assert!(pending_reloads::for_ship(&tx, ship).await.unwrap().is_empty());
    let (effects, events) = crate::db::outbox::tests::split(outbox);
    assert!(events.is_empty());
    assert_eq!(effects.len(), 2);
    for (sequence, effect) in effects.iter().enumerate() {
        let crate::db::outbox::Effect::Notification(crate::db::outbox::Notification::DirectClient { event, args }) = effect else { panic!("expected direct notification") };
        assert_eq!(event, "OnItemChange");
        let wire = marshal::dumps(&marshal::Value::tuple(args.clone()));
        let decoded = marshal::loads(&wire).unwrap();
        let fields = decoded.items().unwrap();
        let marshal::Value::DbRow { descriptor, data, objects } = &fields[0] else { panic!("expected item row") };
        let descriptor = marshal::DbRowDescriptor::from_value(descriptor).unwrap();
        let row = if sequence == 0 { receipts[0].merged.as_ref().unwrap() } else { receipts[0].created.as_ref().unwrap() };
        assert_eq!(descriptor.get(&fields[0], "itemID"), Some(marshal::Value::Int(row.item.item_id)));
        assert_eq!(descriptor.get(&fields[0], "quantity"), Some(marshal::Value::Int(row.item.quantity.min(infinity_core::consts::CLIENT_MAX_SHORT_AMOUNT))));
        let values: Vec<_> = descriptor.columns.iter().map(|(name, _)| {
            let value = descriptor.get(&fields[0], name).unwrap();
            match value {
                marshal::Value::None => serde_json::json!({"type":"none"}),
                marshal::Value::Int(value) => serde_json::json!({"type":"int","value":value.to_string()}),
                marshal::Value::Bool(value) => serde_json::json!({"type":"bool","value":value}),
                marshal::Value::Bytes(value) => serde_json::json!({"type":"str","hex":value.iter().map(|b| format!("{b:02x}")).collect::<String>()}),
                marshal::Value::Unicode(value) => serde_json::json!({"type":"unicode","value":value}),
                _ => panic!("unexpected item column type"),
            }
        }).collect();
        let object_values: Vec<_> = objects.iter().map(|value| match value {
            marshal::Value::None => serde_json::json!({"type":"none"}),
            marshal::Value::Bytes(value) => serde_json::json!({"type":"str","hex":value.iter().map(|b| format!("{b:02x}")).collect::<String>()}),
            marshal::Value::Unicode(value) => serde_json::json!({"type":"unicode","value":value}),
            _ => panic!("unexpected item object type"),
        }).collect();
        let change = match &fields[1] {
            marshal::Value::Dict(pairs) => pairs.iter().map(|(key, value)| {
                let (marshal::Value::Int(key), marshal::Value::Int(value)) = (key, value) else { panic!("unexpected change") };
                serde_json::json!([{"type":"int","value":key.to_string()},{"type":"int","value":value.to_string()}])
            }).collect::<Vec<_>>(),
            _ => panic!("expected change dict"),
        };
        println!("CLIENT_QUANTITY_INPUT_V1 {}", serde_json::json!({
            "version":1,"id":format!("refund-spill-{sequence}"),"scenario":"refund_spill",
            "producer_test":"credited_refund_spills_into_new_stack","producer_symbols":["return_pending","deposit_prepared"],
            "kind":"notification","sequence":sequence,"event":event,"recipient":{"kind":"direct_client"},
            "session":{"source":"fixture_context","value":null},"payload_scope":"notification_args",
            "wire_hex":wire.iter().map(|b| format!("{b:02x}")).collect::<String>(),"wire_size":wire.len(),
            "rows":[{"path":[0],"columns":descriptor.columns.iter().map(|(name, ty)| serde_json::json!([name,*ty as i32])).collect::<Vec<_>>(),
                "virtual_columns":descriptor.virtual_columns,"values":values,
                "fixed_hex":data.iter().map(|b| format!("{b:02x}")).collect::<String>(),"objects":object_values}],
            "change":change,"before":{"source":"fixture_context","item_id":receipts[0].merged.as_ref().unwrap().item.item_id.to_string(),"quantity":(i64::MAX-3).to_string()},
            "after":{"source":"server_output","item_id":row.item.item_id.to_string(),"quantity":row.item.quantity.to_string(),"old_quantity":row.old_quantity.map(|q| q.to_string()),"added_quantity":row.added_quantity.to_string()}
        }));
    }
    drop(locks);
    let a = pending_reloads::insert(&tx, 101, ship, 1, 1, charge, i64::MAX, FLAG_CARGO, source).await.unwrap();
    let b = pending_reloads::insert(&tx, 101, ship, 2, 2, charge, 1, FLAG_CARGO, source).await.unwrap();
    let pending = pending_reloads::for_ship(&tx, ship).await.unwrap();
    let mut refund = RefundPreparation::capture_pending(&tx, &sde, &pending).await.unwrap();
    let mandatory = refund.mandatory_ids();
    let optional: Vec<_> = refund.candidate_ids().into_iter().filter(|id| !mandatory.contains(id)).collect();
    let locks = crate::services::inventory::destination::lock_items(&tx, &sde, refund.requests(), &optional).await.unwrap();
    let mut outbox: Outbox = Outbox::default();
    assert!(matches!(return_pending(&tx, &sde, &mut outbox, &[a, b], true, &locks, &mut refund).await, Err(CallError::User { .. })));
    assert!(pending_reloads::for_ship(&tx, ship).await.unwrap() == pending);
    let (effects, events) = crate::db::outbox::tests::split(outbox);
    assert!(effects.is_empty() && events.is_empty());
    println!("CLIENT_QUANTITY_INPUT_V1 {}", serde_json::json!({
        "version":1,"id":"refund-refused","scenario":"refund_group_overflow",
        "producer_test":"credited_refund_spills_into_new_stack","producer_symbols":["return_pending"],
        "kind":"no_notifications","sequence":0,"recipient":{"kind":"direct_client"},
        "session":{"source":"fixture_context","value":null},"payload_scope":"outbox",
        "wire_hex":null,"wire_size":0,"rows":[],"outbox_len":effects.len(),
        "before":{"source":"fixture_context","pending_ids":[a.to_string(),b.to_string()],"quantities":[i64::MAX.to_string(),"1"]},
        "after":{"source":"server_output","pending_ids":pending.iter().map(|row| row.reload_id.to_string()).collect::<Vec<_>>()}
    }));
    drop(locks);
    tx.rollback().await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated database with SDE"]
async fn missing_ship_discard_waits_for_surviving_source_before_taking_reservation() {
    crate::tests::init_config();
    let config = crate::config::tests::test_database();
    let db = Db::connect(&config).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let charge = sde.types.iter().find(|ty| ty.category_id == Some(CATEGORY_CHARGE)
        && ty.volume.is_some_and(|v| v > 0.0)).unwrap().type_id;
    let hull = sde.types.iter().find(|ty| ty.category_id == Some(CATEGORY_SHIP)).unwrap().type_id;
    let box_type = sde.types.iter().find(|ty| hold_storage::is_ordinary_container(&sde, ty.type_id)).unwrap().type_id;
    let parent = crate::dao::items::creation::rookie_ship(&db, hull, 101, 1, 0).await.unwrap();
    let source = crate::dao::items::creation::rookie_ship(&db, box_type, 101, parent, FLAG_CARGO).await.unwrap();
    let missing_ship = -source;
    let id = pending_reloads::insert(&db, 101, missing_ship, 1, 1, charge, 1, FLAG_CARGO, source).await.unwrap();
    let (mut holder_client, connection) = tokio_postgres::connect(&config.url, tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let holder = holder_client.transaction().await.unwrap();
    let blocker = crate::dao::items::parent_locks::tests::backend_pid(&holder).await;
    let held = crate::services::inventory::destination::lock_items(&holder, &sde,
        &[LockRequest { item_id: source, strength: LockStrength::Update }], &[]).await.unwrap();
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let worker_sde = Arc::clone(&sde);
    let url = config.url.clone();
    let worker = tokio::spawn(async move {
        let (mut client, connection) = tokio_postgres::connect(&url, tokio_postgres::NoTls).await.unwrap();
        tokio::spawn(async move { connection.await.unwrap(); });
        let tx = client.transaction().await.unwrap();
        let pending = pending_reloads::for_ship(&tx, missing_ship).await.unwrap();
        let mut discard = RefundPreparation::discard(&tx, &worker_sde, &pending).await.unwrap();
        ready_tx.send(()).unwrap();
        let locks = crate::services::inventory::destination::lock_items(&tx, &worker_sde, discard.requests(), &[]).await.unwrap();
        let mut outbox: Outbox = Outbox::default();
        let receipts = return_pending(&tx, &worker_sde, &mut outbox, &[id], false, &locks, &mut discard).await.unwrap();
        assert!(receipts.is_empty());
        let (effects, events) = crate::db::outbox::tests::split(outbox);
        assert!(effects.is_empty() && events.is_empty());
        drop(locks);
        tx.commit().await.unwrap();
    });
    ready_rx.await.unwrap();
    crate::dao::items::parent_locks::tests::wait_blocked(&db, blocker).await;
    assert_eq!(pending_reloads::for_ship(&db, missing_ship).await.unwrap().len(), 1);
    drop(held);
    holder.rollback().await.unwrap();
    worker.await.unwrap();
    assert!(pending_reloads::for_ship(&db, missing_ship).await.unwrap().is_empty());
    assert!(loading::load_items(&db, ItemQuery::Location(source)).await.unwrap().is_empty());
    db.execute("DELETE FROM game.items WHERE item_id = ANY($1)", &[&&[source, parent][..]]).await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated database with SDE"]
async fn zero_capacity_box_accepts_its_own_reserved_ammunition() {
    crate::tests::init_config();
    let config = crate::config::tests::test_database();
    let db = Db::connect(&config).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let charge = sde.types.iter().find(|ty| ty.category_id == Some(CATEGORY_CHARGE)
        && ty.volume.is_some_and(|v| v > 0.0)).unwrap().type_id;
    let hull = sde.types.iter().find(|ty| ty.category_id == Some(CATEGORY_SHIP)).unwrap().type_id;
    let box_type = sde.types.iter().find(|ty| hold_storage::is_ordinary_container(&sde, ty.type_id)
        && hold_storage::static_capacity(&sde, ty.type_id).is_ok_and(|cap| cap == 0.0)).unwrap().type_id;
    let (mut client, connection) = tokio_postgres::connect(&config.url, tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let tx = client.transaction().await.unwrap();
    let ship = crate::dao::items::creation::rookie_ship(&tx, hull, 101, 1, 0).await.unwrap();
    let source = crate::dao::items::creation::rookie_ship(&tx, box_type, 101, ship, FLAG_CARGO).await.unwrap();
    let id = pending_reloads::insert(&tx, 101, ship, 1, 1, charge, 1, FLAG_CARGO, source).await.unwrap();
    let before = loading::capacity_snapshot(&tx, source).await.unwrap();
    let room = hold_storage::container_room(&sde, before.parent.as_ref().unwrap(), &before.items, &before.reloads).unwrap().unwrap();
    assert_eq!(room.capacity, 0.0);
    assert!(room.used > 0.0);
    let pending = pending_reloads::for_ship(&tx, ship).await.unwrap();
    let mut refund = RefundPreparation::capture_pending(&tx, &sde, &pending).await.unwrap();
    let locks = crate::services::inventory::destination::lock_items(&tx, &sde, refund.requests(), &[]).await.unwrap();
    let mut outbox: Outbox = Outbox::default();
    let receipts = return_pending(&tx, &sde, &mut outbox, &[id], true, &locks, &mut refund).await.unwrap();
    assert_eq!(receipts.len(), 1);
    assert_eq!((receipts[0].last().quantity, receipts[0].created.as_ref().unwrap().old_quantity), (1, None));
    drop(locks);
    let after = loading::capacity_snapshot(&tx, source).await.unwrap();
    let after_room = hold_storage::container_room(&sde, after.parent.as_ref().unwrap(), &after.items, &after.reloads).unwrap().unwrap();
    assert_eq!(room.used, after_room.used);
    assert!(after.reloads.is_empty());
    let (effects, events) = crate::db::outbox::tests::split(outbox);
    assert_eq!(effects.len(), 1);
    assert!(events.is_empty());
    tx.rollback().await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated database with SDE"]
async fn one_preparation_keeps_refunds_separate_across_owners_and_roots() {
    crate::tests::init_config();
    let config = crate::config::tests::test_database();
    let db = Db::connect(&config).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let charge = sde.types.iter().find(|ty| ty.category_id == Some(CATEGORY_CHARGE)
        && ty.volume.is_some_and(|v| v > 0.0)).unwrap();
    let hull = sde.types.iter().find(|ty| ty.category_id == Some(CATEGORY_SHIP)
        && ty.capacity.is_some_and(|v| v >= charge.volume.unwrap() * 2.0)).unwrap().type_id;
    let (mut client, connection) = tokio_postgres::connect(&config.url, tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let tx = client.transaction().await.unwrap();
    let first = crate::dao::items::creation::rookie_ship(&tx, hull, 101, 1, 0).await.unwrap();
    let second = crate::dao::items::creation::rookie_ship(&tx, hull, 102, 1, 0).await.unwrap();
    let a = pending_reloads::insert(&tx, 101, first, 1, 1, charge.type_id, 1, FLAG_CARGO, first).await.unwrap();
    let b = pending_reloads::insert(&tx, 102, second, 2, 2, charge.type_id, 1, FLAG_CARGO, second).await.unwrap();
    let c = pending_reloads::insert(&tx, 101, first, 3, 3, charge.type_id, 1, FLAG_CARGO, first).await.unwrap();
    let mut pending = pending_reloads::for_ship(&tx, first).await.unwrap();
    pending.extend(pending_reloads::for_ship(&tx, second).await.unwrap());
    let mut refund = RefundPreparation::capture_pending(&tx, &sde, &pending).await.unwrap();
    let mut requests = refund.requests().to_vec();
    requests.extend([first, second].into_iter().map(|item_id| LockRequest { item_id, strength: LockStrength::Update }));
    let locks = crate::services::inventory::destination::lock_items(&tx, &sde, &requests, &[]).await.unwrap();
    let mut outbox: Outbox = Outbox::default();
    let receipts = return_pending(&tx, &sde, &mut outbox, &[a, b], true, &locks, &mut refund).await.unwrap();
    assert_eq!(receipts.len(), 2);
    assert!(receipts.iter().any(|receipt| receipt.created.as_ref().is_some_and(|row| row.item.owner_id == 101 && row.item.location_id == first && row.old_quantity.is_none())));
    assert!(receipts.iter().any(|receipt| receipt.created.as_ref().is_some_and(|row| row.item.owner_id == 102 && row.item.location_id == second && row.old_quantity.is_none())));
    let again = return_pending(&tx, &sde, &mut outbox, &[c], true, &locks, &mut refund).await.unwrap();
    assert_eq!((again[0].last().owner_id, again[0].last().location_id, again[0].last().quantity, again[0].merged.as_ref().unwrap().old_quantity), (101, first, 2, Some(1)));
    let (effects, events) = crate::db::outbox::tests::split(outbox);
    assert_eq!(effects.len(), 3);
    assert!(events.is_empty());
    drop(locks);
    tx.rollback().await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated database with SDE"]
async fn fallback_refund_waits_for_unrelated_surviving_source() {
    crate::tests::init_config();
    let config = crate::config::tests::test_database();
    let db = Db::connect(&config).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let charge = sde.types.iter().find(|ty| ty.category_id == Some(CATEGORY_CHARGE)
        && ty.volume.is_some_and(|v| v > 0.0)).unwrap();
    let hull = sde.types.iter().find(|ty| ty.category_id == Some(CATEGORY_SHIP)
        && ty.capacity.is_some_and(|v| v >= charge.volume.unwrap())).unwrap().type_id;
    let kind = charge.type_id;
    let box_type = sde.types.iter().find(|ty| hold_storage::is_ordinary_container(&sde, ty.type_id)).unwrap().type_id;
    let destination = crate::dao::items::creation::rookie_ship(&db, hull, 101, 1, 0).await.unwrap();
    let root = crate::dao::items::creation::rookie_ship(&db, hull, 102, 1, 0).await.unwrap();
    let source = crate::dao::items::creation::rookie_ship(&db, box_type, 102, root, FLAG_CARGO).await.unwrap();
    let id = pending_reloads::insert(&db, 101, destination, 1, 1, kind, 1, FLAG_CARGO, source).await.unwrap();
    let (mut holder_client, connection) = tokio_postgres::connect(&config.url, tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let holder = holder_client.transaction().await.unwrap();
    let blocker = crate::dao::items::parent_locks::tests::backend_pid(&holder).await;
    let held = crate::services::inventory::destination::lock_items(&holder, &sde,
        &[LockRequest { item_id: source, strength: LockStrength::Update }], &[]).await.unwrap();
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let worker_sde = Arc::clone(&sde);
    let url = config.url.clone();
    let worker = tokio::spawn(async move {
        let (mut client, connection) = tokio_postgres::connect(&url, tokio_postgres::NoTls).await.unwrap();
        tokio::spawn(async move { connection.await.unwrap(); });
        let tx = client.transaction().await.unwrap();
        let pending = pending_reloads::for_ship(&tx, destination).await.unwrap();
        let mut refund = RefundPreparation::capture_pending(&tx, &worker_sde, &pending).await.unwrap();
        let mut requests = refund.requests().to_vec();
        requests.push(LockRequest { item_id: destination, strength: LockStrength::Update });
        ready_tx.send(()).unwrap();
        let locks = crate::services::inventory::destination::lock_items(&tx, &worker_sde, &requests, &[]).await.unwrap();
        let mut outbox: Outbox = Outbox::default();
        let receipts = return_pending(&tx, &worker_sde, &mut outbox, &[id], true, &locks, &mut refund).await.unwrap();
        assert_eq!(receipts.len(), 1);
        assert_eq!((receipts[0].last().location_id, receipts[0].last().quantity), (destination, 1));
        let (effects, events) = crate::db::outbox::tests::split(outbox);
        assert_eq!(effects.len(), 1);
        assert!(events.is_empty());
        drop(locks);
        tx.commit().await.unwrap();
    });
    ready_rx.await.unwrap();
    crate::dao::items::parent_locks::tests::wait_blocked(&db, blocker).await;
    assert_eq!(pending_reloads::for_ship(&db, destination).await.unwrap().len(), 1);
    drop(held);
    holder.rollback().await.unwrap();
    worker.await.unwrap();
    assert!(pending_reloads::for_ship(&db, destination).await.unwrap().is_empty());
    assert!(loading::load_items(&db, ItemQuery::Location(source)).await.unwrap().is_empty());
    db.execute("DELETE FROM game.items WHERE item_id = ANY($1) OR location_id = $2",
        &[&&[source, root, destination][..], &destination]).await.unwrap();
}
