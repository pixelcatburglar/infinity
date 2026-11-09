//! Durable scheduler persistence in `game.jobs`, independent of client services.
//! Deadlines use FILETIME; lease renewal and cross-process ownership are unsupported.
use infinity_core::consts::FILETIME_SECOND;
use crate::{client::filetime_ticks_from_seconds, db::{executor::Executor, Error}};
use crate::scheduler::Job;
use tokio_postgres::Row;

#[cfg(test)]
pub(crate) mod tests;

/// Server design: show at most ten upcoming active jobs to an operator.
const OPERATOR_DUE_LIMIT: i64 = 10;

pub(crate) struct JobCount { pub kind: String, pub state: String, pub count: i64 }
impl JobCount {
    fn from_row(row: &Row) -> Self {
        Self { kind: row.get("kind"), state: row.get("state"), count: row.get("count") }
    }
}

pub(crate) struct DueJob { pub id: String, pub kind: String, pub state: String, pub due_at: i64 }
impl DueJob {
    fn from_row(row: &Row) -> Self {
        Self { id: row.get("job_id"), kind: row.get("kind"), state: row.get("state"), due_at: row.get("due_at") }
    }
}

struct LockedJob { job: Job }
impl LockedJob {
    fn from_row(row: &Row) -> Self {
        Self { job: Job { id: row.get("job_id"), kind: row.get("kind"), due_at: row.get("due_at"), payload: row.get("payload") } }
    }
}

pub(crate) async fn counts(ex: &(impl Executor + ?Sized)) -> Result<Vec<JobCount>, Error> {
    Ok(ex.query("SELECT kind,state,COUNT(*) AS count FROM game.jobs GROUP BY kind,state ORDER BY kind,state", &[]).await?.iter().map(JobCount::from_row).collect())
}

pub(crate) async fn upcoming(ex: &(impl Executor + ?Sized)) -> Result<Vec<DueJob>, Error> {
    Ok(ex.query("SELECT job_id,kind,state,due_at FROM game.jobs WHERE state IN ('scheduled','running') ORDER BY due_at,job_id LIMIT $1", &[&OPERATOR_DUE_LIMIT]).await?.iter().map(DueJob::from_row).collect())
}

pub(crate) async fn lock_job(ex: &(impl Executor + ?Sized), id: &str) -> Result<Option<Job>, Error> {
    Ok(ex.query_opt("SELECT job_id,kind,due_at,payload FROM game.jobs WHERE job_id=$1 FOR UPDATE", &[&id]).await?.map(|row| LockedJob::from_row(&row).job))
}

pub(crate) fn duration_ticks(seconds: u64) -> i64 {
    filetime_ticks_from_seconds(seconds.min((i64::MAX / FILETIME_SECOND) as u64) as i64)
}

