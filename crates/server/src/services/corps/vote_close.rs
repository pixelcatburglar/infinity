//! Durable close of corporation votes, mirroring corporation voting deadlines.
//! One transaction performs the guarded close, the strict-majority decision and
//! sanctioned-action creation, the CEO election handover or restoration, and the
//! blueprint reservation release. The handler classifies the case before choosing
//! locks: general votes write no economy state, every sanctioned type runs under
//! the market lock.

use super::*;
use crate::dao::corporations::votes as dao;
use crate::scheduler::{self, Job};
use std::{future::Future, pin::Pin};

pub(crate) const KIND: &str = "corp.vote.close";

/// Seed a close job for every unclosed case at boot; dead jobs are revived and
/// live ones keep their lease.
pub(crate) async fn seed(db: &Db) -> Result<(), CallError> {
    let _market = crate::services::market::lock().await;
    let now = filetime_now();
    db.transaction(|tx, _: &mut Outbox| Box::pin(async move {
        for deadline in dao::open_deadlines(tx).await.map_err(db_err)? {
            let job = Job { id: format!("{KIND}:{}", deadline.case_id), kind: KIND.into(), due_at: deadline.due_at, payload: deadline.case_id.to_string() };
            crate::dao::jobs::seed(tx, &job, now).await.map_err(db_err)?;
        }
        Ok(())
    }), |value, _: Outbox| async move { scheduler::wake(); Ok(value) }).await
}

/// Guarded close of one case inside the caller's transaction, which already holds
/// the corporation admission locks. A repeated or early call is a no-op.
pub(super) async fn close_inner<X: Send>(ex: &(impl Executor + ?Sized), sde: &Sde, outbox: &mut Outbox<X>, id: i64, now: i64) -> Result<(), CallError> {
    let Some(case) = dao::lock_case(ex, id).await.map_err(db_err)? else { return Ok(()); };
    if case.closed_at.is_some() { return Ok(()); }
    if case.ends > now { return Err(CallError::Internal(anyhow::anyhow!("vote close before deadline"))); }
    if !dao::close(ex, id, now).await.map_err(db_err)? { return Ok(()); }
    sanctions::finish_in(ex, sde, outbox, &case, now).await?;
    votes::notify(ex, outbox, case.corporation_id, id, "OnVoteCaseChanged").await?;
    Ok(())
}

pub(crate) fn handle(db: Db, world: Arc<World>, job: Job) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send>> {
    Box::pin(async move {
        let id: i64 = serde_json::from_str(&job.payload)?;
        let Some(kind) = dao::case(&db, id).await?.map(|case| case.vote_type) else { return Ok(()) };
        // Read-only classification picks the lock before the transaction; the case
        // itself is locked only after admission inside it.
        let _market = if kind == vote_request::VOTE_GENERAL {
            None
        } else {
            Some(crate::services::market::lock().await)
        };
        let sde = Arc::clone(world.sde());
        db.transaction(|tx, outbox: &mut Outbox| {
            let sde = Arc::clone(&sde);
            Box::pin(async move {
                let Some(case) = dao::case(tx, id).await.map_err(db_err)? else { return Ok(()) };
                if case.vote_type != kind {
                    return Err(CallError::Internal(anyhow::anyhow!("vote case kind changed during close")));
                }
                // Admission covers the corporation plus every character the close
                // may touch: the frozen CEO and the elected candidate.
                let mut characters = Vec::new();
                if let Some(previous) = case.previous_ceo_id { characters.push(previous); }
                if case.vote_type == vote_request::VOTE_CEO {
                    let candidate = dao::options(tx, id).await.map_err(db_err)?.into_iter()
                        .find(|option| option.option_id == vote_request::OPTION_POSITIVE)
                        .and_then(|option| option.parameter);
                    if let Some(candidate) = candidate { characters.push(candidate); }
                }
                if !registry::lock_admission(tx, &[], &[case.corporation_id], &characters).await.map_err(db_err)? {
                    return Err(CallError::Internal(anyhow::anyhow!("corporation association changed during vote close")));
                }
                close_inner(tx, &sde, outbox, id, filetime_now()).await
            })
        }, |value, outbox| async move {
            let result = dispatch_world(value, outbox, &world);
            scheduler::wake();
            result
        }).await.map_err(|error| match error {
            CallError::Internal(error) => error,
            CallError::User { msg, .. } => anyhow::anyhow!("vote close refused: {msg}"),
        })
    })
}
