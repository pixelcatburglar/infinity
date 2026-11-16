use super::*;
use crate::services::CallError;
use deadpool_postgres::{Manager, ManagerConfig, Pool, RecyclingMethod};
use std::sync::{Arc, atomic::{AtomicI64, AtomicU64, Ordering}};
use tokio_postgres::NoTls;

#[test]
fn stale_snapshot_is_retryable_while_other_markers_are_not() {
    assert!(Error::StaleSnapshot("pre-lock snapshot changed").is_retryable_transaction());
    assert!(!Error::InvalidItemLockRequest("closure not locked").is_retryable_transaction());
    assert!(!Error::Progress(anyhow::anyhow!("preparation failed")).is_retryable_transaction());
}

#[test]
fn stale_snapshot_retries_through_internal_causes_only() {
    let typed: CallError = anyhow::Error::new(Error::StaleSnapshot("pre-lock snapshot changed")).context("loss capture").into();
    assert!(typed.is_retryable_transaction());
    let unrelated: CallError = anyhow::Error::new(Error::InvalidItemLockRequest("closure not locked")).context("loss capture").into();
    assert!(!unrelated.is_retryable_transaction());
    let stale_text: CallError = anyhow::anyhow!("stale operation snapshot: plain text").into();
    assert!(!stale_text.is_retryable_transaction());
}

#[test]
fn only_deadlocks_and_serialization_failures_retry() {
    assert!(retryable_sqlstate(Some(&SqlState::T_R_DEADLOCK_DETECTED)));
    assert!(retryable_sqlstate(Some(&SqlState::T_R_SERIALIZATION_FAILURE)));
    assert!(!retryable_sqlstate(Some(&SqlState::UNIQUE_VIOLATION)));
    assert!(!retryable_sqlstate(None));
}

#[test]
fn user_and_non_sql_internal_errors_do_not_retry() {
    let user = CallError::User { msg: "refused".into(), dict: Vec::new() };
    let internal = CallError::Internal(anyhow::anyhow!("unrelated failure").context("wrapped"));
    assert!(!user.is_retryable_transaction());
    assert!(!internal.is_retryable_transaction());
}

#[test]
fn retry_budget_stays_one_retry() {
    assert_eq!(MAX_TRANSACTION_RETRIES, 1);
}

#[tokio::test]
#[ignore = "requires a dedicated PostgreSQL database"]
async fn loss_tolerant_commit_scopes_off_per_attempt_and_restores() {
    const META: &str = "SELECT pg_backend_pid() AS pid, current_setting('synchronous_commit') AS sc";
    const MARKERS: &str = "SELECT count(*) AS n FROM async_commit_marker";
    let manager = Manager::from_config(crate::config::tests::test_database().to_pg_config().unwrap(), NoTls, ManagerConfig { recycling_method: RecyclingMethod::Verified });
    let pool = Pool::builder(manager).max_size(1).runtime(deadpool_postgres::Runtime::Tokio1).build().unwrap();
    let connect = |loss_tolerant_async_commit| Db { pool: pool.clone(), online: Arc::new(AtomicI64::new(0)), progress_registry: Arc::new(crate::progress::Registry::default()), loss_tolerant_async_commit };
    let db = connect(true);
    db.execute("CREATE TEMPORARY TABLE async_commit_marker (n int)", &[]).await.unwrap();
    let row = db.query_one(META, &[]).await.unwrap();
    let (pid, baseline): (i32, String) = (row.get("pid"), row.get("sc"));
    assert_ne!(baseline, "off", "dedicated test database must not already run asynchronous commit");
    db.transaction::<_, _, Error, (), _, _, _>(|tx, _| { let baseline = baseline.clone(); Box::pin(async move {
        let row = tx.query_one(META, &[]).await.unwrap();
        assert_eq!((row.get::<_, i32>("pid"), row.get::<_, String>("sc")), (pid, baseline));
        Ok::<(), Error>(())
    }) }, |(), ()| async { Ok(()) }).await.unwrap();
    db.transaction_with_commit::<_, _, Error, (), _, _, _>(CommitMode::LossTolerant, |tx, _| Box::pin(async move {
        assert_eq!(tx.query_one(META, &[]).await.unwrap().get::<_, String>("sc"), "off");
        tx.execute("INSERT INTO async_commit_marker VALUES (1)", &[]).await.unwrap();
        Ok::<(), Error>(())
    }), |(), ()| async { Ok(()) }).await.unwrap();
    let row = db.query_one(META, &[]).await.unwrap();
    assert_eq!((row.get::<_, i32>("pid"), row.get::<_, String>("sc").as_str()), (pid, baseline.as_str()));
    assert_eq!(db.query_one(MARKERS, &[]).await.unwrap().get::<_, i64>("n"), 1);
    let failed = db.transaction_with_commit::<_, _, Error, (), _, _, _>(CommitMode::LossTolerant, |tx, _| Box::pin(async move {
        assert_eq!(tx.query_one(META, &[]).await.unwrap().get::<_, String>("sc"), "off");
        tx.execute("INSERT INTO async_commit_marker VALUES (2)", &[]).await.unwrap();
        Err(Error::InvalidItemLockRequest("forced rollback"))
    }), |(), ()| async { Ok(()) }).await;
    assert!(matches!(failed, Err(Error::InvalidItemLockRequest(_))));
    let row = db.query_one(META, &[]).await.unwrap();
    assert_eq!((row.get::<_, i32>("pid"), row.get::<_, String>("sc").as_str()), (pid, baseline.as_str()));
    assert_eq!(db.query_one(MARKERS, &[]).await.unwrap().get::<_, i64>("n"), 1);
    let (attempts, dispatched) = (AtomicU64::new(0), AtomicU64::new(0));
    db.transaction_with_commit::<_, _, Error, (), _, _, _>(CommitMode::LossTolerant, |tx, _| {
        let attempt = attempts.fetch_add(1, Ordering::Relaxed);
        Box::pin(async move {
            assert_eq!(tx.query_one(META, &[]).await.unwrap().get::<_, String>("sc"), "off");
            if attempt == 0 { return Err(Error::StaleSnapshot("forced retry")); }
            tx.execute("INSERT INTO async_commit_marker VALUES (3)", &[]).await.unwrap();
            Ok::<(), Error>(())
        })
    }, |(), ()| { dispatched.fetch_add(1, Ordering::Relaxed); async { Ok(()) } }).await.unwrap();
    assert_eq!((attempts.into_inner(), dispatched.into_inner()), (2, 1));
    assert_eq!(db.query_one(MARKERS, &[]).await.unwrap().get::<_, i64>("n"), 2);
    connect(false).transaction_with_commit::<_, _, Error, (), _, _, _>(CommitMode::LossTolerant, |tx, _| { let baseline = baseline.clone(); Box::pin(async move {
        assert_eq!(tx.query_one(META, &[]).await.unwrap().get::<_, String>("sc"), baseline);
        Ok::<(), Error>(())
    }) }, |(), ()| async { Ok(()) }).await.unwrap();
    db.execute("DROP TABLE async_commit_marker", &[]).await.unwrap();
}
