//! Test-only PostgreSQL gate for real inventory and salvage transactions.

use super::{Future, Pin, WAIT};

// The test's two-int advisory is distinct from the production bigint fence.
pub(in crate::services) struct Control {
    pub(in crate::services) client: tokio_postgres::Client,
    driver: tokio::task::JoinHandle<Result<(), tokio_postgres::Error>>,
    pub(in crate::services) pid: i32,
    gate: std::cell::Cell<i32>,
    backends: std::cell::RefCell<Vec<i32>>,
}
pub(in crate::services) async fn control_client() -> Control {
    let cfg = crate::config::tests::test_database();
    let (client, connection) = cfg.to_pg_config().unwrap().connect(tokio_postgres::NoTls).await.unwrap();
    let driver = tokio::spawn(async move { connection.await });
    let pid: i32 = client.query_one("SELECT pg_backend_pid() AS pid", &[]).await.unwrap().get("pid");
    Control { client, driver, pid, gate: std::cell::Cell::new(0), backends: std::cell::RefCell::new(Vec::new()) }
}

pub(in crate::services) async fn named_db(name: &str) -> crate::db::Db {
    let mut config = crate::config::tests::test_database();
    config.application_name = name.to_owned();
    crate::db::Db::connect(&config).await.unwrap()
}

pub(in crate::services) fn report_cleanup_failures(cleanup: &[std::thread::Result<()>]) {
    for (stage, outcome) in cleanup.iter().enumerate() {
        if let Err(panic) = outcome {
            let message = panic.downcast_ref::<String>().map(String::as_str)
                .or_else(|| panic.downcast_ref::<&str>().copied()).unwrap_or("non-string panic");
            eprintln!("gate cleanup stage {stage} failed: {message}");
        }
    }
}

impl Control {
    pub(in crate::services) async fn gate_fn(&self, tag: &str, gate: i32) {
        self.client.execute(&format!("CREATE OR REPLACE FUNCTION game.{tag}() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN PERFORM pg_advisory_xact_lock({gate}, 0); RETURN NULL; END $$"), &[]).await.unwrap();
    }
    pub(in crate::services) async fn hold(&self, gate: i32) {
        self.client.execute("BEGIN", &[]).await.unwrap();
        self.client.execute("SELECT pg_advisory_xact_lock($1::int,$2::int)", &[&gate, &0]).await.unwrap();
        self.gate.set(gate);
    }
    pub(in crate::services) async fn release(&self) {
        self.client.execute("COMMIT", &[]).await.unwrap();
    }
    #[cfg(feature = "tests-inventory")]
    pub(in crate::services) fn track_backend(&self, pid: i32) {
        self.backends.borrow_mut().push(pid);
    }
    pub(in crate::services) async fn wait_backends(&self) {
        for pid in self.backends.borrow().iter() {
            tokio::time::timeout(WAIT, async {
                loop {
                    let row = self.client.query_opt("SELECT state FROM pg_stat_activity WHERE pid=$1 AND datname=current_database()", &[pid]).await.unwrap();
                    if row.is_none_or(|r| r.get::<_, Option<String>>("state").as_deref() == Some("idle")) { break; }
                    tokio::task::yield_now().await;
                }
            }).await.expect("business backend transaction never ended");
        }
    }
    pub(in crate::services) async fn observe_pools(&self, names: &[String]) {
        for name in names {
            let row = self.client.query_one(
                "SELECT count(*) AS present FROM pg_stat_activity WHERE datname=current_database() AND application_name=$1",
                &[name]).await.unwrap();
            assert!(row.get::<_, i64>("present") > 0, "fixture pool identity was never observed: {name}");
        }
    }
    pub(in crate::services) async fn wait_pools(&self, names: &[String]) {
        tokio::time::timeout(WAIT, async {
            loop {
                let row = self.client.query_one(
                    "SELECT count(*) AS active FROM pg_stat_activity WHERE datname=current_database() \
                     AND application_name=ANY($1) AND state IS DISTINCT FROM 'idle'",
                    &[&names]).await.unwrap();
                if row.get::<_, i64>("active") == 0 { break; }
                tokio::task::yield_now().await;
            }
        }).await.expect("fixture database transactions never finished");
    }
    pub(in crate::services) async fn gate_uninstall(&self, tag: &str, tables: &[&str]) {
        let mut cleanup = Vec::new();
        for table in tables {
            cleanup.push(crate::tests::catch_scenario(async {
                self.client.execute(&format!("DROP TRIGGER IF EXISTS {tag} ON {table}"), &[]).await.unwrap();
            }).await);
        }
        cleanup.push(crate::tests::catch_scenario(async {
            self.client.execute(&format!("DROP FUNCTION IF EXISTS game.{tag}()"), &[]).await.unwrap();
        }).await);
        report_cleanup_failures(&cleanup);
        for outcome in cleanup { if let Err(panic) = outcome { std::panic::resume_unwind(panic); } }
    }
    pub(in crate::services) fn into_driver(self) -> tokio::task::JoinHandle<Result<(), tokio_postgres::Error>> {
        let Control { client, driver, .. } = self;
        drop(client);
        driver
    }
}

