//! Event and durable-job entry points for corporation kick intents.
//! There is no polling loop; the scheduler and completed lifecycle events call the same check.
use super::exit;
use crate::{dao::corporations::pending_kicks as dao, db::{self, executor::Executor}, scheduler::{self, Job}, space::World, services::CallError};
use serde::Deserialize;
use std::{future::Future, pin::Pin, sync::Arc};
use anyhow::Result;

pub(crate) const KIND: &str = "corporations.pending_kick";
#[derive(Deserialize)] struct Payload { corporation_id: i64, character_id: i64, requested_at: i64 }
pub(crate) async fn schedule(ex: &(impl Executor + ?Sized), row: &dao::PendingKick) -> Result<(), db::Error> {
    let Some(due_at) = row.eligible_at else { return Ok(()) };
    let payload = format!(r#"{{"corporation_id":{},"character_id":{},"requested_at":{}}}"#, row.corporation_id, row.character_id, row.requested_at);
    scheduler::schedule(ex, &Job { id: exit::job_id(row.character_id), kind: KIND.into(), due_at, payload }).await?;
    Ok(())
}
pub(crate) fn payload(job: &Job) -> anyhow::Result<(i64, i64, i64)> { let p: Payload = serde_json::from_str(&job.payload)?; Ok((p.corporation_id,p.character_id,p.requested_at)) }
pub(crate) async fn cancel(ex: &(impl Executor + ?Sized), pending: &dao::PendingKick) -> Result<(), db::Error> {
    let id = exit::job_id(pending.character_id);
    let Some(job) = crate::dao::jobs::lock_job(ex, &id).await? else { return Ok(()) };
    if job.kind != KIND { return Ok(()) }
    let Ok((corp, target, stamp)) = payload(&job) else { return Ok(()) };
    if corp == pending.corporation_id && target == pending.character_id && stamp == pending.requested_at {
        scheduler::cancel(ex, &id).await?;
    }
    Ok(())
}

pub(crate) async fn cancel_issued(ex: &(impl Executor + ?Sized), corp: i64, issuer: i64) -> Result<(), db::Error> {
    for row in dao::by_issuer(ex, corp, issuer).await? {
        dao::delete(ex, corp, row.character_id).await?;
        cancel(ex, &row).await?;
    }
    Ok(())
}

pub(crate) async fn trigger(db: &crate::db::Db, world: &World, target: i64) -> Result<(), CallError> {
    let (corp, _) = super::membership(db, target).await?;
    let Some(row) = dao::get(db, corp, target).await.map_err(CallError::from)? else { return Ok(()) };
    exit::execute(db, world.sde(), world, corp, target, row.issuer_id, Some(row.requested_at)).await.map(|_| ())
}

pub(crate) fn handle(db: crate::db::Db, world: Arc<World>, job: Job) -> Pin<Box<dyn Future<Output = Result<()>> + Send>> {
    Box::pin(async move {
        let (corp, target, stamp) = payload(&job)?;
        let Some(row) = dao::get(&db, corp, target).await? else { return Ok(()) };
        if row.requested_at != stamp { return Ok(()) }
        exit::execute(&db, world.sde(), &world, corp, target, row.issuer_id, Some(stamp)).await.map_err(|error| anyhow::anyhow!(format_call_error(error)))?;
        Ok(())
    })
}
fn format_call_error(error: CallError) -> String { match error { CallError::Internal(error) => error.to_string(), CallError::User { msg, .. } => msg } }
