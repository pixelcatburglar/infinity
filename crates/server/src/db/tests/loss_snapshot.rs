//! Real PostgreSQL coverage for the whole-transaction StaleSnapshot retry contract.

use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use crate::services::CallError;

const MARK: i64 = 101;
const JOURNALLED: &str = "SELECT COUNT(*) AS n FROM game.progress_events WHERE kind = 'ShipLost' AND subject_character_id = $1";

async fn setup(db: &Db) -> i64 {
    let box_id: i64 = db
        .query_one("INSERT INTO game.items (type_id, owner_id, location_id, flag_id, quantity) VALUES (1, $1, $1, 1, 1) RETURNING item_id", &[&MARK])
        .await
        .unwrap()
        .get("item_id");
    db.execute("INSERT INTO game.ship_dirt (ship_item_id, dirt_timestamp) VALUES ($1, 1)", &[&box_id]).await.unwrap();
    box_id
}

/// Every fixture row lives under `game.items` so failed attempts and leaked rows
/// restore the pre-test table when `cleanup` runs.
async fn cleanup(db: &Db, box_id: i64) {
    db.execute("DELETE FROM game.items WHERE item_id = $1 OR owner_id = $2 OR location_id = $2", &[&box_id, &MARK]).await.unwrap();
    assert!(db.query_opt("SELECT 1 FROM game.ship_dirt WHERE ship_item_id = $1", &[&box_id]).await.unwrap().is_none());
}

async fn item_rows(db: &Db, box_id: i64) -> Vec<(i64, i32)> {
    db.query("SELECT item_id, type_id FROM game.items WHERE item_id = $1 OR location_id = $1 ORDER BY item_id", &[&box_id])
        .await
        .unwrap()
        .iter()
        .map(|row| (row.get("item_id"), row.get("type_id")))
        .collect()
}

#[tokio::test]
#[ignore = "requires a dedicated PostgreSQL database"]
async fn stale_snapshot_retries_once_and_only_second_attempt_commits() {
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let box_id = setup(&db).await;
    let attempts = Arc::new(AtomicUsize::new(0));
    let count = attempts.clone();
    let result: Result<(i64, usize), CallError> = db
        .transaction(
            move |tx, outbox: &mut crate::db::outbox::Outbox| {
                let attempt = count.fetch_add(1, Ordering::SeqCst);
                Box::pin(async move {
                    // The first attempt's write and outbox entry roll back with
                    // its transaction and never reach the committed rows.
                    let type_id = if attempt == 0 { 1001 } else { 1002 };
                    tx.execute("INSERT INTO game.items (type_id, owner_id, location_id, flag_id, quantity) VALUES ($1, $2, $3, 1, 1)", &[&type_id, &MARK, &box_id]).await.map_err(Error::from)?;
                    outbox.notify_pilot(MARK, "OnItemChange", Vec::new());
                    if attempt == 0 {
                        return Err(CallError::from(Error::StaleSnapshot("pre-lock snapshot changed")));
                    }
                    let rows = tx.query("SELECT type_id FROM game.items WHERE location_id = $1 ORDER BY item_id", &[&box_id]).await.map_err(Error::from)?;
                    Ok::<i64, CallError>(rows.len() as i64)
                })
            },
            |captured: i64, outbox: crate::db::outbox::Outbox| async move { Ok::<_, CallError>((captured, outbox.into_entries().len())) },
        )
        .await;
    let outcome = match result {
        Ok((captured, entries)) => {
            assert_eq!(captured, 1, "the retried attempt observes an empty location");
            assert_eq!(entries, 1, "only the committed attempt's notification survives");
            Ok(())
        }
        Err(error) => Err(format!("{error:?}")),
    };
    let rows = item_rows(&db, box_id).await;
    let journalled: i64 = db.query_one(JOURNALLED, &[&MARK]).await.unwrap().get("n");
    cleanup(&db, box_id).await;
    outcome.unwrap();
    assert_eq!(attempts.load(Ordering::SeqCst), 2, "the whole transaction ran exactly twice");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0], (box_id, 1));
    assert_eq!(rows[1].1, 1002, "the first attempt's type_id 1001 row rolled back");
    assert_eq!(journalled, 0, "notifications do not reach the durable progress journal");
}

