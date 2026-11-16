//! Durable progress consumer for development indices.
//! The client never sees these receipts; they are the only write path from the
//! original NPC-death and mining journal to `game.sov_development*`. The shared
//! progress driver claims, locks and acknowledges every receipt; this module
//! only prepares outside locks and applies inside them.

use std::{future::Future, pin::Pin, sync::Arc};
use crate::db::Transaction;
use crate::{client::filetime_now, dao::{lua_content::activations, progress::{self as journal, DeliveryState}}, db::{outbox::Outbox, transaction::TxFuture}, lua::content::indices::NAMESPACE, progress::{ApplyError, ContentProgressResources, DeliveryKey, DeliveryMode, DurableRegistration, EventEnvelope, PrepareOutcome, PreparedDelivery}, sde::Sde, services::{db_err, CallError}};
use super::{activation, replay, source};

/// Server policy: one durable consumer across all retained policy versions.
pub(crate) const CONSUMER_KEY: &str = "sovereignty-indices";
const KINDS: &[&str] = &["NpcDestroyed", "OreMined", "GasHarvested"];

pub(crate) fn registration() -> DurableRegistration {
    DurableRegistration { consumer_key: CONSUMER_KEY, kinds: KINDS, mode: DeliveryMode::Prepared(prepare) }
}

/// One prepared delivery: the captured namespace head plus the write set the
/// fold produced outside locks. Excluded sources carry no domain work.
struct Plan {
    key: DeliveryKey,
    identity: EventEnvelope,
    head_ordinal: Option<i64>,
    policies: crate::lua::RetainedPolicies,
    refresh: Option<replay::PreparedRefresh>,
}
impl PreparedDelivery for Plan {
    fn key(&self) -> &DeliveryKey { &self.key }
    fn prelock<'a, 'tx>(&'a self, tx: &'a Transaction<'tx>) -> TxFuture<'a, (), ApplyError> {
        Box::pin(async move {
            activations::lock_namespace(tx, NAMESPACE, false).await?;
            if !self.policies.is_current(NAMESPACE, self.head_ordinal)? {
                return Err(ApplyError::Reprepare);
            }
            if let Some(refresh) = &self.refresh {
                refresh.prelock(tx).await?;
            }
            Ok(())
        })
    }
    fn apply<'a, 'tx>(&'a self, tx: &'a Transaction<'tx>, outbox: &'a mut Outbox, _sde: &'a Sde, event: &'a EventEnvelope) -> TxFuture<'a, (), ApplyError> {
        Box::pin(async move {
            if &self.identity != event {
                return Err(ApplyError::Reprepare);
            }
            if let Some(refresh) = &self.refresh {
                refresh.apply(tx, outbox).await?;
            }
            Ok(())
        })
    }
}

/// Prepare one canonical receipt outside every lock; the shared driver owns the
/// transaction, claim, pending lock and the single acknowledgement.
pub(crate) fn prepare(resources: ContentProgressResources, key: DeliveryKey) -> Pin<Box<dyn Future<Output = Result<PrepareOutcome, ApplyError>> + Send>> {
    Box::pin(async move {
        let event = match journal::delivery(&resources.db, &key).await? {
            DeliveryState::Applied => return Ok(PrepareOutcome::AlreadyApplied),
            DeliveryState::Terminal => return Err(ApplyError::Terminal),
            DeliveryState::Missing => return Err(ApplyError::Call(db_err(anyhow::anyhow!("development-index delivery missing")))),
            DeliveryState::Pending(event) => event,
        };
        let now = filetime_now();
        let future = match &event.event {
            crate::lua::events::GameEvent::NpcDestroyed { occurred_at, .. } => *occurred_at > now,
            crate::lua::events::GameEvent::OreMined { slices, .. } | crate::lua::events::GameEvent::GasHarvested { slices, .. } => slices.iter().any(|slice| slice.occurred_at > now),
            _ => false,
        };
        if future {
            return Err(ApplyError::Call(db_err(anyhow::anyhow!("development-index source slice in the future"))));
        }
        let identities = resources.policies.activations(NAMESPACE)?;
        let refresh = match source::map(&event, &resources.sde).map_err(ApplyError::Permanent)? {
            None => None,
            Some(fact) => {
                let history = activation::load(&resources.policies).map_err(CallError::from)?;
                if history.identities != identities {
                    return Err(ApplyError::Call(db_err(anyhow::anyhow!("development-index activation changed during prepare"))));
                }
                if fact.slices.iter().all(|slice| slice.order.occurred_at < history.segments[0].starts_at) {
                    None
                } else {
                    Some(replay::plan(&resources.db, &resources.sde, &resources.policies, history, fact.system_id, Some(&event.event_id), &fact.slices, now).await?)
                }
            }
        };
        Ok(PrepareOutcome::Ready(Arc::new(Plan { key, identity: event, head_ordinal: identities.last().map(|row| row.ordinal), policies: resources.policies.clone(), refresh })))
    })
}

#[cfg(all(test, feature = "tests-content"))]
mod tests;
