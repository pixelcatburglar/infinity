//! Real-database coverage: each timed category records exactly once per
//! completed operation, warnings name the external call site, and a real
//! window rollover logs the collected summary.
use super::super::*;
use crate::db::{executor::Executor, Db, Error, Transaction};
use deadpool_postgres::{Manager, ManagerConfig, Pool, RecyclingMethod};
use std::{io, sync::{Arc, atomic::AtomicI64}};
use tokio_postgres::NoTls;
use tracing::instrument::WithSubscriber;

/// In-memory log sink for the scoped subscriber, as in the services audit tests.
#[derive(Clone)]
struct Capture(Arc<Mutex<Vec<u8>>>);
impl io::Write for Capture {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> { self.0.lock().unwrap().extend_from_slice(bytes); Ok(bytes.len()) }
    fn flush(&mut self) -> io::Result<()> { Ok(()) }
}

#[tokio::test]
#[ignore = "requires a dedicated PostgreSQL database; run this exact test in its own process"]
async fn database_timings_cover_pool_statements_and_transactions() {
    let manager = Manager::from_config(crate::config::tests::test_database().to_pg_config().unwrap(), NoTls, ManagerConfig { recycling_method: RecyclingMethod::Verified });
    let db = Db { pool: Pool::builder(manager).max_size(1).runtime(deadpool_postgres::Runtime::Tokio1).build().unwrap(), online: Arc::new(AtomicI64::new(0)), progress_registry: Arc::new(crate::progress::Registry::default()), loss_tolerant_async_commit: true };
    let bytes = Arc::new(Mutex::new(Vec::new()));
    let capture = Capture(bytes.clone());
    let subscriber = tracing_subscriber::fmt().without_time().with_ansi(false).with_writer(move || capture.clone()).finish();
    let site = async move {
        startup_complete();
        *state().lock().unwrap() = Stats::default();
        // The warning names this external site: `query_one` sits at column 12
        // in the query call that follows the `site` binding.
        let site = (file!(), line!() + 1, 12);
        db.query_one("SELECT pg_sleep(0.11)", &[]).await.unwrap();
        Executor::query(&db, "SELECT 1", &[]).await.unwrap();
        db.uncached().query_one("SELECT 1", &[]).await.unwrap();
        db.transaction::<_, _, Error, (), _, _, _>(|tx, _| Box::pin(async move {
            tx.query_one("SELECT pg_sleep(1.1)", &[]).await.unwrap();
            Ok::<(), Error>(())
        }), |(), ()| async move { Ok(()) }).await.unwrap();
        // A transaction begun outside Db records nothing on explicit rollback.
        let mut client = db.pool.get().await.unwrap();
        Transaction::from(client.transaction().await.unwrap()).rollback().await.unwrap();
        drop(client);
        let counts = { let stats = state().lock().unwrap(); Kind::ALL.map(|kind| stats.kinds[kind.index()].count) };
        assert_eq!(counts, [4, 4, 1, 1]);
        state().lock().unwrap().start = Instant::now().checked_sub(WINDOW);
        db.execute("SELECT 1", &[]).await.unwrap();
        site
    }.with_subscriber(subscriber).await;
    let output = String::from_utf8(bytes.lock().unwrap().clone()).unwrap();
    let (file, line, column) = site;
    let warns = |kind: &str| output.lines().filter(move |text| text.contains("database operation slow") && text.contains(&format!("kind=\"{kind}\""))).collect::<Vec<_>>();
    let location = format!("file=\"{file}\" line={line} column={column}");
    assert!(warns("statement").iter().any(|text| text.contains(&location)), "{output}");
    assert_eq!(warns("transaction").len(), 1, "{output}");
    assert!(output.lines().filter(|text| text.contains("database operation slow")).all(|text| text.contains("WARN")), "{output}");
    let summary: Vec<_> = output.lines().filter(|text| text.contains("database timing summary")).collect();
    assert_eq!(summary.len(), 1, "{output}");
    for expected in ["checkout={count=4", "statement={count=4", "transaction={count=1", "commit={count=1", "top_complete=true", "overflow={count=0", "integration.rs:"] {
        assert!(summary[0].contains(expected), "{expected}: {output}");
    }
}

/// A slow operation logged before `startup_complete` is INFO with
/// `phase = "startup"`; afterwards the same message is WARN again.
#[tokio::test]
async fn slow_warning_switches_level_at_startup_boundary() {
    let previous = STARTUP_COMPLETE.swap(false, Ordering::Relaxed);
    let bytes = Arc::new(Mutex::new(Vec::new()));
    let capture = Capture(bytes.clone());
    let subscriber = tracing_subscriber::fmt().without_time().with_ansi(false).with_writer(move || capture.clone()).finish();
    async move {
        Warn { kind: Kind::Statement, caller: Location::caller(), elapsed: Duration::from_secs(1), overflow: false }.log();
        startup_complete();
        Warn { kind: Kind::Statement, caller: Location::caller(), elapsed: Duration::from_secs(1), overflow: false }.log();
    }.with_subscriber(subscriber).await;
    STARTUP_COMPLETE.store(previous, Ordering::Relaxed);
    let output = String::from_utf8(bytes.lock().unwrap().clone()).unwrap();
    let slow: Vec<_> = output.lines().filter(|text| text.contains("database operation slow")).collect();
    assert_eq!(slow.len(), 2, "{output}");
    assert!(slow[0].contains("INFO") && slow[0].contains("phase=\"startup\""), "{output}");
    assert!(slow[1].contains("WARN") && !slow[1].contains("phase"), "{output}");
}
