//! PostgreSQL regressions for selected stack locks and held-hull deposits.
use super::*;
use crate::dao::items::parent_locks::{LockRequest, LockStrength};
use crate::services::inventory::{destination::{lock_items, ParentSnapshot}, hold_storage::{self, Addition}};
use std::time::Duration;

pub(super) async fn connection() -> tokio_postgres::Client {
    let (client, connection) = crate::config::tests::test_database().to_pg_config().unwrap().connect(tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    client
}
async fn box_fixture(db: &crate::db::Db, sde: &Sde, owner: i64, station: i64) -> (i64, i32) {
    let (container, charge) = sde.types.iter()
        .filter(|t| t.published == Some(true) && t.group_id.is_some_and(|g| infinity_core::consts::CARGO_CONTAINER_GROUPS.contains(&g)))
        .find_map(|container| {
            let capacity = container.attribute(infinity_core::consts::dogma::ATTR_CAPACITY).or(container.capacity).filter(|v| v.is_finite() && *v > 0.0)?;
            let charge = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_CHARGE)
                && t.volume.is_some_and(|v| v.is_finite() && v > 0.0 && v * 17.0 <= capacity))?;
            Some((container.type_id, charge.type_id))
        }).expect("SDE cargo container and fitting charge");
    let parent = crate::dao::items::creation::rookie_ship(db, container, owner, station, FLAG_HANGAR).await.unwrap();
    (parent, charge)
}


