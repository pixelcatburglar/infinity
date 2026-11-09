//! Transactional regressions for canonical deposit and parent guard reuse.
use super::*;
use infinity_core::consts::{CATEGORY_CHARGE, CATEGORY_SHIP, FLAG_CARGO};
use crate::dao::items::inventory;
use std::sync::atomic::{AtomicBool, Ordering};

mod lock_candidates;

async fn guarded_deposit(
    tx: &(impl crate::db::executor::Executor + ?Sized), sde: &Arc<Sde>, outbox: &mut Outbox,
    destination: &Destination, kind: i32, quantity: i64,
    snapshot: &crate::services::inventory::destination::ParentSnapshot,
) -> Result<Item, CallError> {
    use crate::dao::items::parent_locks::LockStrength;
    use crate::services::inventory::destination::{lock_items, parent_requests};
    let mut prep = DepositPreparation::capture(tx, sde, destination, kind).await?;
    let mut requests = parent_requests(sde, snapshot.ancestry.iter().map(|row| (row.item_id, row.type_id)), LockStrength::Update, LockStrength::Share)?;
    if crate::services::inventory::hold_storage::is_ordinary_container(sde, snapshot.item.type_id) {
        requests.push(crate::dao::items::parent_locks::LockRequest { item_id: snapshot.item.item_id, strength: LockStrength::Update });
    }
    let optional: Vec<_> = prep.candidate_id().filter(|id| !requests.iter().any(|request| request.item_id == *id)).into_iter().collect();
    requests.extend(prep.candidate_request());
    let locks = lock_items(tx, sde, &requests, &optional).await?;
    let guard = ParentGuard::revalidate(&locks, std::slice::from_ref(snapshot))?;
    Ok(deposit_prepared(tx, sde, outbox, &mut prep, quantity, 0, Some(&locks), Some(&guard), Default::default(), None).await?.last().clone())
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn parent_guard_batch_stale_hull_and_rollback() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let station = sde.map.stations().next().expect("SDE station").station_id;
    let kind = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_CHARGE)).expect("charge type").type_id;
    let hull_kind = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SHIP)).expect("ship type").type_id;
    let fixture = crate::services::tests::fixture::create(&db).await.unwrap();
    let owner = fixture.character;
    let corporation = crate::services::tests::fixture::create_corporation(&db, owner, station).await.unwrap();
    let dispatched = AtomicBool::new(false);
    let result: Result<(), CallError> = db.transaction(
        |tx, outbox: &mut Outbox| {
            let sde = sde.clone();
            Box::pin(async move {
                let box_id = transfers::insert(tx, kind, owner, station, FLAG_HANGAR, 1).await.map_err(db_err)?;
                let snapshot = load_items(tx, &sde, ItemQuery::Id(box_id)).await.map_err(db_err)?.pop().unwrap();
                let parent_snapshot = crate::services::inventory::destination::ParentSnapshot { item: snapshot, ancestry: crate::dao::items::deliveries::ancestry(tx, box_id).await.map_err(db_err)? };
                let empty_locks = crate::services::inventory::destination::lock_items(tx, &sde, &[], &[]).await?;
                for (new_owner, new_location, new_flag) in [(owner + 1, station, FLAG_HANGAR), (owner, station + 1, FLAG_HANGAR), (owner, station, FLAG_CARGO)] {
                    transfers::hand_over(tx, &[transfers::Transfer { item_id: box_id, owner: new_owner, location: new_location, flag: new_flag }], &[box_id], &empty_locks, &[box_id]).await.map_err(db_err)?;
                    if new_owner != owner {
                        assert!(deposit(tx, &sde, outbox, &Destination::owned(owner, box_id, FLAG_CARGO), kind, 1, 0).await.is_err());
                    }
                    let locks = crate::services::inventory::destination::lock_items(tx, &sde, &[crate::dao::items::parent_locks::LockRequest { item_id: box_id, strength: crate::dao::items::parent_locks::LockStrength::Share }], &[]).await?;
                    assert!(ParentGuard::revalidate(&locks, std::slice::from_ref(&parent_snapshot)).is_err());
                    transfers::hand_over(tx, &[transfers::Transfer { item_id: box_id, owner, location: station, flag: FLAG_HANGAR }], &[box_id], &empty_locks, &[box_id]).await.map_err(db_err)?;
                }
                let destination = Destination::owned(owner, box_id, FLAG_CARGO);
                let wrong_owner = Destination { capacity_owner: owner + 1, ..destination };
                assert!(guarded_deposit(tx, &sde, outbox, &wrong_owner, kind, 1, &parent_snapshot).await.is_err());
                let first = guarded_deposit(tx, &sde, outbox, &destination, kind, 2, &parent_snapshot).await?;
                let merged = guarded_deposit(tx, &sde, outbox, &destination, kind, 3, &parent_snapshot).await?;
                assert_eq!((merged.item_id, merged.type_id, merged.owner_id, merged.location_id, merged.flag_id, merged.quantity, merged.singleton),
                    (first.item_id, kind, owner, box_id, FLAG_CARGO, 5, false));
                let rows = load_items(tx, &sde, ItemQuery::Location(box_id)).await.map_err(db_err)?;
                assert_eq!(rows.len(), 1);
                assert_eq!(rows[0].quantity, 5);
                let guest = Destination { item_owner: owner + 1, ..destination };
                guarded_deposit(tx, &sde, outbox, &guest, kind, 1, &parent_snapshot).await?;
                let rows = load_items(tx, &sde, ItemQuery::Location(box_id)).await.map_err(db_err)?;
                assert!(rows.iter().any(|row| row.owner_id == owner + 1 && row.quantity == 1));
                let missing = transfers::insert(tx, kind, owner, station, FLAG_HANGAR, 1).await.map_err(db_err)?;
                inventory::delete(tx, missing).await.map_err(db_err)?;
                assert!(guarded_deposit(tx, &sde, outbox, &Destination::owned(owner, missing, FLAG_CARGO), kind, 1, &parent_snapshot).await.is_err());
                assert!(deposit(tx, &sde, outbox, &Destination::owned(owner, missing, FLAG_CARGO), kind, 1, 0).await.is_err());
                deposit(tx, &sde, outbox, &Destination::owned(owner, station, FLAG_HANGAR), kind, 1, 0).await?;
                let office = transfers::insert(tx, kind, corporation, station, FLAG_HANGAR, 1).await.map_err(db_err)?;
                crate::dao::offices::insert(tx, office, corporation, station, i64::MAX).await.map_err(db_err)?;
                deposit(tx, &sde, outbox, &Destination { item_owner: owner, capacity_owner: corporation, location: office, flag: FLAG_HANGAR }, kind, 1, 0).await.unwrap();
                let hull = crate::dao::items::creation::rookie_ship(tx, hull_kind, owner, station, FLAG_HANGAR).await.map_err(db_err)?;
                deposit_with_capacity(tx, &sde, outbox, &Destination::owned(owner, hull, FLAG_CARGO), kind, 1, 0, Default::default(), Some(1_000_000.0)).await?;
                let wrong = Destination::owned(owner + 1, hull, FLAG_CARGO);
                assert!(deposit_with_capacity(tx, &sde, outbox, &wrong, kind, 1, 0, Default::default(), Some(1_000_000.0)).await.is_err());
                Err(CallError::Internal(anyhow::anyhow!("rollback fixture")))
            })
        },
        |(), _| async { dispatched.store(true, Ordering::SeqCst); Ok(()) },
    ).await;
    assert!(matches!(result, Err(CallError::Internal(err)) if err.to_string() == "rollback fixture"));
    assert!(!dispatched.load(Ordering::SeqCst));
    assert!(load_items(&db, &sde, ItemQuery::Owner(owner)).await.unwrap().is_empty());
    crate::services::tests::fixture::cleanup_corporation(&db, corporation, &[owner], 0).await.unwrap();
    crate::services::tests::fixture::cleanup(&db, fixture).await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn guarded_take_and_corporate_audience() {
    use infinity_core::consts::{FLAG_CORP_DELIVERIES, ROLE_DELIVERIES_VIEW};
    use crate::db::outbox::{Effect, Notification};
    use crate::services::tests::fixture;

    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let station = sde.map.stations().next().unwrap().station_id;
    let kind = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_CHARGE)).unwrap().type_id;
    let viewer = fixture::create(&db).await.unwrap();
    let denied = fixture::create(&db).await.unwrap();
    let corp = fixture::create_corporation(&db, viewer.character, station).await.unwrap();
    fixture::assign_corporation(&db, viewer.character, corp, ROLE_DELIVERIES_VIEW).await.unwrap();
    fixture::assign_corporation(&db, denied.character, corp, 0).await.unwrap();

    let mut notices: Outbox = Outbox::default();
    let receipt = deposit(&db, &sde, &mut notices, &Destination::owned(corp, station, FLAG_CORP_DELIVERIES), kind, 5, 0).await.unwrap();
    let item = receipt.last();
    let recipients = |outbox: Outbox| -> Vec<i64> {
        outbox.into_entries().into_iter().map(|entry| match entry {
            Effect::Notification(Notification::Pilot { char_id, .. }) => char_id,
            _ => panic!("unexpected corporate notification"),
        }).collect()
    };
    assert_eq!(recipients(notices), vec![viewer.character]);
    let mut notices: Outbox = Outbox::default();
    take_items(&db, &mut notices, item.clone(), 2).await.unwrap();
    assert_eq!(recipients(notices), vec![viewer.character]);
    let mut notices: Outbox = Outbox::default();
    assert!(take_items(&db, &mut notices, item.clone(), 5).await.is_err());
    assert!(take_items(&db, &mut notices, item.clone(), 1).await.is_err());
    assert!(notices.into_entries().is_empty());
    let current = load_items(&db, &sde, ItemQuery::Id(item.item_id)).await.unwrap().pop().unwrap();
    assert_eq!(current.quantity, 3);
    let mut notices: Outbox = Outbox::default();
    take_items(&db, &mut notices, current, 3).await.unwrap();
    assert_eq!(recipients(notices), vec![viewer.character]);
    assert!(load_items(&db, &sde, ItemQuery::Id(item.item_id)).await.unwrap().is_empty());

    fixture::cleanup_corporation(&db, corp, &[viewer.character, denied.character], 0).await.unwrap();
    fixture::cleanup(&db, viewer).await.unwrap();
    fixture::cleanup(&db, denied).await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn personal_office_stack_notices_follow_office_viewers() {
    use infinity_core::consts::{FLAG_CORP_SAG1, ROLE_HANGAR_CAN_QUERY_1};
    use crate::db::outbox::{Effect, Notification};
    use crate::services::tests::fixture;

    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let station = sde.map.stations().next().unwrap().station_id;
    let kind = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_CHARGE)).unwrap().type_id;
    let viewer = fixture::create(&db).await.unwrap();
    let denied = fixture::create(&db).await.unwrap();
    let owner = fixture::create(&db).await.unwrap();
    let corp = fixture::create_corporation(&db, viewer.character, station).await.unwrap();
    fixture::assign_corporation(&db, viewer.character, corp, ROLE_HANGAR_CAN_QUERY_1).await.unwrap();
    fixture::assign_corporation(&db, denied.character, corp, 0).await.unwrap();
    let office = transfers::insert(&db, kind, corp, station, FLAG_HANGAR, 1).await.unwrap();
    crate::dao::offices::insert(&db, office, corp, station, i64::MAX).await.unwrap();
    let recipients = |outbox: Outbox| -> Vec<i64> {
        outbox.into_entries().into_iter().map(|entry| match entry {
            Effect::Notification(Notification::Pilot { char_id, event, args }) => {
                assert_eq!(event, "OnItemChange");
                assert_eq!(args.len(), 3);
                char_id
            }
            _ => panic!("unexpected office notification"),
        }).collect()
    };
    let mut notices: Outbox = Outbox::default();
    let receipt = deposit(&db, &sde, &mut notices, &Destination { item_owner: owner.character, capacity_owner: corp, location: office, flag: FLAG_CORP_SAG1 }, kind, 5, 0).await.unwrap();
    let item = receipt.last();
    assert_eq!((item.owner_id, item.location_id, item.flag_id, item.type_id, item.quantity), (owner.character, office, FLAG_CORP_SAG1, kind, 5));
    assert_eq!(recipients(notices), vec![viewer.character]);
    let mut notices: Outbox = Outbox::default();
    take_items(&db, &mut notices, item.clone(), 2).await.unwrap();
    assert_eq!(recipients(notices), vec![viewer.character]);
    let current = load_items(&db, &sde, ItemQuery::Id(item.item_id)).await.unwrap().pop().unwrap();
    let mut notices: Outbox = Outbox::default();
    take_items(&db, &mut notices, current, 3).await.unwrap();
    assert_eq!(recipients(notices), vec![viewer.character]);
    fixture::cleanup_corporation(&db, corp, &[viewer.character, denied.character], office).await.unwrap();
    fixture::cleanup(&db, viewer).await.unwrap();
    fixture::cleanup(&db, denied).await.unwrap();
    fixture::cleanup(&db, owner).await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn delivery_returns_new_and_merged_rows_at_docked_and_remote_stations() {
    use crate::db::outbox::{Effect, Notification};
    use crate::services::tests::fixture;

    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let docked = sde.map.stations().find(|station| station.solar_system_id.is_some()).unwrap();
    let remote = sde.map.stations().find(|station| station.station_id != docked.station_id).unwrap();
    let kind = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_CHARGE)).unwrap().type_id;
    let fixture = fixture::create(&db).await.unwrap();
    let owner = fixture.character;
    crate::dao::characters::ship::dock(&db, owner, docked.station_id, docked.solar_system_id.unwrap()).await.unwrap();

    for (station, expect_notice) in [(docked.station_id, true), (remote.station_id, false)] {
        let mut notices: Outbox = Outbox::default();
        let first_receipt = deliver(&db, &sde, &mut notices, owner, station, kind, 5).await.unwrap();
        let first = first_receipt.last();
        assert_eq!((first.owner_id, first.location_id, first.flag_id, first.type_id, first.quantity), (owner, station, FLAG_HANGAR, kind, 5));
        let entries = notices.into_entries();
        assert_eq!(entries.len(), usize::from(expect_notice));
        if expect_notice {
            assert!(matches!(&entries[0], Effect::Notification(Notification::Pilot { char_id, event, .. }) if *char_id == owner && event == "OnItemChange"));
        }
        let mut notices: Outbox = Outbox::default();
        let merged_receipt = deliver(&db, &sde, &mut notices, owner, station, kind, 3).await.unwrap();
        let merged = merged_receipt.last();
        assert_eq!((merged.item_id, merged.owner_id, merged.location_id, merged.flag_id, merged.type_id, merged.quantity),
            (first.item_id, owner, station, FLAG_HANGAR, kind, 8));
        assert_eq!(notices.into_entries().len(), usize::from(expect_notice));
        let stored = load_items(&db, &sde, ItemQuery::Id(merged.item_id)).await.unwrap().pop().unwrap();
        assert_eq!((stored.item_id, stored.location_id, stored.flag_id, stored.quantity, stored.custom_info.as_ref()),
            (merged.item_id, merged.location_id, merged.flag_id, merged.quantity, merged.custom_info.as_ref()));
    }
    fixture::cleanup(&db, fixture).await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn continued_deposit_crosses_stack_boundary() {
    use crate::dao::items::parent_locks::{LockRequest, LockStrength};
    use crate::services::inventory::destination::lock_items;
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let station = sde.map.stations().next().unwrap().station_id;
    let kind = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_CHARGE)).unwrap().type_id;
    let hull_kind = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SHIP)).unwrap().type_id;
    let fixture = crate::services::tests::fixture::create(&db).await.unwrap();
    let owner = fixture.character;
    let mut client = lock_candidates::connection().await;
    let tx = client.transaction().await.unwrap();
    let hull = crate::dao::items::creation::rookie_ship(&tx, hull_kind, owner, station, FLAG_HANGAR).await.unwrap();
    let stack = transfers::insert(&tx, kind, owner, hull, FLAG_CARGO, i64::MAX - 3).await.unwrap();
    let destination = Destination::owned(owner, hull, FLAG_CARGO);
    let mut prep = DepositPreparation::capture(&tx, &sde, &destination, kind).await.unwrap();
    let requests = [LockRequest { item_id: hull, strength: LockStrength::Update }, prep.candidate_request().unwrap()];
    let locks = lock_items(&tx, &sde, &requests, &[]).await.unwrap();
    let mut outbox: Outbox = Outbox::default();
    let first = deposit_prepared(&tx, &sde, &mut outbox, &mut prep, 1, 0, Some(&locks), None, Default::default(), Some(f64::MAX)).await.unwrap();
    assert_eq!(first.merged.as_ref().unwrap().old_quantity, Some(i64::MAX - 3));
    let receipt = deposit_prepared(&tx, &sde, &mut outbox, &mut prep, 3, 0, Some(&locks), None, Default::default(), Some(f64::MAX)).await.unwrap();
    let merged = receipt.merged.as_ref().unwrap();
    let created = receipt.created.as_ref().unwrap();
    assert_eq!((merged.item.item_id, merged.old_quantity, merged.added_quantity, merged.item.quantity), (stack, Some(i64::MAX - 2), 2, i64::MAX));
    assert_eq!((created.old_quantity, created.added_quantity, created.item.quantity), (None, 1, 1));
    assert_ne!(created.item.item_id, stack);
    assert_eq!(receipt.last().item_id, created.item.item_id);
    assert!(enrich_deposit(&sde, transfers::DepositStack { merged: None, created: None }, 1).is_err());
    use crate::db::outbox::{Effect, Notification};
    let entries = outbox.into_entries();
    assert_eq!(entries.len(), 3);
    let facts = [first.merged.as_ref().unwrap(), merged, created];
    for (sequence, (entry, fact)) in entries.into_iter().zip(facts).enumerate() {
        let Effect::Notification(Notification::DirectClient { event, args }) = entry else {
            panic!("expected direct item notification");
        };
        assert_eq!(event, "OnItemChange");
        let payload = Value::Tuple(args);
        let wire = marshal::dumps(&payload);
        let decoded = marshal::loads(&wire).unwrap();
        let parts = decoded.items().unwrap();
        let Value::DbRow { descriptor, data, objects } = &parts[0] else { panic!("expected item row") };
        let descriptor = marshal::DbRowDescriptor::from_value(descriptor).unwrap();
        assert_eq!(descriptor.get(&parts[0], "itemID"), Some(Value::Int(fact.item.item_id)));
        assert_eq!(descriptor.get(&parts[0], "quantity"), Some(Value::Int(fact.item.quantity.min(infinity_core::consts::CLIENT_MAX_SHORT_AMOUNT))));
        let scalar = |value: &Value| match value {
            Value::None => serde_json::json!({"type":"none"}),
            Value::Int(n) => serde_json::json!({"type":"int","value":n.to_string()}),
            Value::Bool(v) => serde_json::json!({"type":"bool","value":v}),
            Value::Bytes(v) => serde_json::json!({"type":"str","hex":v.iter().map(|b| format!("{b:02x}")).collect::<String>()}),
            Value::Unicode(v) => serde_json::json!({"type":"unicode","value":v}),
            _ => panic!("unexpected item scalar"),
        };
        let Value::Dict(change) = &parts[1] else { panic!("expected item change") };
        let changes: Vec<_> = change.iter().map(|(key, value)| serde_json::json!([scalar(key), scalar(value)])).collect();
        let values: Vec<_> = descriptor.columns.iter().map(|(name, _)| scalar(&descriptor.get(&parts[0], name).unwrap())).collect();
        let columns: Vec<_> = descriptor.columns.iter().map(|(name, ty)| serde_json::json!([name, *ty as i64])).collect();
        let virtual_columns: Vec<_> = descriptor.virtual_columns.iter().map(|(name, callable)| serde_json::json!([name, callable])).collect();
        let hex = |bytes: &[u8]| bytes.iter().map(|b| format!("{b:02x}")).collect::<String>();
        println!("CLIENT_QUANTITY_INPUT_V1 {}", serde_json::json!({
            "version": 1, "id": format!("continued-deposit-{sequence}"), "scenario": "continued_deposit_crosses_stack_boundary",
            "producer_test": "services::items::tests::continued_deposit_crosses_stack_boundary",
            "producer_symbols": ["deposit_prepared", "deposit_write", "Item::row"], "kind": "notification",
            "sequence": sequence.to_string(), "event": event, "recipient": {"kind":"direct_client"},
            "session": {"source":"fixture_context", "generation":null},
            "wire_hex": hex(&wire), "wire_size": wire.len().to_string(), "payload_scope": "notification_args",
            "rows": [{"path":[0], "columns":columns, "virtual_columns":virtual_columns, "values":values,
                "fixed_hex":hex(data), "objects":objects.iter().map(scalar).collect::<Vec<_>>()}],
            "change": changes,
            "before": {"source":"server_output", "item_id":fact.item.item_id.to_string(),
                "quantity":fact.old_quantity.map(|n| n.to_string())},
            "after": {"source":"server_output", "item_id":fact.item.item_id.to_string(),
                "quantity":fact.item.quantity.to_string(), "added_quantity":fact.added_quantity.to_string()},
        }));
        if let Some(old) = fact.old_quantity {
            let expected = serde_json::json!([{"type":"int","value":"9"},{"type":"int","value":old.min(infinity_core::consts::CLIENT_MAX_SHORT_AMOUNT).to_string()}]);
            assert_eq!(changes, vec![expected]);
        }
    }
    drop(locks);
    tx.rollback().await.unwrap();
    crate::services::tests::fixture::cleanup(&db, fixture).await.unwrap();
}
