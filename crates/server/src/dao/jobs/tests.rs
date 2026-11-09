//! Test-only inspection of durable job state in `game.jobs`; no client counterpart.
//! These helpers do not modify jobs or implement scheduler behavior.
use crate::db::{Db, Error};
use tokio_postgres::Row;

pub(crate) struct JobState {
    pub(crate) state: String,
    pub(crate) attempt: i32,
    pub(crate) last_error: Option<String>,
}

impl JobState {
    fn from_row(row: &Row) -> Self {
        Self {
            state: row.get("state"),
            attempt: row.get("attempt"),
            last_error: row.get("last_error"),
        }
    }
}

struct ActiveJob {
    job_id: String,
    kind: String,
    state: String,
    due_at: i64,
    lease_until: Option<i64>,
}

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

pub(crate) async fn existing_ids(ex: &(impl crate::db::executor::Executor + ?Sized)) -> Result<Vec<String>, Error> {
    Ok(ex.query("SELECT job_id FROM game.jobs ORDER BY job_id", &[]).await?
        .iter().map(|row| row.get("job_id")).collect())
}

pub(crate) async fn inspect(db: &Db, id: &str) -> Result<JobState, Error> {
    let row = db.query_one("SELECT state,attempt,last_error FROM game.jobs WHERE job_id=$1", &[&id]).await?;
    Ok(JobState::from_row(&row))
}
