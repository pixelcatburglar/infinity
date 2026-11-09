//! Scheduler arithmetic and database lease regression tests.
//! Run ignored tests serially; runner tests require a database without scheduled or running jobs.
use super::*;
use crate::{client::filetime_now, dao::jobs};

async fn test_db() -> Db {
    Db::connect(&crate::config::tests::test_database()).await.unwrap()
}

async fn test_client() -> tokio_postgres::Client {
    let (client, connection) = crate::config::tests::test_database().to_pg_config().unwrap().connect(tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    client
}

#[test]
fn backoff_saturates_and_caps_exponent() {
    assert_eq!(runner::backoff(30, 1), 30);
    assert_eq!(runner::backoff(30, 3), 120);
    assert_eq!(runner::backoff(30, 100), 7680);
    assert_eq!(runner::backoff(u64::MAX, 2), u64::MAX);
}

fn unused_handler(_: Db, _: Arc<World>, _: Job) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send>> {
    Box::pin(async { Ok(()) })
}

#[test]
fn duplicate_registry_is_rejected() {
    assert!(runner::registry(vec![register("same", unused_handler), register("same", unused_handler)]).is_err());
    assert!(runner::registry(vec![register("first", unused_handler), register("second", unused_handler)]).is_ok());
}

struct FailedWrite<'a, E: crate::db::executor::Executor>(&'a E);
impl<E: crate::db::executor::Executor> crate::db::executor::Executor for FailedWrite<'_, E> {
    async fn query<T: ?Sized + tokio_postgres::ToStatement + Sync>(&self, sql: &T, args: &[&(dyn tokio_postgres::types::ToSql + Sync)]) -> Result<Vec<tokio_postgres::Row>, Error> { self.0.query(sql, args).await }
    async fn query_one<T: ?Sized + tokio_postgres::ToStatement + Sync>(&self, sql: &T, args: &[&(dyn tokio_postgres::types::ToSql + Sync)]) -> Result<tokio_postgres::Row, Error> { self.0.query_one(sql, args).await }
    async fn query_opt<T: ?Sized + tokio_postgres::ToStatement + Sync>(&self, sql: &T, args: &[&(dyn tokio_postgres::types::ToSql + Sync)]) -> Result<Option<tokio_postgres::Row>, Error> { self.0.query_opt(sql, args).await }
    async fn execute<T: ?Sized + tokio_postgres::ToStatement + Sync>(&self, _: &T, _: &[&(dyn tokio_postgres::types::ToSql + Sync)]) -> Result<u64, Error> { self.0.execute("SELECT 1 / 0", &[]).await }
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL; run serially"]
async fn failed_periodic_completion_leaves_expired_claim_reclaimable() {
    let mut client = test_client().await;
    let mut tx = client.transaction().await.unwrap();
    let existing = jobs::tests::existing_ids(&tx).await.unwrap();
    let excluded: Vec<&str> = existing.iter().map(String::as_str).collect();
    let now = filetime_now();
    let id = format!("scheduler-completion-test:{now}");
    assert!(!existing.contains(&id), "test job ID already exists");
    let job = Job { id: id.clone(), kind: "test".into(), due_at: now, payload: "{}".into() };
    schedule(&tx, &job).await.unwrap();
    let claim = jobs::claim(&tx, now, now + 1, 100, &excluded).await.unwrap().into_iter().find(|c| c.job.id == id).unwrap();
    let save = tx.savepoint("failed_completion").await.unwrap();
    assert!(jobs::complete_periodic(&FailedWrite(&save), &id, claim.token, now + 100, now).await.is_err());
    save.rollback().await.unwrap();
    let reclaimed = jobs::claim(&tx, now + 2, now + 3, 100, &excluded).await.unwrap().into_iter().find(|c| c.job.id == id).unwrap();
    assert_ne!(claim.token, reclaimed.token);
    cancel(&tx, &id).await.unwrap();
    tx.rollback().await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL; run serially"]
async fn claims_reclaim_and_stale_ack() {
    let db = test_db().await;
    let existing = jobs::tests::existing_ids(&db).await.unwrap();
    let now = filetime_now();
    let id = format!("scheduler-test:{}", now);
    assert!(!existing.contains(&id), "test job ID already exists");
    let job = Job { id: id.clone(), kind: "test".into(), due_at: now, payload: "{}".into() };
    assert_eq!(schedule(&db, &job).await.unwrap(), 1);
    let task_db = db.clone();
    let task_id = id.clone();
    let result = tokio::spawn(async move {
        let db = task_db;
        let id = task_id;
        let excluded: Vec<&str> = existing.iter().map(String::as_str).collect();
        assert_eq!(schedule(&db, &job).await.unwrap(), 0);
        let (a, b) = tokio::join!(jobs::claim(&db, now, now + 1, 1, &excluded), jobs::claim(&db, now, now + 1, 1, &excluded));
        let mut claims = a.unwrap(); claims.extend(b.unwrap());
        assert_eq!(claims.iter().filter(|claim| claim.job.id == id).count(), 1);
        let first = claims.into_iter().find(|claim| claim.job.id == id).expect("job claimed");
        let mut excluded_with_own = excluded.clone();
        excluded_with_own.push(id.as_str());
        assert!(jobs::claim(&db, now + 2, now + 3, 100, &excluded_with_own).await.unwrap().iter().all(|claim| claim.job.id != id));
        let reclaimed = jobs::claim(&db, now + 2, now + 3, 100, &excluded).await.unwrap().into_iter().find(|claim| claim.job.id == id).expect("lease reclaimed");
        assert_ne!(first.token, reclaimed.token);
        assert_eq!(jobs::ack(&db, &id, first.token).await.unwrap(), 0);
        let mut changed = job.clone(); changed.due_at += 10;
        schedule(&db, &changed).await.unwrap();
        assert_eq!(jobs::ack(&db, &id, reclaimed.token).await.unwrap(), 0);
        let next = jobs::claim(&db, changed.due_at, changed.due_at + 1, 1, &excluded).await.unwrap().pop().unwrap();
        assert_eq!(next.attempt, reclaimed.attempt + 1);
        assert_ne!(next.token, reclaimed.token);
        assert_eq!(jobs::ack(&db, &id, next.token).await.unwrap(), 1);
    }).await;
    let cleanup = cancel(&db, &id).await;
    if let Err(error) = result {
        cleanup.expect("failed to clean up test job after task failure");
        std::panic::resume_unwind(error.into_panic());
    }
    cleanup.expect("failed to clean up test job");
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL; run serially"]
async fn retry_and_periodic_completion_are_token_guarded() {
    let mut client = test_client().await;
    let tx = client.transaction().await.unwrap();
    let existing = jobs::tests::existing_ids(&tx).await.unwrap();
    let excluded: Vec<&str> = existing.iter().map(String::as_str).collect();
    let now = filetime_now();
    let id = format!("scheduler-retry-test:{now}");
    assert!(!existing.contains(&id), "test job ID already exists");
    let job = Job { id: id.clone(), kind: "test".into(), due_at: now, payload: "{}".into() };
    schedule(&tx, &job).await.unwrap();
    let first = jobs::claim(&tx, now, now + 10, 100, &excluded).await.unwrap().into_iter().find(|c| c.job.id == id).unwrap();
    assert_eq!(first.attempt, 1);
    assert_eq!(jobs::fail(&tx, &id, first.token, now + 20, now, "failure", false).await.unwrap(), 1);
    let second = jobs::claim(&tx, now + 20, now + 30, 100, &excluded).await.unwrap().into_iter().find(|c| c.job.id == id).unwrap();
    assert_eq!(second.attempt, 2);
    assert_eq!(jobs::complete_periodic(&tx, &id, first.token, now + 40, now + 20).await.unwrap(), 0);
    assert_eq!(jobs::complete_periodic(&tx, &id, second.token, now + 40, now + 20).await.unwrap(), 1);
    let third = jobs::claim(&tx, now + 40, now + 50, 100, &excluded).await.unwrap().into_iter().find(|c| c.job.id == id).unwrap();
    assert_eq!(third.attempt, 1);
    assert_eq!(jobs::fail(&tx, &id, third.token, now + 60, now + 40, "dead", true).await.unwrap(), 1);
    assert!(jobs::claim(&tx, now + 60, now + 70, 100, &excluded).await.unwrap().iter().all(|c| c.job.id != id));
    jobs::seed(&tx, &job, now + 80).await.unwrap();
    let revived = jobs::claim(&tx, now + 80, now + 90, 100, &excluded).await.unwrap().into_iter().find(|c| c.job.id == id).unwrap();
    assert_eq!(revived.attempt, 1);
    cancel(&tx, &id).await.unwrap();
    tx.rollback().await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL; run serially"]
async fn distinct_tokens_seed_and_dead_revival() {
    let mut client = test_client().await;
    let tx = client.transaction().await.unwrap();
    let existing = jobs::tests::existing_ids(&tx).await.unwrap();
    let excluded: Vec<&str> = existing.iter().map(String::as_str).collect();
    let now = filetime_now();
    let ids = [format!("seed-a:{now}"), format!("seed-b:{now}")];
    assert!(ids.iter().all(|id| !existing.contains(id)), "test job ID already exists");
    let first = Job { id: ids[0].clone(), kind: "test".into(), due_at: now, payload: "{}".into() };
    let second = Job { id: ids[1].clone(), ..first.clone() };
    assert_eq!(jobs::seed(&tx, &first, now).await.unwrap(), 1);
    assert_eq!(jobs::seed(&tx, &second, now).await.unwrap(), 1);
    let mut later = first.clone(); later.due_at += 100;
    assert_eq!(jobs::seed(&tx, &later, now + 1).await.unwrap(), 0);
    let claims = jobs::claim(&tx, now, now + 10, 2, &excluded).await.unwrap();
    assert_eq!(claims.len(), 2);
    assert_ne!(claims[0].token, claims[1].token);
    assert_eq!(jobs::seed(&tx, &later, now + 1).await.unwrap(), 0);
    let claim = claims.iter().find(|c| c.job.id == ids[0]).unwrap();
    assert_eq!(jobs::fail(&tx, &ids[0], claim.token, now, now, "dead error", true).await.unwrap(), 1);
    assert_eq!(schedule(&tx, &first).await.unwrap(), 1);
    let row = tx.query_one("SELECT attempt,last_error,due_at FROM game.jobs WHERE job_id=$1", &[&ids[0]]).await.unwrap();
    assert_eq!(row.get::<_, i32>("attempt"), 0);
    assert_eq!(row.get::<_, Option<String>>("last_error"), None);
    let revived = jobs::claim(&tx, now, now + 1, 1, &excluded).await.unwrap().pop().unwrap();
    assert_eq!(revived.attempt, 1);
    assert_eq!(jobs::fail(&tx, &ids[0], revived.token, now, now, "dead again", true).await.unwrap(), 1);
    assert_eq!(jobs::seed(&tx, &later, now).await.unwrap(), 1);
    let row = tx.query_one("SELECT attempt,last_error,due_at FROM game.jobs WHERE job_id=$1", &[&ids[0]]).await.unwrap();
    assert_eq!(row.get::<_, i32>("attempt"), 0);
    assert_eq!(row.get::<_, Option<String>>("last_error"), None);
    assert_eq!(row.get::<_, i64>("due_at"), later.due_at);
    for id in &ids { cancel(&tx, id).await.unwrap(); }
    tx.rollback().await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL; run serially"]
async fn three_failures_increment_attempts_and_backoff() {
    let mut client = test_client().await;
    let tx = client.transaction().await.unwrap();
    let existing = jobs::tests::existing_ids(&tx).await.unwrap();
    let excluded: Vec<&str> = existing.iter().map(String::as_str).collect();
    let now = filetime_now();
    let id = format!("retries:{now}");
    assert!(!existing.contains(&id), "test job ID already exists");
    let job = Job { id: id.clone(), kind: "test".into(), due_at: now, payload: "{}".into() };
    schedule(&tx, &job).await.unwrap();
    let mut due = now;
    let mut previous_delay = 0;
    for attempt in 1..=3 {
        let claim = jobs::claim(&tx, due, due + 1, 1, &excluded).await.unwrap().pop().unwrap();
        assert_eq!(claim.attempt, attempt);
        let delay = jobs::duration_ticks(runner::backoff(30, attempt));
        assert!(delay > previous_delay);
        let next = due.saturating_add(delay);
        assert_eq!(jobs::fail(&tx, &id, claim.token, next, due, "failure", false).await.unwrap(), 1);
        assert!(jobs::claim(&tx, next - 1, next, 1, &excluded).await.unwrap().is_empty());
        due = next;
        previous_delay = delay;
    }
    cancel(&tx, &id).await.unwrap();
    tx.rollback().await.unwrap();
}

static SLOW_RUNS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
fn slow_handler(_: Db, _: Arc<World>, _: Job) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send>> {
    Box::pin(async {
        SLOW_RUNS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        Ok(())
    })
}
fn panic_handler(_: Db, _: Arc<World>, _: Job) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send>> {
    Box::pin(async { panic!("scheduler test panic") })
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL without scheduled or running jobs and with SDE; run serially"]
async fn runner_dead_letters_at_configured_threshold_but_not_periodic() {
    let db = test_db().await;
    jobs::tests::require_job_free_database(&db).await;
    let world = World::new(db.clone(), Arc::new(crate::sde::Sde::load(&db).await.unwrap()), 0);
    let now = filetime_now();
    let ids = [format!("threshold:{now}"), format!("periodic-threshold:{now}")];
    for (id, kind) in ids.iter().zip(["threshold", "periodic-threshold"]) {
        schedule(&db, &Job { id: id.clone(), kind: kind.into(), due_at: now, payload: "{}".into() }).await.unwrap();
    }
    let settings = crate::config::scheduler::SchedulerConfig::default();
    assert_eq!(settings.dead_letter_attempts, 25);
    for attempt in 1..settings.dead_letter_attempts {
        let claims = jobs::claim(&db, now, now + 1, 2, &[]).await.unwrap();
        assert_eq!(claims.len(), 2);
        for claim in claims {
            assert_eq!(claim.attempt, attempt);
            assert_eq!(jobs::fail(&db, &claim.job.id, claim.token, now, now, "failure", false).await.unwrap(), 1);
        }
    }
    let (stop, shutdown) = tokio::sync::watch::channel(false);
    let runner = runner::start(db.clone(), world, vec![register("threshold", panic_handler), periodic("periodic-threshold", panic_handler, 600)], settings, shutdown).unwrap();
    wake();
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let first = jobs::tests::inspect(&db, &ids[0]).await.unwrap();
            let second = jobs::tests::inspect(&db, &ids[1]).await.unwrap();
            if [first, second].iter().all(|row| row.attempt == 25 && row.state != "running") { break; }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }).await.unwrap();
    stop.send(true).unwrap();
    runner.await.unwrap();
    let dead = jobs::tests::inspect(&db, &ids[0]).await.unwrap();
    let periodic_row = jobs::tests::inspect(&db, &ids[1]).await.unwrap();
    assert_eq!(dead.state, "dead");
    assert_eq!(periodic_row.state, "scheduled");
    for id in &ids { cancel(&db, id).await.unwrap(); }
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL without scheduled or running jobs and with SDE; run serially"]
async fn runner_excludes_local_slow_claim_and_retries_panic_and_missing_handler() {
    let db = test_db().await;
    jobs::tests::require_job_free_database(&db).await;
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let world = World::new(db.clone(), sde, 0);
    let now = filetime_now();
    let ids = [format!("slow:{now}"), format!("panic:{now}"), format!("missing:{now}"), format!("foreign:{now}")];
    for (id, kind) in ids.iter().zip(["test-slow", "test-panic", "test-missing", "test-foreign"]) {
        schedule(&db, &Job { id: id.clone(), kind: kind.into(), due_at: now, payload: "{}".into() }).await.unwrap();
    }
    let foreign = jobs::claim(&db, now, now + 1, 100, &[&ids[0], &ids[1], &ids[2]]).await.unwrap().pop().unwrap();
    assert_eq!(foreign.job.id, ids[3]);
    SLOW_RUNS.store(0, std::sync::atomic::Ordering::SeqCst);
    let settings = crate::config::scheduler::SchedulerConfig { poll_secs: 1, lease_secs: 2, max_concurrent: 3, retry_base_secs: 30, dead_letter_attempts: 25, drain_secs: 1 };
    let (stop, shutdown) = tokio::sync::watch::channel(false);
    let runner = runner::start(db.clone(), world, vec![periodic("test-slow", slow_handler, 600), register("test-panic", panic_handler), register("test-foreign", unused_handler)], settings, shutdown).unwrap();
    wake();
    tokio::time::sleep(std::time::Duration::from_secs(4)).await;
    stop.send(true).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), runner).await.unwrap().unwrap();
    assert_eq!(SLOW_RUNS.load(std::sync::atomic::Ordering::SeqCst), 1);
    let slow = jobs::tests::inspect(&db, &ids[0]).await.unwrap();
    assert_eq!(slow.state, "scheduled");
    assert_eq!(slow.attempt, 0);
    let later = filetime_now().saturating_add(crate::client::filetime_ticks_from_seconds(60));
    let claims = jobs::claim(&db, later, later + 1, 100, &[]).await.unwrap();
    assert!(claims.iter().all(|c| c.job.id != ids[0] && c.job.id != ids[3]));
    for id in &ids[1..3] {
        assert!(claims.iter().any(|c| c.job.id == *id && c.attempt >= 2));
    }
    for id in &ids { cancel(&db, id).await.unwrap(); }
}

static RUNNING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
fn long_handler(_: Db, _: Arc<World>, _: Job) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send>> {
    Box::pin(async {
        struct Guard;
        impl Drop for Guard { fn drop(&mut self) { RUNNING.store(false, std::sync::atomic::Ordering::SeqCst); } }
        RUNNING.store(true, std::sync::atomic::Ordering::SeqCst);
        let _guard = Guard;
        std::future::pending::<()>().await;
        Ok(())
    })
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL without scheduled or running jobs and with SDE; run serially"]
async fn aborting_drain_aborts_owned_handler_and_leaves_lease() {
    let db = test_db().await;
    jobs::tests::require_job_free_database(&db).await;
    let world = World::new(db.clone(), Arc::new(crate::sde::Sde::load(&db).await.unwrap()), 0);
    let now = filetime_now();
    let id = format!("shutdown:{now}");
    schedule(&db, &Job { id: id.clone(), kind: "test-long".into(), due_at: now, payload: "{}".into() }).await.unwrap();
    let settings = crate::config::scheduler::SchedulerConfig { poll_secs: 1, lease_secs: 2, max_concurrent: 1, retry_base_secs: 1, dead_letter_attempts: 25, drain_secs: 1 };
    let (stop, shutdown) = tokio::sync::watch::channel(false);
    let mut runner = runner::start(db.clone(), world, vec![register("test-long", long_handler)], settings, shutdown).unwrap();
    wake();
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while !RUNNING.load(std::sync::atomic::Ordering::SeqCst) { tokio::time::sleep(std::time::Duration::from_millis(10)).await; }
    }).await.unwrap();
    stop.send(true).unwrap();
    assert!(tokio::time::timeout(std::time::Duration::from_millis(50), &mut runner).await.is_err());
    runner.abort();
    let _ = runner.await;
    tokio::task::yield_now().await;
    assert!(!RUNNING.load(std::sync::atomic::Ordering::SeqCst));
    let later = filetime_now().saturating_add(crate::client::filetime_ticks_from_seconds(3));
    assert!(jobs::claim(&db, later, later + 1, 100, &[]).await.unwrap().iter().any(|c| c.job.id == id));
    cancel(&db, &id).await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL without scheduled or running jobs and with SDE; run serially"]
async fn shutdown_prevents_new_due_jobs_during_unrelated_teardown() {
    let db = test_db().await;
    jobs::tests::require_job_free_database(&db).await;
    let world = World::new(db.clone(), Arc::new(crate::sde::Sde::load(&db).await.unwrap()), 0);
    let now = filetime_now();
    let running_id = format!("shutdown-running:{now}");
    let due_id = format!("shutdown-due:{now}");
    schedule(&db, &Job { id: running_id.clone(), kind: "test-long".into(), due_at: now, payload: "{}".into() }).await.unwrap();
    let settings = crate::config::scheduler::SchedulerConfig { poll_secs: 1, lease_secs: 30, max_concurrent: 2, retry_base_secs: 1, dead_letter_attempts: 25, drain_secs: 1 };
    let (stop, shutdown) = tokio::sync::watch::channel(false);
    let runner = runner::start(db.clone(), world, vec![register("test-long", long_handler), register("test-slow", slow_handler)], settings, shutdown).unwrap();
    wake();
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while !RUNNING.load(std::sync::atomic::Ordering::SeqCst) { tokio::time::sleep(std::time::Duration::from_millis(10)).await; }
    }).await.unwrap();
    let teardown = tokio::spawn(std::future::pending::<()>());
    stop.send(true).unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    SLOW_RUNS.store(0, std::sync::atomic::Ordering::SeqCst);
    schedule(&db, &Job { id: due_id.clone(), kind: "test-slow".into(), due_at: filetime_now(), payload: "{}".into() }).await.unwrap();
    wake();
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert!(!teardown.is_finished());
    assert!(!runner.is_finished());
    assert_eq!(SLOW_RUNS.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert_eq!(jobs::tests::inspect(&db, &due_id).await.unwrap().state, "scheduled");
    teardown.abort();
    let _ = teardown.await;
    runner.abort();
    let _ = runner.await;
    cancel(&db, &running_id).await.unwrap();
    cancel(&db, &due_id).await.unwrap();
}