pub(crate) struct Claim { pub job: Job, pub token: i64, pub attempt: i32 }
impl Claim {
    fn from_row(row: &Row) -> Self {
        Self { job: Job { id: row.get("job_id"), kind: row.get("kind"), due_at: row.get("due_at"), payload: row.get("payload") }, token: row.get("lease_token"), attempt: row.get("attempt") }
    }
}
pub(crate) async fn schedule(ex: &(impl Executor + ?Sized), job: &Job, now: i64) -> Result<u64, Error> {
    ex.execute("INSERT INTO game.jobs (job_id,kind,due_at,payload,state,created_at,updated_at) VALUES ($1,$2,$3,$4,'scheduled',$5,$5)
        ON CONFLICT (job_id) DO UPDATE SET kind=EXCLUDED.kind,due_at=EXCLUDED.due_at,payload=EXCLUDED.payload,state='scheduled',attempt=CASE WHEN game.jobs.state='dead' THEN 0 ELSE game.jobs.attempt END,last_error=CASE WHEN game.jobs.state='dead' THEN NULL ELSE game.jobs.last_error END,lease_until=NULL,lease_token=NULL,updated_at=EXCLUDED.updated_at
        WHERE game.jobs.state <> 'scheduled' OR (game.jobs.kind,game.jobs.due_at,game.jobs.payload) IS DISTINCT FROM (EXCLUDED.kind,EXCLUDED.due_at,EXCLUDED.payload)", &[&job.id,&job.kind,&job.due_at,&job.payload,&now]).await
}
pub(crate) async fn seed(ex: &(impl Executor + ?Sized), job: &Job, now: i64) -> Result<u64, Error> {
    ex.execute("INSERT INTO game.jobs (job_id,kind,due_at,payload,state,created_at,updated_at) VALUES ($1,$2,$3,$4,'scheduled',$5,$5) ON CONFLICT (job_id) DO UPDATE SET kind=EXCLUDED.kind,due_at=EXCLUDED.due_at,payload=EXCLUDED.payload,state='scheduled',attempt=0,lease_until=NULL,lease_token=NULL,last_error=NULL,updated_at=EXCLUDED.updated_at WHERE game.jobs.state='dead'", &[&job.id,&job.kind,&job.due_at,&job.payload,&now]).await
}
pub(crate) async fn cancel(ex: &(impl Executor + ?Sized), id: &str) -> Result<u64, Error> { ex.execute("DELETE FROM game.jobs WHERE job_id=$1", &[&id]).await }
// Local exclusions prevent only same-process reclaim; multi-process
// deployments need lease renewal or a persistent owner column.
pub(crate) async fn claim(ex: &(impl Executor + ?Sized), now: i64, lease_until: i64, limit: i64, active: &[&str]) -> Result<Vec<Claim>, Error> {
    Ok(ex.query("WITH due AS (SELECT job_id FROM game.jobs WHERE ((state='scheduled' AND due_at <= $1) OR (state='running' AND lease_until < $1)) AND job_id <> ALL($4) ORDER BY due_at,job_id LIMIT $3 FOR UPDATE SKIP LOCKED) UPDATE game.jobs j SET state='running',attempt=CASE WHEN j.attempt=$5 THEN j.attempt ELSE j.attempt+1 END,lease_token=nextval('game.jobs_lease_seq'),lease_until=$2,updated_at=$1 FROM due WHERE j.job_id=due.job_id RETURNING j.job_id,j.kind,j.due_at,j.payload,j.lease_token,j.attempt", &[&now,&lease_until,&limit,&active,&i32::MAX]).await?.iter().map(Claim::from_row).collect())
}
pub(crate) async fn next_due(ex: &(impl Executor + ?Sized), active: &[&str]) -> Result<Option<i64>, Error> {
    let row = ex.query_one("SELECT MIN(CASE WHEN state='scheduled' THEN due_at WHEN state='running' THEN lease_until END) AS next_due FROM game.jobs WHERE job_id <> ALL($1)", &[&active]).await?;
    Ok(row.get("next_due"))
}
pub(crate) async fn ack(ex: &(impl Executor + ?Sized), id: &str, token: i64) -> Result<u64, Error> { ex.execute("DELETE FROM game.jobs WHERE job_id=$1 AND state='running' AND lease_token=$2", &[&id,&token]).await }
pub(crate) async fn complete_periodic(ex: &(impl Executor + ?Sized), id: &str, token: i64, due: i64, now: i64) -> Result<u64, Error> {
    ex.execute("UPDATE game.jobs SET state='scheduled',due_at=$3,attempt=0,lease_until=NULL,lease_token=NULL,last_error=NULL,updated_at=$4 WHERE job_id=$1 AND state='running' AND lease_token=$2", &[&id,&token,&due,&now]).await
}
pub(crate) async fn fail(ex: &(impl Executor + ?Sized), id: &str, token: i64, due: i64, now: i64, error: &str, dead: bool) -> Result<u64, Error> {
    ex.execute("UPDATE game.jobs SET state=CASE WHEN $6 THEN 'dead' ELSE 'scheduled' END,due_at=$3,updated_at=$4,last_error=$5,lease_until=NULL,lease_token=NULL WHERE job_id=$1 AND state='running' AND lease_token=$2", &[&id,&token,&due,&now,&error,&dead]).await
}