pub(in crate::services) async fn wait_driver(driver: &mut tokio::task::JoinHandle<Result<(), tokio_postgres::Error>>) {
    match tokio::time::timeout(WAIT, &mut *driver).await {
        Ok(result) => result.expect("control driver task failed").expect("control connection failed"),
        Err(_) => {
            driver.abort();
            match driver.await {
                Ok(Err(error)) => eprintln!("control connection failed while aborting: {error}"),
                Err(error) if !error.is_cancelled() => eprintln!("control driver failed while aborting: {error}"),
                _ => (),
            }
            panic!("control driver never exited");
        }
    }
}

/// Poll the borrowed request until it completes early or its backend waits on
/// this control connection's exact two-int advisory lock.
pub(in crate::services) async fn drive_gated<T>(
    db: &crate::db::Db, control: &Control, fut: &mut Pin<Box<dyn Future<Output = T> + '_>>, stage: &str,
) -> (Option<T>, i32) {
    let mut outcome = None;
    let mut poisoned = false;
    let observed = crate::tests::catch_scenario(async {
    outcome = Some(tokio::time::timeout(WAIT, async {
        loop {
            let ready = std::future::poll_fn(|cx| std::task::Poll::Ready(
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| fut.as_mut().poll(cx))))).await;
            let ready = match ready { Ok(value) => value, Err(panic) => { poisoned = true; std::panic::resume_unwind(panic) } };
            if let std::task::Poll::Ready(value) = ready { return (Some(value), 0); }
            if let Some(row) = db.query_opt(
                "SELECT a.pid FROM pg_stat_activity a JOIN pg_locks waiting ON waiting.pid=a.pid \
                 JOIN pg_locks held ON held.pid=$2 AND held.locktype='advisory' AND held.granted \
                 AND held.database=waiting.database AND held.classid=waiting.classid \
                 AND held.objid=waiting.objid AND held.objsubid=waiting.objsubid \
                 WHERE a.datname=current_database() AND a.wait_event_type='Lock' AND a.query ILIKE $1 \
                 AND a.pid<>$2 AND $2=ANY(pg_blocking_pids(a.pid)) AND waiting.locktype='advisory' \
                 AND NOT waiting.granted AND waiting.classid=($3::int::bigint & 4294967295)::oid \
                 AND waiting.objid=0::oid AND waiting.objsubid=2 LIMIT 1",
                &[&stage, &control.pid, &control.gate.get()]).await.unwrap() {
                let pid = row.get::<_, i32>("pid");
                control.backends.borrow_mut().push(pid);
                return (None, pid);
            }
            tokio::task::yield_now().await;
        }
    }).await.expect("business backend never gated"));
    }).await;
    if let Err(panic) = observed {
        control.release().await;
        if !poisoned {
            let drain = crate::tests::catch_scenario(async {
                tokio::time::timeout(WAIT, fut.as_mut()).await.expect("business request did not finish after gate release");
            }).await;
            if drain.is_err() { eprintln!("business request failed while draining after gate observation panic"); }
        }
        std::panic::resume_unwind(panic);
    }
    outcome.expect("gate observation returned")
}

/// Keep the proof and observer owned while observation is caught, then release
/// the test gate and finish the proof before propagating any observation panic.
#[cfg(feature = "tests-inventory")]
pub(in crate::services) async fn finish_proof<T: std::fmt::Debug>(
    db: &crate::db::Db, control: &Control, business_pid: i32,
    proof: &mut Pin<Box<dyn Future<Output = T> + '_>>,
) -> T {
    let mut observer = tokio::spawn({ let db = db.clone(); async move {
        crate::dao::items::parent_locks::tests::wait_blocked(&db, business_pid).await
    }});
    let mut result = None;
    let mut poisoned = false;
    let observed = crate::tests::catch_scenario(async {
        tokio::time::timeout(WAIT, async {
            loop {
                let polled = std::future::poll_fn(|cx| std::task::Poll::Ready(
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| proof.as_mut().poll(cx))))).await;
                match polled {
                    Ok(std::task::Poll::Ready(value)) => result = Some(value),
                    Ok(std::task::Poll::Pending) => (),
                    Err(panic) => { poisoned = true; std::panic::resume_unwind(panic); }
                }
                if result.is_some() || observer.is_finished() { break; }
                tokio::task::yield_now().await;
            }
        }).await.expect("proof observation timed out");
    }).await;
    control.release().await;
    let mut completed = None;
    let drain = if result.is_none() && !poisoned {
        crate::tests::catch_scenario(async {
            completed = Some(tokio::time::timeout(WAIT, proof.as_mut()).await.expect("proof never completed"));
        }).await
    } else { Ok(()) };
    if !observer.is_finished() { observer.abort(); }
    let pid = (&mut observer).await;
    if let Ok(pid) = pid { control.track_backend(pid); }
    if let Err(panic) = observed { std::panic::resume_unwind(panic); }
    if let Err(panic) = drain { std::panic::resume_unwind(panic); }
    assert!(result.is_none(), "proof completed before the gate was released: {result:?}");
    let pid = pid.expect("proof did not block on the business backend");
    assert_ne!(pid, business_pid, "proof must use a different backend");
    completed.expect("proof did not finish")
}