#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn selected_candidate_merges_only_the_locked_duplicate() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let station = sde.map.stations().next().unwrap().station_id;
    let fixture = crate::services::tests::fixture::create(&db).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
    let db = test_db;
    let owner = fixture.character;
    let (parent, kind) = box_fixture(&db, &sde, owner, station).await;
    let first = transfers::insert(&db, kind, owner, parent, FLAG_CARGO, 5).await.unwrap();
    let second = transfers::insert(&db, kind, owner, parent, FLAG_CARGO, 7).await.unwrap();
    let mut client = connection().await;
    let tx = client.transaction().await.unwrap();
    let destination = Destination::owned(owner, parent, FLAG_CARGO);
    let mut prep = DepositPreparation::capture(&tx, &sde, &destination, kind).await.unwrap();
    let snapshot = ParentSnapshot { item: load_items(&tx, &sde, ItemQuery::Id(parent)).await.unwrap().pop().unwrap(), ancestry: crate::dao::items::deliveries::ancestry(&tx, parent).await.unwrap() };
    assert_eq!(prep.candidate_id(), Some(first));
    let requests = [LockRequest { item_id: parent, strength: LockStrength::Update }, prep.candidate_request().unwrap()];
    let locks = lock_items(&tx, &sde, &requests, &[first]).await.unwrap();
    let guard = ParentGuard::revalidate(&locks, &[snapshot]).unwrap();
    let mut outbox: Outbox = Outbox::default();
    let receipt = deposit_prepared(&tx, &sde, &mut outbox, &mut prep, 3, 0, Some(&locks), Some(&guard), Default::default(), None).await.unwrap();
    let row = receipt.merged.as_ref().unwrap();
    assert_eq!((row.item.item_id, row.item.quantity, row.old_quantity), (first, 8, Some(5)));
    let receipt = deposit_prepared(&tx, &sde, &mut outbox, &mut prep, 2, 0, Some(&locks), Some(&guard), Default::default(), None).await.unwrap();
    let row = receipt.merged.as_ref().unwrap();
    assert_eq!((row.item.item_id, row.item.quantity, row.old_quantity), (first, 10, Some(8)));
    let fresh = prep.fresh_attempt();
    assert!(matches!(fresh.progress, DepositProgress::Captured));
    tx.commit().await.unwrap();
    let other = load_items(&db, &sde, ItemQuery::Id(second)).await.unwrap().pop().unwrap();
    assert_eq!(other.quantity, 7);
    }).await;
    crate::services::tests::fixture::cleanup(&db, fixture).await.unwrap();
    result.expect("selected candidate regression");
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn deleted_or_moved_candidate_inserts_without_merging_another_stack() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let station = sde.map.stations().next().unwrap().station_id;
    let fixture = crate::services::tests::fixture::create(&db).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
    let db = test_db;
    let owner = fixture.character;
    for moved in [false, true] {
        let (parent, kind) = box_fixture(&db, &sde, owner, station).await;
        let chosen = transfers::insert(&db, kind, owner, parent, FLAG_CARGO, 5).await.unwrap();
        let other = transfers::insert(&db, kind, owner, parent, FLAG_CARGO, 7).await.unwrap();
        let mut client = connection().await;
        let tx = client.transaction().await.unwrap();
        let destination = Destination::owned(owner, parent, FLAG_CARGO);
        let mut prep = DepositPreparation::capture(&tx, &sde, &destination, kind).await.unwrap();
        let snapshot = ParentSnapshot { item: load_items(&tx, &sde, ItemQuery::Id(parent)).await.unwrap().pop().unwrap(), ancestry: crate::dao::items::deliveries::ancestry(&tx, parent).await.unwrap() };
        assert_eq!(prep.candidate_id(), Some(chosen));
        let separate = connection().await;
        if moved {
            separate.execute("UPDATE game.items SET location_id = $2 WHERE item_id = $1", &[&chosen, &station]).await.unwrap();
        } else {
            separate.execute("DELETE FROM game.items WHERE item_id = $1", &[&chosen]).await.unwrap();
        }
        let requests = [LockRequest { item_id: parent, strength: LockStrength::Update }, prep.candidate_request().unwrap()];
        let locks = lock_items(&tx, &sde, &requests, &[chosen]).await.unwrap();
        let guard = ParentGuard::revalidate(&locks, &[snapshot]).unwrap();
        let mut outbox: Outbox = Outbox::default();
        let receipt = deposit_prepared(&tx, &sde, &mut outbox, &mut prep, 3, 0, Some(&locks), Some(&guard), Default::default(), None).await.unwrap();
        let item = &receipt.created.as_ref().unwrap().item;
        assert_ne!(item.item_id, chosen);
        assert_ne!(item.item_id, other);
        let merged = deposit_prepared(&tx, &sde, &mut outbox, &mut prep, 2, 0, Some(&locks), Some(&guard), Default::default(), None).await.unwrap();
        let row = merged.merged.as_ref().unwrap();
        assert_eq!((row.item.item_id, row.item.quantity, row.old_quantity), (item.item_id, 5, Some(3)));
        tx.commit().await.unwrap();
        assert_eq!(load_items(&db, &sde, ItemQuery::Id(other)).await.unwrap()[0].quantity, 7);
        if moved {
            let row = load_items(&db, &sde, ItemQuery::Id(chosen)).await.unwrap().pop().unwrap();
            assert_eq!((row.location_id, row.quantity), (station, 5));
        }
        inventory::delete(&db, other).await.unwrap();
        inventory::delete(&db, item.item_id).await.unwrap();
        if moved { inventory::delete(&db, chosen).await.unwrap(); }
        inventory::delete(&db, parent).await.unwrap();
    }
    }).await;
    crate::services::tests::fixture::cleanup(&db, fixture).await.unwrap();
    result.expect("candidate drift regression");
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn stale_mandatory_parent_refuses_without_effects() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let station = sde.map.stations().next().unwrap().station_id;
    let fixture = crate::services::tests::fixture::create(&db).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
    let db = test_db;
    let owner = fixture.character;
    let (parent, kind) = box_fixture(&db, &sde, owner, station).await;
    let candidate = transfers::insert(&db, kind, owner, parent, FLAG_CARGO, 5).await.unwrap();
    let mut client = connection().await;
    let tx = client.transaction().await.unwrap();
    let item = load_items(&tx, &sde, ItemQuery::Id(parent)).await.unwrap().pop().unwrap();
    let ancestry = crate::dao::items::deliveries::ancestry(&tx, parent).await.unwrap();
    let snapshot = ParentSnapshot { item, ancestry };
    let destination = Destination::owned(owner, parent, FLAG_CARGO);
    let mut prep = DepositPreparation::capture(&tx, &sde, &destination, kind).await.unwrap();
    assert_eq!(prep.candidate_id(), Some(candidate));
    let separate = connection().await;
    separate.execute("UPDATE game.items SET flag_id = $2 WHERE item_id = $1", &[&parent, &FLAG_CARGO]).await.unwrap();
    let requests = [LockRequest { item_id: parent, strength: LockStrength::Update }, prep.candidate_request().unwrap()];
    let locks = lock_items(&tx, &sde, &requests, &[candidate]).await.unwrap();
    let mut outbox: Outbox = Outbox::default();
    let result = match ParentGuard::revalidate(&locks, &[snapshot]) {
        Ok(guard) => deposit_prepared(&tx, &sde, &mut outbox, &mut prep, 2, 0, Some(&locks), Some(&guard), Default::default(), None).await.map(|_| ()),
        Err(error) => Err(error),
    };
    assert!(result.is_err());
    assert!(outbox.into_entries().is_empty());
    tx.rollback().await.unwrap();
    let row = load_items(&db, &sde, ItemQuery::Id(parent)).await.unwrap().pop().unwrap();
    assert_eq!((row.flag_id, row.quantity), (FLAG_CARGO, 1));
    }).await;
    crate::services::tests::fixture::cleanup(&db, fixture).await.unwrap();
    result.expect("parent drift regression");
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn candidate_also_mandatory_source_drift_refuses_during_lock_acquisition() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let station = sde.map.stations().next().unwrap().station_id;
    let fixture = crate::services::tests::fixture::create(&db).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
    let db = test_db;
    let owner = fixture.character;
    let (parent, kind) = box_fixture(&db, &sde, owner, station).await;
    let source = transfers::insert(&db, kind, owner, parent, FLAG_CARGO, 5).await.unwrap();
    let mut a = connection().await;
    let mut b = connection().await;
    let holder = a.transaction().await.unwrap();
    let operation = b.transaction().await.unwrap();
    let pid = crate::dao::items::parent_locks::tests::backend_pid(&holder).await;
    let destination = Destination::owned(owner, parent, FLAG_CARGO);
    let mut prep = DepositPreparation::capture(&operation, &sde, &destination, kind).await.unwrap();
    let snapshot = ParentSnapshot { item: load_items(&operation, &sde, ItemQuery::Id(parent)).await.unwrap().pop().unwrap(), ancestry: crate::dao::items::deliveries::ancestry(&operation, parent).await.unwrap() };
    assert_eq!(prep.candidate_id(), Some(source));
    holder.execute("UPDATE game.items SET location_id = $2 WHERE item_id = $1", &[&source, &station]).await.unwrap();
    let holder_op = async {
        tokio::time::timeout(Duration::from_secs(10), crate::dao::items::parent_locks::tests::wait_blocked(&db, pid)).await.unwrap();
        holder.commit().await.unwrap();
    };
    let deposit_op = async {
        let requests = [LockRequest { item_id: parent, strength: LockStrength::Update }, LockRequest { item_id: source, strength: LockStrength::Update }, prep.candidate_request().unwrap()];
        let mut outbox: Outbox = Outbox::default();
        let result = match lock_items(&operation, &sde, &requests, &[]).await {
            Ok(locks) => match ParentGuard::revalidate(&locks, &[snapshot]) {
                Ok(guard) => deposit_prepared(&operation, &sde, &mut outbox, &mut prep, 2, 0, Some(&locks), Some(&guard), Default::default(), None).await.map(|_| ()),
                Err(error) => Err(error),
            },
            Err(error) => Err(error),
        };
        assert!(result.is_err());
        assert!(outbox.into_entries().is_empty());
        operation.rollback().await.unwrap();
    };
    tokio::time::timeout(Duration::from_secs(10), async { tokio::join!(holder_op, deposit_op); }).await.unwrap();
    let row = load_items(&db, &sde, ItemQuery::Id(source)).await.unwrap().pop().unwrap();
    assert_eq!((row.location_id, row.flag_id, row.quantity), (station, FLAG_CARGO, 5));
    let rows = load_items(&db, &sde, ItemQuery::Location(parent)).await.unwrap();
    assert!(!rows.iter().any(|row| row.owner_id == owner && row.type_id == kind && row.flag_id == FLAG_CARGO));
    }).await;
    crate::services::tests::fixture::cleanup(&db, fixture).await.unwrap();
    result.expect("mandatory source drift regression");
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn concurrent_held_hull_deposits_merge_in_commit_order() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let station = sde.map.stations().next().unwrap().station_id;
    let kind = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_CHARGE)).unwrap().type_id;
    let hull_kind = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SHIP)).unwrap().type_id;
    let fixture = crate::services::tests::fixture::create(&db).await.unwrap();
    let owner = fixture.character;
    let hull = crate::dao::items::creation::rookie_ship(&db, hull_kind, owner, station, FLAG_HANGAR).await.unwrap();
    let original = transfers::insert(&db, kind, owner, hull, FLAG_CARGO, 5).await.unwrap();
    let mut a = connection().await;
    let mut b = connection().await;
    let first = a.transaction().await.unwrap();
    let second = b.transaction().await.unwrap();
    let pid = crate::dao::items::parent_locks::tests::backend_pid(&first).await;
    let destination = Destination::owned(owner, hull, FLAG_CARGO);
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let first_op = async {
        let additions = [Addition { type_id: kind, singleton: false, quantity: 2 }];
        let held = hold_storage::check_destination(&first, &sde, owner, hull, FLAG_CARGO, &additions, Default::default(), Some(1_000_000.0)).await.unwrap().unwrap();
        let mut outbox: Outbox = Outbox::default();
        let receipt = deposit_held(&first, &sde, &mut outbox, &destination, kind, 2, 0, Some(&held), Some(1_000_000.0)).await.unwrap();
        let row = receipt.merged.as_ref().unwrap();
        assert_eq!((row.item.item_id, row.old_quantity), (original, Some(5)));
        ready_tx.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(10), crate::dao::items::parent_locks::tests::wait_blocked(&db, pid)).await.unwrap();
        first.commit().await.unwrap();
    };
    let second_op = async {
        ready_rx.await.unwrap();
        let additions = [Addition { type_id: kind, singleton: false, quantity: 3 }];
        let held = hold_storage::check_destination(&second, &sde, owner, hull, FLAG_CARGO, &additions, Default::default(), Some(1_000_000.0)).await.unwrap().unwrap();
        let mut outbox: Outbox = Outbox::default();
        let receipt = deposit_held(&second, &sde, &mut outbox, &destination, kind, 3, 0, Some(&held), Some(1_000_000.0)).await.unwrap();
        let row = receipt.merged.as_ref().unwrap();
        assert_eq!((row.item.item_id, row.old_quantity), (original, Some(7)));
        second.commit().await.unwrap();
    };
    tokio::time::timeout(Duration::from_secs(10), async { tokio::join!(first_op, second_op); }).await.unwrap();
    let rows = load_items(&db, &sde, ItemQuery::Location(hull)).await.unwrap();
    let stacks: Vec<_> = rows.iter().filter(|row| row.type_id == kind && row.flag_id == FLAG_CARGO).collect();
    assert_eq!(stacks.len(), 1);
    assert_eq!((stacks[0].item_id, stacks[0].quantity), (original, 10));
    crate::services::tests::fixture::cleanup(&db, fixture).await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn created_guard_miss_rolls_back_and_fresh_attempt_discards_created_id() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let station = sde.map.stations().next().unwrap().station_id;
    let fixture = crate::services::tests::fixture::create(&db).await.unwrap();
    let owner = fixture.character;
    let (parent, kind) = box_fixture(&db, &sde, owner, station).await;
    let destination = Destination::owned(owner, parent, FLAG_CARGO);
    let mut prep = DepositPreparation::capture(&db, &sde, &destination, kind).await.unwrap();
    let snapshot = ParentSnapshot { item: load_items(&db, &sde, ItemQuery::Id(parent)).await.unwrap().pop().unwrap(), ancestry: crate::dao::items::deliveries::ancestry(&db, parent).await.unwrap() };
    let mut client = connection().await;
    let tx = client.transaction().await.unwrap();
    let requests = [LockRequest { item_id: parent, strength: LockStrength::Update }];
    let locks = lock_items(&tx, &sde, &requests, &[]).await.unwrap();
    let guard = ParentGuard::revalidate(&locks, std::slice::from_ref(&snapshot)).unwrap();
    let mut outbox: Outbox = Outbox::default();
    let receipt = deposit_prepared(&tx, &sde, &mut outbox, &mut prep, 2, 0, Some(&locks), Some(&guard), Default::default(), None).await.unwrap();
    let created = &receipt.created.as_ref().unwrap().item;
    let receipt2 = deposit_prepared(&tx, &sde, &mut outbox, &mut prep, 2, 0, Some(&locks), Some(&guard), Default::default(), None).await.unwrap();
    let merged = receipt2.merged.as_ref().unwrap();
    assert_eq!((merged.item.item_id, merged.item.quantity, merged.old_quantity), (created.item_id, 4, Some(2)));
    drop(locks);
    tx.rollback().await.unwrap();
    let mut fresh = prep.fresh_attempt();
    assert!(matches!(fresh.progress, DepositProgress::Captured));
    let tx = client.transaction().await.unwrap();
    let locks = lock_items(&tx, &sde, &requests, &[]).await.unwrap();
    let guard = ParentGuard::revalidate(&locks, std::slice::from_ref(&snapshot)).unwrap();
    let mut outbox: Outbox = Outbox::default();
    let receipt = deposit_prepared(&tx, &sde, &mut outbox, &mut fresh, 2, 0, Some(&locks), Some(&guard), Default::default(), None).await.unwrap();
    let created_again = &receipt.created.as_ref().unwrap().item;
    assert_ne!(created_again.item_id, created.item_id);
    drop(locks);
    tx.rollback().await.unwrap();

    let dispatched = Arc::new(AtomicBool::new(false));
    let dispatch_flag = dispatched.clone();
    let result: Result<(), CallError> = db.transaction(
        |tx, outbox: &mut Outbox| {
            let sde = sde.clone();
            let mut prep = prep.fresh_attempt();
            let snapshot = snapshot.clone();
            Box::pin(async move {
                let locks = lock_items(tx, &sde, &requests, &[]).await?;
                let guard = ParentGuard::revalidate(&locks, &[snapshot])?;
                let receipt = deposit_prepared(tx, &sde, outbox, &mut prep, 2, 0, Some(&locks), Some(&guard), Default::default(), None).await?;
                tx.execute("DELETE FROM game.items WHERE item_id = $1", &[&receipt.last().item_id]).await.unwrap();
                deposit_prepared(tx, &sde, outbox, &mut prep, 2, 0, Some(&locks), Some(&guard), Default::default(), None).await?;
                Ok(())
            })
        },
        move |(), _: Outbox| async move {
            dispatch_flag.store(true, Ordering::SeqCst);
            Ok(())
        },
    ).await;
    assert!(result.is_err());
    assert!(!dispatched.load(Ordering::SeqCst));
    assert!(load_items(&db, &sde, ItemQuery::Location(parent)).await.unwrap().is_empty());
    crate::services::tests::fixture::cleanup(&db, fixture).await.unwrap();
}