#[tokio::test]
#[ignore = "requires a dedicated PostgreSQL database"]
async fn always_stale_snapshot_returns_internal_after_two_attempts() {
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let box_id = setup(&db).await;

    let attempts = Arc::new(AtomicUsize::new(0));
    let count = attempts.clone();
    let result: Result<(), CallError> = db
        .transaction(
            move |tx, outbox: &mut crate::db::outbox::Outbox| {
                count.fetch_add(1, Ordering::SeqCst);
                Box::pin(async move {
                    tx.execute("INSERT INTO game.items (type_id, owner_id, location_id, flag_id, quantity) VALUES (1, $1, $2, 1, 1)", &[&MARK, &box_id]).await.map_err(Error::from)?;
                    outbox.notify_pilot(MARK, "OnItemChange", Vec::new());
                    Err(CallError::from(Error::StaleSnapshot("pre-lock snapshot changed")))
                })
            },
            |(), _: crate::db::outbox::Outbox| async move { panic!("dispatch must not run for a failed transaction") },
        )
        .await;
    let failure = match result {
        Err(CallError::Internal(error)) => {
            assert!(error.chain().any(|cause| matches!(cause.downcast_ref::<Error>(), Some(Error::StaleSnapshot(_)))));
            Ok(())
        }
        Err(error) => Err(format!("expected an Internal failure, got {error:?}")),
        Ok(()) => Err("expected an Internal failure, got success".to_owned()),
    };
    let rows = item_rows(&db, box_id).await;
    let journalled: i64 = db.query_one(JOURNALLED, &[&MARK]).await.unwrap().get("n");
    cleanup(&db, box_id).await;
    failure.unwrap();
    assert_eq!(attempts.load(Ordering::SeqCst), 2, "the retry budget admits no third invocation");
    assert_eq!(rows.len(), 1, "both attempts' tentative rows rolled back");
    assert_eq!(journalled, 0, "no outbox effect is published for a failed transaction");
}

#[tokio::test]
#[ignore = "requires a dedicated PostgreSQL database"]
async fn invalid_lock_request_and_plain_internal_never_retry() {
    for case in ["invalid lock request", "plain internal"] {
        let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
        let box_id = setup(&db).await;

        let attempts = Arc::new(AtomicUsize::new(0));
        let count = attempts.clone();
        let result: Result<(), CallError> = db
            .transaction(
                move |tx, outbox: &mut crate::db::outbox::Outbox| {
                    count.fetch_add(1, Ordering::SeqCst);
                    Box::pin(async move {
                        tx.execute("INSERT INTO game.items (type_id, owner_id, location_id, flag_id, quantity) VALUES (1, $1, $2, 1, 1)", &[&MARK, &box_id]).await.map_err(Error::from)?;
                        outbox.notify_pilot(MARK, "OnItemChange", Vec::new());
                        Err(match case {
                            "invalid lock request" => CallError::from(Error::InvalidItemLockRequest("closure not locked")),
                            _ => crate::services::db_err(anyhow::anyhow!("unrelated failure")),
                        })
                    })
                },
                |(), _: crate::db::outbox::Outbox| async move { panic!("dispatch must not run for a failed transaction") },
            )
            .await;
        let failure = match result {
            Err(CallError::Internal(_)) => Ok(()),
            Err(CallError::User { .. }) => Err(format!("{case}: expected an Internal failure, got a user refusal")),
            Ok(()) => Err(format!("{case}: expected an Internal failure, got success")),
        };
        let rows = item_rows(&db, box_id).await;
        let journalled: i64 = db.query_one(JOURNALLED, &[&MARK]).await.unwrap().get("n");
        cleanup(&db, box_id).await;
        failure.unwrap();
        assert_eq!(attempts.load(Ordering::SeqCst), 1, "{case} must not rerun the operation");
        assert_eq!(rows.len(), 1, "{case} attempt's tentative row rolled back");
        assert_eq!(journalled, 0, "{case} publishes no outbox effect");
    }
}
