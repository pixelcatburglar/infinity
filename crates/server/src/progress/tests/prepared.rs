//! Prepared receipt validation and rollback against the real journal.
use super::*;
use std::sync::Arc;

struct Plan { key: DeliveryKey, stale: bool }
impl PreparedDelivery for Plan {
    fn key(&self) -> &DeliveryKey { &self.key }
    fn prelock<'a, 'tx>(&'a self, _: &'a Transaction<'tx>) -> TxFuture<'a, (), ApplyError> {
        Box::pin(async move {
            if self.stale { return Err(ApplyError::Reprepare); }
            Ok(())
        })
    }
    fn apply<'a, 'tx>(&'a self, tx: &'a Transaction<'tx>, _: &'a mut Outbox, _: &'a Sde, _: &'a EventEnvelope) -> TxFuture<'a, (), ApplyError> {
        Box::pin(async move {
            tx.execute("INSERT INTO progress_test_effects DEFAULT VALUES", &[]).await.map_err(crate::db::Error::from)?;
            Ok(())
        })
    }
}
fn prepare(_: ContentProgressResources, _: DeliveryKey) -> std::pin::Pin<Box<dyn Future<Output = Result<PrepareOutcome, CallError>> + Send>> {
    Box::pin(async { Err(CallError::Internal(anyhow::anyhow!("unexpected preparation"))) })
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn prepared_mode_rejects_mismatch_and_rolls_back_stale_prelock() {
    crate::tests::init_config();
    let config = crate::config::tests::test_database();
    let db = crate::db::Db::connect(&config).await.unwrap();
    let sde = Sde::load(&db).await.unwrap();
    let (mut client, connection) = tokio_postgres::connect(&config.url, tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let tx = client.transaction().await.unwrap();
    tx.batch_execute("CREATE TEMP TABLE progress_test_effects (id bigserial) ON COMMIT DROP").await.unwrap();
    let registration = DurableRegistration { consumer_key: "prepared-test", kinds: &["SkillTrained"], mode: DeliveryMode::Prepared(prepare) };
    register_fixture(&tx, &registration).await;
    let event = GameEvent::SkillTrained { char_id: 1, type_id: 2, level: 3 };
    let entry = crate::dao::progress::JournalEvent { source_namespace: format!("prepared-test-{}", crate::handshake::random_u64()), source_key: "0".into(), schema_version: codec::SCHEMA_VERSION, kind: event.kind().into(), occurred_at: 1, recorded_at: 1, payload: codec::encode(&event).unwrap(), context: "{}".into(), origin: "test".into(), ordinal: 0, subject_character_id: Some(1) };
    let inserted = crate::dao::progress::insert_batch(&tx, &[entry]).await.unwrap();
    let key = DeliveryKey { consumer_key: registration.consumer_key.into(), event_id: inserted[0].1.clone() };
    let foreign_before = foreign_snapshot(&tx, registration.consumer_key, &key.event_id).await;
    let plan = Arc::new(Plan { key: key.clone(), stale: true });
    assert!(matches!(apply_once_tx(&tx, &mut Outbox::default(), &sde, &registration, key.clone(), None, ClaimMode::Blocking).await, Err(ApplyError::Call(_))));
    let direct = DurableRegistration { mode: DeliveryMode::Direct(handler), ..registration };
    assert!(matches!(apply_once_tx(&tx, &mut Outbox::default(), &sde, &direct, key.clone(), Some(plan.as_ref()), ClaimMode::Blocking).await, Err(ApplyError::Call(_))));
    let other = Plan { key: DeliveryKey { consumer_key: key.consumer_key.clone(), event_id: "00000000-0000-0000-0000-000000000000".into() }, stale: false };
    assert!(matches!(apply_once_tx(&tx, &mut Outbox::default(), &sde, &registration, key.clone(), Some(&other), ClaimMode::Blocking).await, Err(ApplyError::Call(_))));
    tx.batch_execute("SAVEPOINT stale").await.unwrap();
    assert!(matches!(apply_once_tx(&tx, &mut Outbox::default(), &sde, &registration, key.clone(), Some(plan.as_ref()), ClaimMode::Background).await, Err(ApplyError::Reprepare)));
    tx.batch_execute("ROLLBACK TO SAVEPOINT stale").await.unwrap();
    let count: i64 = tx.query_one("SELECT count(*) AS count FROM progress_test_effects", &[]).await.unwrap().get("count");
    assert_eq!(count, 0);
    assert!(matches!(crate::dao::progress::delivery(&tx, &key).await.unwrap(), crate::dao::progress::DeliveryState::Pending(_)));
    assert_eq!(foreign_snapshot(&tx, registration.consumer_key, &key.event_id).await, foreign_before);
    tx.rollback().await.unwrap();
}


static PRELOCKED: tokio::sync::Notify = tokio::sync::Notify::const_new();
static PRELOCK_COUNT: AtomicUsize = AtomicUsize::new(0);
static APPLIED_COUNT: AtomicUsize = AtomicUsize::new(0);
static PUBLISHED_COUNT: AtomicUsize = AtomicUsize::new(0);
static MISSING_PACKAGE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

static RETRY_COUNT: AtomicUsize = AtomicUsize::new(0);

struct RetryPlan { key: DeliveryKey }
impl PreparedDelivery for RetryPlan {
    fn key(&self) -> &DeliveryKey { &self.key }
    fn prelock<'a, 'tx>(&'a self, tx: &'a Transaction<'tx>) -> TxFuture<'a, (), ApplyError> {
        Box::pin(async move {
            if RETRY_COUNT.fetch_add(1, Ordering::SeqCst) == 0 {
                tx.batch_execute("DO $$ BEGIN RAISE EXCEPTION 'retry fixture' USING ERRCODE = '40001'; END $$").await.map_err(crate::db::Error::from)?;
            }
            Ok(())
        })
    }
    fn apply<'a, 'tx>(&'a self, _: &'a Transaction<'tx>, _: &'a mut Outbox, _: &'a Sde, _: &'a EventEnvelope) -> TxFuture<'a, (), ApplyError> {
        Box::pin(async { Ok(()) })
    }
}
struct DrainPlan { key: DeliveryKey, source: String }
impl PreparedDelivery for DrainPlan {
    fn key(&self) -> &DeliveryKey { &self.key }
    fn prelock<'a, 'tx>(&'a self, _: &'a Transaction<'tx>) -> TxFuture<'a, (), ApplyError> {
        Box::pin(async move {
            PRELOCK_COUNT.fetch_add(1, Ordering::SeqCst);
            PRELOCKED.notify_one();
            Ok(())
        })
    }
    fn apply<'a, 'tx>(&'a self, tx: &'a Transaction<'tx>, outbox: &'a mut Outbox, _: &'a Sde, event: &'a EventEnvelope) -> TxFuture<'a, (), ApplyError> {
        Box::pin(async move {
            if event.source_namespace != self.source { return Err(ApplyError::Reprepare); }
            crate::dao::lua_store::set(tx, &self.source, "applied", "1").await?;
            APPLIED_COUNT.fetch_add(1, Ordering::SeqCst);
            outbox.event_from(crate::db::outbox::SourceKey::canonical("prepared-test", &self.source, 0), GameEvent::SkillStateChanged { char_id: 1 });
            Ok(())
        })
    }
}
fn drain_prepare(resources: ContentProgressResources, key: DeliveryKey) -> std::pin::Pin<Box<dyn Future<Output = Result<PrepareOutcome, CallError>> + Send>> {
    Box::pin(async move {
        if MISSING_PACKAGE.load(Ordering::SeqCst) {
            resources.policies.resolve("missing-progress-test", "missing").await?;
        }
        match crate::dao::progress::delivery(&resources.db, &key).await? {
            crate::dao::progress::DeliveryState::Pending(event) => Ok(PrepareOutcome::Ready(Arc::new(DrainPlan { key, source: event.source_namespace }))),
            crate::dao::progress::DeliveryState::Applied => Ok(PrepareOutcome::AlreadyApplied),
            crate::dao::progress::DeliveryState::Missing => Err(CallError::Internal(anyhow::anyhow!("missing receipt"))),
        }
    })
}
fn published(event: &GameEvent) {
    if matches!(event, GameEvent::SkillStateChanged { char_id: 1 }) {
        PUBLISHED_COUNT.fetch_add(1, Ordering::SeqCst);
    }
}
fn drain_job() -> crate::scheduler::Job {
    crate::scheduler::Job { id: DRAIN_KIND.into(), kind: DRAIN_KIND.into(), due_at: 1, payload: "{}".into() }
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn real_drain_provider_prelock_claim_and_commit() {
    const CHILD: &str = "INFINITY_PROGRESS_DRAIN_TEST_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "progress::tests::prepared::real_drain_provider_prelock_claim_and_commit", "--ignored", "--nocapture"])
            .env(CHILD, "1").status().unwrap();
        assert!(status.success());
        return;
    }
    crate::tests::init_config();
    let config = crate::config::tests::test_database();
    let registration = DurableRegistration { consumer_key: "prepared-drain-test", kinds: &["SkillTrained"], mode: DeliveryMode::Prepared(drain_prepare) };
    let db = crate::db::Db::connect(&config).await.unwrap().with_progress_registry(Registry::new(vec![published], vec![registration]).unwrap());
    register_fixture(&db, &registration).await;
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let world = crate::space::World::new(db.clone(), sde.clone(), 1);
    let event = GameEvent::SkillTrained { char_id: 1, type_id: 2, level: 3 };
    let namespace = format!("prepared-drain-{}", crate::handshake::random_u64());
    let entry = crate::dao::progress::JournalEvent { source_namespace: namespace.clone(), source_key: "0".into(), schema_version: codec::SCHEMA_VERSION, kind: event.kind().into(), occurred_at: 1, recorded_at: 1, payload: codec::encode(&event).unwrap(), context: "{}".into(), origin: "test".into(), ordinal: 0, subject_character_id: Some(1) };
    let inserted = crate::dao::progress::insert_batch(&db, &[entry]).await.unwrap();
    let key = DeliveryKey { consumer_key: registration.consumer_key.into(), event_id: inserted[0].1.clone() };
    let foreign_before = foreign_snapshot(&db, registration.consumer_key, &key.event_id).await;

    // No host: preparation is rejected and the scheduled pass retains the receipt.
    drain(db.clone(), world.clone(), drain_job()).await.unwrap();
    let row = db.query_one("SELECT state,attempts FROM game.progress_deliveries WHERE consumer_key=$1 AND event_id=$2::text::uuid", &[&key.consumer_key, &key.event_id]).await.unwrap();
    assert_eq!(row.get::<_, String>("state"), "pending");
    assert_eq!(row.get::<_, i32>("attempts"), 1);
    assert_eq!(PRELOCK_COUNT.load(Ordering::SeqCst), 0);

    let dir = std::env::temp_dir().join(format!("progress-drain-lua-{}", crate::handshake::random_u64()));
    std::fs::create_dir(&dir).unwrap();
    let mut lua_config = crate::config::Config::default();
    lua_config.lua.scripts_dir = dir.to_string_lossy().into_owned();
    let hub = crate::chat::Hub::new("test", db.clone(), world.clone());
    crate::lua::start(&lua_config, &db, &sde, &world, &hub).await.unwrap();
    db.execute("UPDATE game.progress_deliveries SET next_attempt_at=0 WHERE consumer_key=$1 AND event_id=$2::text::uuid", &[&key.consumer_key, &key.event_id]).await.unwrap();
    MISSING_PACKAGE.store(true, Ordering::SeqCst);
    drain(db.clone(), world.clone(), drain_job()).await.unwrap();
    let row = db.query_one("SELECT state,attempts FROM game.progress_deliveries WHERE consumer_key=$1 AND event_id=$2::text::uuid", &[&key.consumer_key, &key.event_id]).await.unwrap();
    assert_eq!(row.get::<_, String>("state"), "pending");
    assert_eq!(row.get::<_, i32>("attempts"), 2);
    assert_eq!(PRELOCK_COUNT.load(Ordering::SeqCst), 0);
    MISSING_PACKAGE.store(false, Ordering::SeqCst);
    db.execute("UPDATE game.progress_deliveries SET next_attempt_at=0 WHERE consumer_key=$1 AND event_id=$2::text::uuid", &[&key.consumer_key, &key.event_id]).await.unwrap();

    let (mut client, connection) = tokio_postgres::connect(&config.url, tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let locked = client.transaction().await.unwrap();
    crate::dao::progress::lock_pending(&locked, &key).await.unwrap().unwrap();
    drain(db.clone(), world.clone(), drain_job()).await.unwrap();
    assert_eq!(PRELOCK_COUNT.load(Ordering::SeqCst), 1);
    assert_eq!(APPLIED_COUNT.load(Ordering::SeqCst), 0);
    assert!(matches!(crate::dao::progress::delivery(&db, &key).await.unwrap(), crate::dao::progress::DeliveryState::Pending(_)));

    // Blocking claim must wait for the same locked receipt, unlike background skip.
    let _ = PRELOCKED.notified().await;
    let plan = Arc::new(DrainPlan { key: key.clone(), source: namespace.clone() });
    let task_db = db.clone();
    let task_sde = sde.clone();
    let task_key = key.clone();
    let mut task = tokio::spawn(async move {
        task_db.transaction(
            |tx, outbox: &mut Outbox| {
                let plan = plan.clone(); let key = task_key.clone(); let sde = task_sde.clone();
                Box::pin(async move { apply_once_tx(tx, outbox, &sde, &registration, key, Some(plan.as_ref()), ClaimMode::Blocking).await })
            },
            |outcome, _: Outbox| async move { Ok::<_, ApplyError>(outcome) },
        ).await
    });
    PRELOCKED.notified().await;
    assert!(tokio::time::timeout(std::time::Duration::from_millis(100), &mut task).await.is_err());
    assert_eq!(APPLIED_COUNT.load(Ordering::SeqCst), 0);
    locked.rollback().await.unwrap();
    assert!(matches!(task.await.unwrap().unwrap(), ApplyOutcome::Applied));
    assert_eq!(APPLIED_COUNT.load(Ordering::SeqCst), 1);
    assert_eq!(PUBLISHED_COUNT.load(Ordering::SeqCst), 1);
    drain(db.clone(), world, drain_job()).await.unwrap();
    assert_eq!(APPLIED_COUNT.load(Ordering::SeqCst), 1);
    assert_eq!(PUBLISHED_COUNT.load(Ordering::SeqCst), 1);
    let retry_event = GameEvent::SkillTrained { char_id: 1, type_id: 2, level: 3 };
    let retry_entry = crate::dao::progress::JournalEvent { source_namespace: namespace.clone(), source_key: "retry".into(), schema_version: codec::SCHEMA_VERSION, kind: retry_event.kind().into(), occurred_at: 1, recorded_at: 1, payload: codec::encode(&retry_event).unwrap(), context: "{}".into(), origin: "test".into(), ordinal: 1, subject_character_id: Some(1) };
    let retry_id = crate::dao::progress::insert_batch(&db, &[retry_entry]).await.unwrap()[0].1.clone();
    let retry_key = DeliveryKey { consumer_key: registration.consumer_key.into(), event_id: retry_id };
    let retry_plan = Arc::new(RetryPlan { key: retry_key.clone() });
    let outcome: Result<ApplyOutcome, ApplyError> = db.transaction(
        |tx, outbox: &mut Outbox| {
            let plan = retry_plan.clone(); let key = retry_key.clone(); let sde = sde.clone();
            Box::pin(async move { apply_once_tx(tx, outbox, &sde, &registration, key, Some(plan.as_ref()), ClaimMode::Blocking).await })
        },
        |outcome, _: Outbox| async move { Ok(outcome) },
    ).await;
    assert!(matches!(outcome.unwrap(), ApplyOutcome::Applied));
    assert_eq!(RETRY_COUNT.load(Ordering::SeqCst), 2);
    assert_eq!(foreign_snapshot(&db, registration.consumer_key, &key.event_id).await, foreign_before);
    crate::dao::lua_store::delete(&db, &namespace, "applied").await.unwrap();
    let published_key = format!("prepared-test:{namespace}");
    db.execute("DELETE FROM game.progress_deliveries WHERE event_id IN (SELECT event_id FROM game.progress_events WHERE source_namespace=$1 OR (source_namespace='business' AND source_key=$2))", &[&namespace, &published_key]).await.unwrap();
    db.execute("DELETE FROM game.progress_events WHERE source_namespace=$1", &[&namespace]).await.unwrap();
    db.execute("DELETE FROM game.progress_events WHERE source_namespace='business' AND source_key=$1", &[&published_key]).await.unwrap();
    db.execute("DELETE FROM game.progress_subscription_kinds WHERE consumer_key=$1", &[&key.consumer_key]).await.unwrap();
    db.execute("DELETE FROM game.progress_subscriptions WHERE consumer_key=$1", &[&key.consumer_key]).await.unwrap();
    std::fs::remove_dir_all(dir).unwrap();
}