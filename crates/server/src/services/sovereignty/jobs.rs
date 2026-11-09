//! Durable Hub self-destruct scheduler identity and handler support.
//! Jobs are tied to one real item; their payload carries the expected request guards.
//! Cancellation deletes the same ID even if its lease was already claimed.

use std::{future::Future, pin::Pin, sync::Arc};
use serde::{Deserialize, Serialize};
use crate::{db::Db, scheduler::{self, Job}, space::World};

/// Durable scheduler kind for Hub self-destruction.
pub(crate) const DESTRUCT_KIND: &str = "sovereignty.destruct";

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct DestructPayload {
    pub item: i64,
    pub system: i64,
    pub ownership_epoch: i64,
    pub system_revision: i64,
    pub due: i64,
}

pub(crate) fn registration() -> scheduler::Registration {
    scheduler::register(DESTRUCT_KIND, handle)
}

fn handle(db: Db, world: Arc<World>, job: Job) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send>> {
    Box::pin(async move {
        let payload: DestructPayload = serde_json::from_str(&job.payload)?;
        if job.kind != DESTRUCT_KIND || job.id != destruct_id(payload.item) || job.due_at != payload.due {
            return Err(anyhow::anyhow!("invalid Hub destruct job"));
        }
        let resources = super::Resources { db, sde: world.sde().clone(), world };
        super::lifecycle::finish_destruct(resources, payload).await.map_err(|error| anyhow::anyhow!("Hub destruct failed: {error:?}"))
    })
}
/// Stable per-item job identity; rescheduling invalidates the previous scheduler lease.
pub(crate) fn destruct_id(item: i64) -> String {
    format!("sovereignty:destruct:{item}")
}
