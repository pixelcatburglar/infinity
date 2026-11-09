//! Real PostgreSQL regressions for destination-parent locks.
use super::*;
use crate::{dao::items::{inventory, quantities::{self, QuantityChange}, transfers}, db::Db};
use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};
use tokio::sync::{Barrier, oneshot};

/// Return the holder backend so an internally opened service transaction can be observed.
pub(crate) async fn backend_pid(tx: &tokio_postgres::Transaction<'_>) -> i32 {
    tx.query_one("SELECT pg_backend_pid() AS pid", &[]).await.unwrap().get("pid")
}

/// Wait until PostgreSQL reports a backend blocked by this fixture's holder.
pub(crate) async fn wait_blocked(db: &Db, blocker_pid: i32) -> i32 {
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        loop {
            if let Some(row) = db.query_opt(
                "SELECT pid FROM pg_stat_activity WHERE wait_event_type = 'Lock' \
                 AND $1 = ANY(pg_blocking_pids(pid)) ORDER BY pid LIMIT 1",
                &[&blocker_pid],
            ).await.unwrap() {
                return row.get("pid");
            }
            tokio::task::yield_now().await;
        }
    }).await.expect("backend did not block on the fixture transaction")
}

async fn connection() -> tokio_postgres::Client {
    let cfg = crate::config::tests::test_database();
    let (client, connection) = cfg.to_pg_config().unwrap().connect(tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    client
}

async fn fixture(db: &Db) -> (i64, i64) {
    let child = transfers::insert(db, 1, 101, 102, 4, 1).await.unwrap();
    let parent = transfers::insert(db, 1, 101, 102, 4, 1).await.unwrap();
    db.execute("UPDATE game.items SET location_id = $2 WHERE item_id = $1", &[&child, &parent]).await.unwrap();
    (parent, child)
}

async fn cleanup(db: &Db, parent: i64) {
    db.execute("DELETE FROM game.items WHERE item_id = $1 OR location_id = $1", &[&parent]).await.unwrap();
}

#[tokio::test]
#[ignore = "requires a dedicated PostgreSQL database"]
async fn take_first_returns_fresh_parent_and_refuses_stale_insert() {
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let (parent, child) = fixture(&db).await;
    let mut a = connection().await;
    let mut b = connection().await;
    let take = a.transaction().await.unwrap();
    let closure = inventory::descendants(&take, &[parent]).await.unwrap();
    let requests: Vec<_> = closure.iter().map(|&item_id| LockRequest { item_id, strength: LockStrength::Update }).collect();
    let locks = lock(&take, &[], &requests).await.unwrap();
    transfers::hand_over(&take, &[transfers::Transfer { item_id: parent, owner: 103, location: 104, flag: 5 }], &closure, &locks, &[]).await.unwrap();
    let insert = b.transaction().await.unwrap();
    insert.batch_execute("SAVEPOINT probe").await.unwrap();
    let conflict = insert.query("SELECT item_id FROM game.items WHERE item_id = $1 FOR SHARE NOWAIT", &[&parent]).await.unwrap_err();
    assert_eq!(conflict.code(), Some(&tokio_postgres::error::SqlState::LOCK_NOT_AVAILABLE));
    insert.batch_execute("ROLLBACK TO SAVEPOINT probe").await.unwrap();
    let (ready_tx, ready_rx) = oneshot::channel();
    let task = async {
        ready_tx.send(()).unwrap();
        let rows = lock(&insert, &[], &[LockRequest { item_id: parent, strength: LockStrength::Share }]).await.unwrap();
        assert_eq!(rows.rows().len(), 1);
        let current = &rows.rows()[0];
        assert_eq!((current.owner_id, current.location_id, current.flag_id), (103, 104, 5));
        if current.owner_id == 101 {
            transfers::insert(&insert, 1, 101, parent, 4, 1).await.unwrap();
        }
        insert.commit().await.unwrap();
    };
    let release = async { ready_rx.await.unwrap(); take.commit().await.unwrap(); };
    tokio::time::timeout(std::time::Duration::from_secs(30), async { tokio::join!(task, release); }).await.unwrap();
    let rows = db.query("SELECT item_id, owner_id, location_id FROM game.items WHERE item_id = $1 OR location_id = $1 ORDER BY item_id", &[&parent]).await.unwrap();
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|r| r.get::<_, i64>("owner_id") == 103));
    assert!(rows.iter().any(|r| r.get::<_, i64>("item_id") == child && r.get::<_, i64>("location_id") == parent));
    cleanup(&db, parent).await;
}

