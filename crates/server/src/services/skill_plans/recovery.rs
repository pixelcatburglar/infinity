//! Recovery of viewer milestone snapshots after skill restoration and at startup.
//! Snapshots are derived from game skill tables; recovery never replays old levels.
//! It does not create completion notifications.
use std::{future::Future, pin::Pin, sync::Arc};
use crate::{db::{Db, outbox::Outbox}, scheduler::{self, Job}, sde::Sde, services::{db_err, CallError}, space::World};

/// Durable one-shot reconciliation after startup skill recovery.
pub(crate) const KIND: &str = "skill_plans.recover";
pub(crate) async fn character(db: &Db, sde: &Arc<Sde>, character: i64) -> Result<(), CallError> {
    db.transaction(|tx, _: &mut Outbox| {
        let sde = Arc::clone(sde);
        Box::pin(async move {
            super::progress::reconcile_tx(tx, &sde, character, None).await
        })
    }, |(), _| async move { Ok(()) }).await
}
pub(crate) async fn seed(db: &Db) -> Result<(), CallError> {
    let mut after = 0;
    let now = crate::client::filetime_now();
    loop {
        let page = crate::dao::skills::plan_progress::characters_after(db, after).await.map_err(db_err)?;
        if page.is_empty() { break; }
        for character in page {
            let job = Job { id: format!("{KIND}:{character}"), kind: KIND.into(), due_at: now, payload: character.to_string() };
            crate::dao::jobs::seed(db, &job, now).await.map_err(db_err)?;
            after = character;
        }
    }
    scheduler::wake();
    Ok(())
}
pub(crate) fn handle(db: Db, world: Arc<World>, job: Job) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send>> {
    Box::pin(async move {
        let character_id: i64 = serde_json::from_str(&job.payload)?;
        let result = async {
            crate::services::skills::expert_systems::advance_expired(&db, &world, world.sde(), character_id, crate::client::filetime_now(), |notification| notification.dispatch(&world, None)).await?;
            character(&db, world.sde(), character_id).await
        }.await;
        result.map_err(|error| match error { CallError::Internal(error) => error, CallError::User { msg, .. } => anyhow::anyhow!("skill plan recovery refused: {msg}") })
    })
}
#[cfg(test)] mod tests;
