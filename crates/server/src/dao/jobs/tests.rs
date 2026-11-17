//! Test-only inspection of durable job state in `game.jobs`; no client counterpart.
//! These helpers do not modify jobs or implement scheduler behavior.
#[cfg(any(feature = "tests-skills", feature = "tests-economy", feature = "tests-slash", feature = "tests-storage"))]
use crate::db::Db;
#[cfg(any(feature = "tests-economy", feature = "tests-slash", feature = "tests-storage"))]
use crate::db::Error;
#[cfg(any(feature = "tests-skills", feature = "tests-economy", feature = "tests-slash", feature = "tests-storage"))]
use tokio_postgres::Row;

#[cfg(any(feature = "tests-slash", feature = "tests-storage"))]
pub(crate) struct JobState {
    pub(crate) state: String,
    pub(crate) attempt: i32,
    #[cfg(feature = "tests-slash")]
    pub(crate) last_error: Option<String>,
}

#[cfg(any(feature = "tests-slash", feature = "tests-storage"))]
impl JobState {
    fn from_row(row: &Row) -> Self {
        Self {
            state: row.get("state"),
            attempt: row.get("attempt"),
            #[cfg(feature = "tests-slash")]
            last_error: row.get("last_error"),
        }
    }
}

#[cfg(any(feature = "tests-skills", feature = "tests-economy", feature = "tests-slash", feature = "tests-storage"))]
struct ActiveJob {
    job_id: String,
    kind: String,
    state: String,
    due_at: i64,
    lease_until: Option<i64>,
}

#[cfg(any(feature = "tests-skills", feature = "tests-economy", feature = "tests-slash", feature = "tests-storage"))]
impl ActiveJob {
    fn from_row(row: &Row) -> Self {
        Self {
            job_id: row.get("job_id"),
            kind: row.get("kind"),
            state: row.get("state"),
            due_at: row.get("due_at"),
            lease_until: row.get("lease_until"),
        }
    }
}

#[cfg(any(feature = "tests-skills", feature = "tests-economy", feature = "tests-slash", feature = "tests-storage"))]
pub(crate) async fn require_job_free_database(db: &Db) {
    let rows = db.query(
        "SELECT job_id,kind,state,due_at,lease_until FROM game.jobs WHERE state IN ('scheduled','running') ORDER BY due_at,job_id",
        &[],
    ).await.expect("failed to inspect scheduled and running jobs");
    if !rows.is_empty() {
        let jobs: Vec<_> = rows.iter().map(ActiveJob::from_row).map(|job| {
            format!("job_id={:?} kind={:?} state={:?} due_at={} lease_until={:?}",
                job.job_id, job.kind, job.state, job.due_at, job.lease_until)
        }).collect();
        panic!("scheduled or running jobs found:\n{}\nPoint INFINITY_TEST_DATABASE_URL at a separately initialized database without scheduled or running jobs", jobs.join("\n"));
    }
}

#[cfg(any(feature = "tests-economy", feature = "tests-slash", feature = "tests-storage"))]
pub(crate) async fn existing_ids(ex: &(impl crate::db::executor::Executor + ?Sized)) -> Result<Vec<String>, Error> {
    Ok(ex.query("SELECT job_id FROM game.jobs ORDER BY job_id", &[]).await?
        .iter().map(|row| row.get("job_id")).collect())
}

#[cfg(any(feature = "tests-slash", feature = "tests-storage"))]
pub(crate) async fn inspect(db: &Db, id: &str) -> Result<JobState, Error> {
    let row = db.query_one("SELECT state,attempt,last_error FROM game.jobs WHERE job_id=$1", &[&id]).await?;
    Ok(JobState::from_row(&row))
}

#[cfg(feature = "tests-storage")]
#[tokio::test]
#[ignore = "requires a dedicated PostgreSQL database"]
async fn npc_bounty_ensure_once_preserves_claim_and_dead_letter() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    require_job_free_database(&db).await;
    let now = crate::client::filetime_now();
    let job = crate::scheduler::Job { id: format!("npc-bounty-test-{now}"), kind: "npc.bounty.payout".into(), due_at: now, payload: "{}".into() };
    // A fresh row lands once; a repeat insert is a no-op, not a reschedule.
    assert_eq!(super::ensure_once(&db, &job, now).await.unwrap(), 1);
    assert_eq!(super::ensure_once(&db, &job, now).await.unwrap(), 0);
    // A claimed running job keeps its lease, token, attempt and error fields.
    let running = crate::scheduler::Job { id: format!("npc-bounty-run-{now}"), kind: "npc.bounty.payout".into(), due_at: now, payload: "{}".into() };
    db.execute("INSERT INTO game.jobs (job_id,kind,due_at,payload,state,attempt,lease_until,lease_token,last_error,created_at,updated_at)
        VALUES ($1,$2,$3,$4,'running',3,$5,9,'oops',$6,$6)",
        &[&running.id, &running.kind, &running.due_at, &running.payload, &(now + 1), &now]).await.unwrap();
    assert_eq!(super::ensure_once(&db, &running, now).await.unwrap(), 0);
    let row = db.query_one("SELECT state,attempt,lease_until,lease_token,last_error,due_at FROM game.jobs WHERE job_id=$1", &[&running.id]).await.unwrap();
    assert_eq!(row.get::<_, String>("state"), "running");
    assert_eq!((row.get::<_, i32>("attempt"), row.get::<_, Option<i64>>("lease_until"), row.get::<_, Option<i64>>("lease_token"), row.get::<_, i64>("due_at")), (3, Some(now + 1), Some(9), now));
    assert_eq!(row.get::<_, Option<String>>("last_error").as_deref(), Some("oops"));
    // A dead job is never resurrected by ensure_once.
    let dead = crate::scheduler::Job { id: format!("npc-bounty-dead-{now}"), kind: "npc.bounty.payout".into(), due_at: now, payload: "{}".into() };
    db.execute("INSERT INTO game.jobs (job_id,kind,due_at,payload,state,attempt,last_error,created_at,updated_at)
        VALUES ($1,$2,$3,$4,'dead',5,'gone',$6,$6)", &[&dead.id, &dead.kind, &dead.due_at, &dead.payload, &now]).await.unwrap();
    assert_eq!(super::ensure_once(&db, &dead, now).await.unwrap(), 0);
    assert_eq!(db.query_one("SELECT state FROM game.jobs WHERE job_id=$1", &[&dead.id]).await.unwrap().get::<_, String>("state"), "dead");
    db.execute("DELETE FROM game.jobs WHERE job_id=ANY($1)", &[&vec![job.id, running.id, dead.id]]).await.unwrap();
}