#[tokio::test]
#[ignore = "requires a dedicated PostgreSQL database"]
async fn insert_first_share_compatibility_and_take_includes_child() {
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let (parent, older_child) = fixture(&db).await;
    assert!(older_child < parent);
    let mut a = connection().await;
    let mut b = connection().await;
    let mut c = connection().await;
    let insert = a.transaction().await.unwrap();
    let requests: Vec<_> = [older_child, parent, older_child].into_iter().map(|item_id| LockRequest { item_id, strength: LockStrength::Share }).collect();
    let rows = lock(&insert, &[], &requests).await.unwrap();
    assert_eq!(rows.rows().iter().map(|r| r.item_id).collect::<Vec<_>>(), vec![older_child, parent]);
    assert_eq!((rows.rows()[1].type_id, rows.rows()[1].quantity, rows.rows()[1].singleton), (1, 1, false));
    assert!(lock(&insert, &[], &[LockRequest { item_id: -1, strength: LockStrength::Share }]).await.unwrap().rows().is_empty());
    let other = b.transaction().await.unwrap();
    assert_eq!(lock(&other, &[], &[LockRequest { item_id: parent, strength: LockStrength::Share }]).await.unwrap().rows().len(), 1);
    other.commit().await.unwrap();
    let child = transfers::insert(&insert, 1, 101, parent, 4, 1).await.unwrap();
    let take = c.transaction().await.unwrap();
    take.batch_execute("SAVEPOINT probe").await.unwrap();
    let conflict = take.query("SELECT item_id FROM game.items WHERE item_id = $1 FOR UPDATE NOWAIT", &[&parent]).await.unwrap_err();
    assert_eq!(conflict.code(), Some(&tokio_postgres::error::SqlState::LOCK_NOT_AVAILABLE));
    take.batch_execute("ROLLBACK TO SAVEPOINT probe").await.unwrap();
    let (ready_tx, ready_rx) = oneshot::channel();
    let operation = async {
        ready_tx.send(()).unwrap();
        // Wait for the parent before capturing the newly committed child.
        lock(&take, &[], &[LockRequest { item_id: parent, strength: LockStrength::Update }]).await.unwrap();
        let closure = inventory::descendants(&take, &[parent]).await.unwrap();
        let requests: Vec<_> = closure.iter().map(|&item_id| LockRequest { item_id, strength: LockStrength::Update }).collect();
        let locks = lock(&take, &[], &requests).await.unwrap();
        transfers::hand_over(&take, &[transfers::Transfer { item_id: parent, owner: 103, location: 104, flag: 5 }], &closure, &locks, &[]).await.unwrap();
        take.commit().await.unwrap();
    };
    let release = async { ready_rx.await.unwrap(); insert.commit().await.unwrap(); };
    tokio::time::timeout(std::time::Duration::from_secs(30), async { tokio::join!(operation, release); }).await.unwrap();
    let rows = db.query("SELECT item_id, owner_id, location_id FROM game.items WHERE item_id = $1 OR location_id = $1", &[&parent]).await.unwrap();
    assert_eq!(rows.len(), 3);
    assert!(rows.iter().all(|r| r.get::<_, i64>("owner_id") == 103));
    assert!(rows.iter().any(|r| r.get::<_, i64>("item_id") == child && r.get::<_, i64>("location_id") == parent));
    cleanup(&db, parent).await;
}

