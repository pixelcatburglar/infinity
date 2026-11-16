//! Corporation skill-plan publication ordering and committed audiences.
//! The notice hub holds one asynchronous gate per corporation across the write
//! transaction and its post-commit dispatch, so a later write cannot overtake an
//! earlier commit's notices on any stream. The audience is the corporation and
//! member snapshot committed with the write; the notice hub only intersects it
//! with current sessions and never reads the database.
use std::sync::Arc;
use crate::{dao::characters::members, db::{executor::Executor, outbox::Outbox}, gateway::notices::GatewayNotice, services::CallError};
use super::Error;

/// The corporation and its sorted, deduplicated member identifiers committed
/// with the write.
pub(crate) struct Audience { pub(crate) corporation: i64, pub(crate) members: Arc<[i64]> }
async fn audience(ex: &(impl Executor + ?Sized), corporation: i64) -> Result<Audience, crate::db::Error> {
    let mut ids = members::active_ids(ex, corporation).await?;
    ids.sort_unstable();
    ids.dedup();
    Ok(Audience { corporation, members: ids.into() })
}

/// A gateway notice the adapter binds to the committed corporation and member
/// snapshot.
pub(crate) type Notice = dyn Fn(Audience) -> Result<GatewayNotice, CallError> + Send + Sync;

/// Snapshot the committed corporation and member set once inside the write
/// transaction and queue the bound notice for post-commit dispatch. A rollback
/// queues nothing.
pub(crate) async fn publish(ex: &(impl Executor + ?Sized), outbox: &mut Outbox, corporation: i64, notice: &Notice) -> Result<(), Error> {
    let audience = audience(ex, corporation).await.map_err(Error::from)?;
    outbox.notify_gateway(notice(audience).map_err(Error::from)?);
    Ok(())
}

#[cfg(all(test, feature = "tests-skills"))] mod tests;
