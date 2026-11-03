//! A single revision-fenced durable job at the end of each finite grace period.
//! Reads and updates `game.account_clone_grades`, records expiry in
//! `game.account_clone_grade_audit`, and uses `game.jobs` for durable delivery.
//! Revision and deadline fences guard replay; only grace expiry changes the
//! effective grade, with no separate transition at `omega_until`. Client grade
//! notifications follow `omega/client/clone_grade_svc.py`.
use std::{future::Future, pin::Pin, sync::Arc};
use serde::{Deserialize, Serialize};
use crate::{client::filetime_now, dao::accounts::subscriptions as dao, db::{executor::Executor, outbox::{dispatch_world, Outbox}, Db}, scheduler::{self, Job}, services::CallError, space::World};
use super::{management, rules};

pub(crate) const KIND: &str = "subscription.clone_grade.expire";

#[derive(Clone, Copy, Serialize, Deserialize)]
struct Payload { user: i64, revision: i64, deadline: i64 }

fn id(user: i64) -> String { format!("{KIND}:{user}") }

pub(crate) async fn schedule(ex: &(impl Executor + ?Sized), row: &dao::CloneGrade) -> Result<(), CallError> {
    if row.grade == rules::OMEGA && row.omega_until.is_some() {
        let deadline = row.grace_until.ok_or_else(|| CallError::Internal(anyhow::anyhow!("finite clone grade missing grace deadline")))?;
        let payload = serde_json::to_string(&Payload { user: row.user_id, revision: row.revision, deadline }).map_err(|e| CallError::Internal(e.into()))?;
        scheduler::schedule(ex, &Job { id: id(row.user_id), kind: KIND.into(), due_at: deadline, payload }).await?;
    } else {
        scheduler::cancel(ex, &id(row.user_id)).await?;
    }
    Ok(())
}

pub(crate) fn handle(db: Db, world: Arc<World>, job: Job) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send>> {
    Box::pin(async move {
        let payload: Payload = serde_json::from_str(&job.payload)?;
        if job.kind != KIND || job.id != id(payload.user) { return Ok(()); }
        let now = filetime_now();
        let result: Result<(), CallError> = db.transaction(
            |tx, outbox: &mut Outbox| {
                let payload = payload;
                Box::pin(async move {
                    let row = dao::lock(tx, payload.user).await?;
                    if row.revision != payload.revision || row.grade != rules::OMEGA || row.omega_until.is_none() || row.grace_until != Some(payload.deadline) { return Ok(()); }
                    if now < payload.deadline {
                        schedule(tx, &row).await?;
                        return Ok(());
                    }
                    if !dao::set_grade(tx, row.user_id, rules::ALPHA, None, None, row.revision).await? {
                        return Err(CallError::Internal(anyhow::anyhow!("clone-grade expiry revision conflict or overflow")));
                    }
                    dao::audit(tx, row.user_id, None, &row, rules::ALPHA, None, None, "expiry").await?;
                    management::notify(outbox, row.user_id, rules::ALPHA, 0, true, true);
                    Ok(())
                })
            },
            |value, outbox| { let world = Arc::clone(&world); async move { let result = dispatch_world(value, outbox, &world); scheduler::wake(); result } },
        ).await;
        result.map_err(|error| anyhow::anyhow!("clone-grade expiry failed: {error:?}"))
    })
}

pub(crate) async fn recover(db: &Db) -> Result<(), CallError> {
    for row in dao::finite(db).await? { schedule(db, &row).await?; }
    scheduler::wake();
    Ok(())
}