#[tokio::test]
#[ignore = "requires a dedicated PostgreSQL database"]
async fn constructed_deadlock_retries_whole_transaction_once() {
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let parent = transfers::insert(&db, 1, 101, 102, 4, 1).await.unwrap();
    let source = transfers::insert(&db, 1, 101, 102, 4, 2).await.unwrap();
    let barrier = Arc::new(Barrier::new(2));
    let insert_attempts = Arc::new(AtomicUsize::new(0));
    let take_attempts = Arc::new(AtomicUsize::new(0));
    let insert_db = db.clone();
    let take_db = db.clone();
    let ib = barrier.clone();
    let tb = barrier.clone();
    let ia = insert_attempts.clone();
    let ta = take_attempts.clone();
    let insert = insert_db.transaction(move |tx, outbox: &mut crate::db::outbox::Outbox| {
        let attempt = ia.fetch_add(1, Ordering::SeqCst);
        let barrier = ib.clone();
        Box::pin(async move {
            outbox.notify_pilot(101, "test", Vec::new());
            // Deliberately reversed test-only locks construct a deadlock for the retry runner;
            // they do not demonstrate the production lock protocol.
            lock(tx, &[], &[LockRequest { item_id: source, strength: LockStrength::Update }]).await?;
            if attempt == 0 { barrier.wait().await; }
            let rows = lock(tx, &[], &[LockRequest { item_id: parent, strength: LockStrength::Share }]).await?;
            if rows.rows().iter().any(|row| row.item_id == parent && row.owner_id == 101) {
                let change = QuantityChange { type_id: 1, expected_quantity: 2, quantity: Some(1), owner_id: 101, location_id: 102, flag_id: 4, singleton: false };
                assert!(quantities::apply(tx, &std::collections::BTreeMap::from([(source, change)])).await?);
                transfers::insert(tx, 1, 101, parent, 4, 1).await?;
            }
            Ok::<_, db::Error>(())
        })
    }, |(), outbox: crate::db::outbox::Outbox| async move { Ok::<_, db::Error>(outbox.into_entries().len()) });
    let take = take_db.transaction(move |tx, outbox: &mut crate::db::outbox::Outbox| {
        let attempt = ta.fetch_add(1, Ordering::SeqCst);
        let barrier = tb.clone();
        Box::pin(async move {
            outbox.notify_pilot(103, "test", Vec::new());
            lock(tx, &[], &[LockRequest { item_id: parent, strength: LockStrength::Update }]).await?;
            if attempt == 0 { barrier.wait().await; }
            lock(tx, &[], &[LockRequest { item_id: source, strength: LockStrength::Update }]).await?;
            let closure = inventory::descendants(tx, &[parent]).await?;
            let requests: Vec<_> = closure.iter().map(|&item_id| LockRequest { item_id, strength: LockStrength::Update }).collect();
            let locks = lock(tx, &[], &requests).await?;
            transfers::hand_over(tx, &[transfers::Transfer { item_id: parent, owner: 103, location: 104, flag: 5 }], &closure, &locks, &[]).await?;
            Ok::<_, db::Error>(())
        })
    }, |(), outbox: crate::db::outbox::Outbox| async move { Ok::<_, db::Error>(outbox.into_entries().len()) });
    let (insert_result, take_result) = tokio::time::timeout(std::time::Duration::from_secs(30), async { tokio::join!(insert, take) }).await.unwrap();
    assert_eq!(insert_result.unwrap(), 1);
    assert_eq!(take_result.unwrap(), 1);
    assert_eq!(insert_attempts.load(Ordering::SeqCst) + take_attempts.load(Ordering::SeqCst), 3);
    let source_row = db.query_one("SELECT quantity FROM game.items WHERE item_id = $1", &[&source]).await.unwrap();
    let quantity: i64 = source_row.get("quantity");
    let rows = db.query("SELECT owner_id, location_id FROM game.items WHERE item_id = $1 OR location_id = $1", &[&parent]).await.unwrap();
    assert!((1..=2).contains(&rows.len()));
    assert_eq!(quantity, if rows.len() == 2 { 1 } else { 2 });
    assert!(rows.iter().all(|r| r.get::<_, i64>("owner_id") == 103));
    cleanup(&db, parent).await;
    db.execute("DELETE FROM game.items WHERE item_id = $1", &[&source]).await.unwrap();
}

#[tokio::test]
#[ignore = "requires a dedicated PostgreSQL database"]
async fn nested_reservation_and_outbox_survive_rollback() {
    use crate::dao::items::pending_reloads;
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let (parent, child) = fixture(&db).await;
    let reservation = pending_reloads::insert(&db, 101, child, child, 4, 1, 1, 4, parent).await.unwrap();
    let dispatched = Arc::new(AtomicUsize::new(0));
    let count = dispatched.clone();
    let result: Result<(), db::Error> = db.transaction(|tx, outbox: &mut crate::db::outbox::Outbox| Box::pin(async move {
        let rows = lock(tx, &[], &[LockRequest { item_id: parent, strength: LockStrength::Share }]).await?;
        assert_eq!(rows.rows()[0].owner_id, 101);
        assert!(pending_reloads::take(tx, reservation).await?.is_some());
        transfers::insert(tx, 1, 101, parent, 4, 1).await?;
        outbox.notify_pilot(101, "test", Vec::new());
        tx.execute("SELECT nonexistent_parent_lock_test_column", &[]).await.map_err(db::Error::from)?;
        Ok(())
    }), move |(), _: crate::db::outbox::Outbox| async move {
        count.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }).await;
    assert!(result.is_err());
    assert_eq!(dispatched.load(Ordering::SeqCst), 0);
    let remaining = pending_reloads::for_source(&db, parent).await.unwrap();
    assert_eq!(remaining.len(), 1);
    assert_eq!((remaining[0].reload_id, remaining[0].owner_id, remaining[0].source_location_id), (reservation, 101, parent));
    let descendants = inventory::descendants(&db, &[parent]).await.unwrap();
    assert_eq!(descendants, vec![child, parent]);
    pending_reloads::take(&db, reservation).await.unwrap();
    cleanup(&db, parent).await;
}

#[tokio::test]
#[ignore = "requires a dedicated PostgreSQL database"]
async fn hull_before_older_source_crossing_commits_without_retry() {
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let (hull, source) = fixture(&db).await;
    assert!(source < hull);
    let mut a = connection().await;
    let mut b = connection().await;
    let mut probe_client = connection().await;
    let first = a.transaction().await.unwrap();
    let second = b.transaction().await.unwrap();
    let pid = backend_pid(&first).await;
    let requests = [
        LockRequest { item_id: source, strength: LockStrength::Update },
        LockRequest { item_id: hull, strength: LockStrength::Update },
    ];
    let first_count = AtomicUsize::new(0);
    let second_count = AtomicUsize::new(0);
    let (ready_tx, ready_rx) = oneshot::channel();
    let first_op = async {
        first_count.fetch_add(1, Ordering::SeqCst);
        let held = lock(&first, &[hull], &[LockRequest { item_id: hull, strength: LockStrength::Update }]).await.unwrap();
        assert!(held.is_write_locked(hull));
        ready_tx.send(()).unwrap();
        wait_blocked(&db, pid).await;
        let probe = probe_client.transaction().await.unwrap();
        assert_eq!(probe.query("SELECT item_id FROM game.items WHERE item_id = $1 FOR UPDATE NOWAIT", &[&source]).await.unwrap().len(), 1);
        probe.rollback().await.unwrap();
        let held = lock(&first, &[hull], &requests).await.unwrap();
        assert!(held.is_write_locked(hull) && held.is_write_locked(source));
        first.commit().await.unwrap();
    };
    let second_op = async {
        second_count.fetch_add(1, Ordering::SeqCst);
        ready_rx.await.unwrap();
        let held = lock(&second, &[hull], &requests).await.unwrap();
        assert!(held.is_write_locked(hull) && held.is_write_locked(source));
        second.commit().await.unwrap();
    };
    tokio::time::timeout(std::time::Duration::from_secs(30), async { tokio::join!(first_op, second_op); }).await.expect("crossing timed out");
    assert_eq!(first_count.load(Ordering::SeqCst), 1);
    assert_eq!(second_count.load(Ordering::SeqCst), 1);
    cleanup(&db, hull).await;
}
