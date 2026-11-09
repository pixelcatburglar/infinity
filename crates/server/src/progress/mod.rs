//! Durable progress event registration and delivery.
//! The client does not consume this journal; Lua observes committed source events.
//! Domain-specific progress consumers and retention are not provided here.
//! Polling is scoped to registered durable keys. After successful startup
//! reconciliation this does not exclude any enabled production subscription.

pub(crate) mod codec;
pub(crate) mod migration;

use std::{collections::HashSet, future::Future};
use crate::{db::{outbox::Outbox, transaction::TxFuture}, lua::events::GameEvent, sde::Sde, services::CallError};
use tokio_postgres::Transaction;

pub(crate) type LiveHandler = fn(&GameEvent);
pub(crate) type DurableHandler = for<'a, 'tx> fn(&'a Transaction<'tx>, &'a mut Outbox, &'a Sde, &'a EventEnvelope) -> TxFuture<'a, (), CallError>;

#[derive(Clone, Copy)]
pub(crate) struct DurableRegistration {
    pub consumer_key: &'static str,
    pub kinds: &'static [&'static str],
    pub mode: DeliveryMode,
}

#[derive(Clone, Copy)]
pub(crate) enum DeliveryMode { Direct(DurableHandler), Prepared(PrepareDelivery) }

#[derive(Clone)]
pub(crate) struct ContentProgressResources {
    pub db: crate::db::Db,
    pub sde: std::sync::Arc<Sde>,
    pub world: std::sync::Arc<crate::space::World>,
    pub policies: crate::lua::RetainedPolicies,
}

pub(crate) type PrepareDelivery = fn(ContentProgressResources, DeliveryKey) -> std::pin::Pin<Box<dyn Future<Output = Result<PrepareOutcome, CallError>> + Send>>;
pub(crate) enum PrepareOutcome { Ready(std::sync::Arc<dyn PreparedDelivery>), AlreadyApplied }
pub(crate) trait PreparedDelivery: Send + Sync {
    fn key(&self) -> &DeliveryKey;
    /// Lock and revalidate the prepared dependencies only. Do not write domain
    /// state or outbox effects, or invoke Lua before the receipt is claimed.
    fn prelock<'a, 'tx>(&'a self, tx: &'a Transaction<'tx>) -> TxFuture<'a, (), ApplyError>;
    fn apply<'a, 'tx>(&'a self, tx: &'a Transaction<'tx>, outbox: &'a mut Outbox, sde: &'a Sde, event: &'a EventEnvelope) -> TxFuture<'a, (), ApplyError>;
}

#[derive(Debug)]
pub(crate) enum ApplyError { Reprepare, Call(CallError) }
impl From<CallError> for ApplyError { fn from(error: CallError) -> Self { Self::Call(error) } }
impl From<crate::db::Error> for ApplyError { fn from(error: crate::db::Error) -> Self { Self::Call(error.into()) } }
impl crate::db::transaction::TransactionError for ApplyError {
    fn is_retryable_transaction(&self) -> bool {
        match self { Self::Reprepare => false, Self::Call(error) => crate::db::transaction::TransactionError::is_retryable_transaction(error) }
    }
}
#[derive(Clone, Copy)]
pub(crate) enum ClaimMode { Background, Blocking }

#[derive(Debug)]
pub(crate) struct RegisterError(pub(crate) String);
impl std::fmt::Display for RegisterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str(&self.0) }
}
impl std::error::Error for RegisterError {}
#[derive(Clone)]
pub struct Registry {
    live: Vec<LiveHandler>,
    durable: Vec<DurableRegistration>,
    durable_keys: std::sync::Arc<[&'static str]>,
}
pub struct AfterCommitHandle {
    state: std::sync::Arc<std::sync::Mutex<AfterCommitState>>,
}
pub(crate) enum AfterCommitAction {
    Event(GameEvent),
    Child(AfterCommitHandle),
}
struct AfterCommitState {
    registry: Registry,
    actions: Vec<AfterCommitAction>,
}
impl AfterCommitHandle {
    pub(crate) fn new(registry: Registry, actions: Vec<AfterCommitAction>) -> Self {
        Self { state: std::sync::Arc::new(std::sync::Mutex::new(AfterCommitState { registry, actions })) }
    }
    pub(crate) fn empty(registry: &Registry) -> Self { Self::new(registry.clone(), Vec::new()) }
    pub(crate) fn publish(self) {
        let state = match std::sync::Arc::try_unwrap(self.state) {
            Ok(state) => state,
            Err(_) => panic!("after-commit publication handle still referenced"),
        };
        let AfterCommitState { registry, actions } = match state.into_inner() {
            Ok(state) => state,
            Err(_) => panic!("after-commit publication lock poisoned"),
        };
        for action in actions {
            match action {
                AfterCommitAction::Event(event) => registry.publish_committed(&event),
                AfterCommitAction::Child(child) => child.publish(),
            }
        }
    }
}
impl Registry {
    pub(crate) fn new(live: Vec<LiveHandler>, durable: Vec<DurableRegistration>) -> Result<Self, RegisterError> {
        let mut keys = HashSet::new();
        for registration in &durable {
            if !keys.insert(registration.consumer_key) {
                return Err(RegisterError(format!("duplicate progress consumer {}", registration.consumer_key)));
            }
            let mut kinds = HashSet::new();
            for kind in registration.kinds {
                if !kinds.insert(*kind) {
                    return Err(RegisterError(format!("duplicate kind {kind} for {}", registration.consumer_key)));
                }
            }
        }
        let durable_keys = durable.iter().map(|r| r.consumer_key).collect();
        Ok(Self { live, durable, durable_keys })
    }
    pub(crate) fn durable(&self) -> &[DurableRegistration] { &self.durable }
    pub(crate) fn registration(&self, key: &str) -> Option<&DurableRegistration> {
        self.durable.iter().find(|r| r.consumer_key == key)
    }
    pub(crate) fn publish_committed(&self, event: &GameEvent) {
        for handler in &self.live {
            if std::panic::catch_unwind(|| handler(event)).is_err() {
                tracing::warn!(kind = event.kind(), "progress observer panicked");
            }
        }
    }
}
impl Default for Registry {
    fn default() -> Self { Self { live: Vec::new(), durable: Vec::new(), durable_keys: std::sync::Arc::default() } }
}

pub(crate) fn publish_live(registry: &Registry, event: &GameEvent) {
    if matches!(event, GameEvent::Targeted { .. } | GameEvent::Damaged { .. } | GameEvent::ModuleCycleEnded { .. } | GameEvent::WarpArrived { .. }) {
        registry.publish_committed(event);
    }
}

pub(crate) struct EventEnvelope {
    pub event_id: String,
    pub event: GameEvent,
    pub schema_version: i32,
    pub kind: String,
    pub payload: String,
    pub occurred_at: i64,
    pub recorded_at: i64,
    pub source_namespace: String,
    pub source_key: String,
    pub ordinal: i64,
    pub context: String,
    pub causation: Option<String>,
    pub origin: String,
    pub subject_character_id: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DeliveryKey {
    pub consumer_key: String,
    pub event_id: String,
}

pub(crate) enum ApplyOutcome { Applied, AlreadyApplied, NotClaimed }

pub(crate) async fn apply_once_tx(
    tx: &Transaction<'_>, outbox: &mut Outbox, sde: &Sde,
    registration: &DurableRegistration, key: DeliveryKey,
    prepared: Option<&dyn PreparedDelivery>, claim_mode: ClaimMode,
) -> Result<ApplyOutcome, ApplyError> {
    if key.consumer_key != registration.consumer_key {
        return Err(CallError::Internal(anyhow::anyhow!("progress consumer key mismatch")).into());
    }
    match (registration.mode, prepared) {
        (DeliveryMode::Direct(_), None) => {}
        (DeliveryMode::Prepared(_), Some(plan)) if plan.key() == &key => {
            plan.prelock(tx).await?;
            if matches!(claim_mode, ClaimMode::Background)
                && !crate::dao::progress::try_claim(tx, &key, crate::client::filetime_now()).await? {
                return match crate::dao::progress::delivery(tx, &key).await? {
                    crate::dao::progress::DeliveryState::Applied => Ok(ApplyOutcome::AlreadyApplied),
                    crate::dao::progress::DeliveryState::Pending(_) => Ok(ApplyOutcome::NotClaimed),
                    crate::dao::progress::DeliveryState::Missing => Err(CallError::Internal(anyhow::anyhow!("progress delivery does not exist")).into()),
                };
            }
        }
        _ => return Err(CallError::Internal(anyhow::anyhow!("progress delivery mode or key mismatch")).into()),
    }
    let Some(event) = crate::dao::progress::lock_pending(tx, &key).await? else {
        return Ok(ApplyOutcome::AlreadyApplied);
    };
    let applied = match (registration.mode, prepared) {
        (DeliveryMode::Direct(handler), None) => handler(tx, outbox, sde, &event).await.map_err(ApplyError::Call),
        (DeliveryMode::Prepared(_), Some(plan)) => plan.apply(tx, outbox, sde, &event).await,
        _ => unreachable!(),
    };
    if let Err(error) = applied {
        tracing::warn!(consumer = registration.consumer_key, event_id = %event.event_id,
            kind = event.event.kind(), source_namespace = %event.source_namespace, source_key = %event.source_key,
            occurred_at = event.occurred_at, recorded_at = event.recorded_at, ordinal = event.ordinal,
            context_bytes = event.context.len(), has_causation = event.causation.is_some(),
            origin = %event.origin, subject_character_id = ?event.subject_character_id,
            "progress consumer failed");
        return Err(error);
    }
    crate::dao::progress::mark_applied(tx, &key).await?;
    Ok(ApplyOutcome::Applied)
}
pub type PrepareError = crate::db::Error;

pub struct PrepareCx {
    invocation: u128,
    namespace: Option<String>,
    ordinal: usize,
    events: Vec<crate::dao::progress::JournalEvent>,
}

impl PrepareCx {
    pub(crate) fn next_source(&mut self) -> Result<(String, usize), PrepareError> {
        let ordinal = self.ordinal;
        self.ordinal = ordinal.checked_add(1).ok_or_else(|| PrepareError::Progress(anyhow::anyhow!("progress ordinal overflow")))?;
        let invocation = self.invocation;
        let namespace = self.namespace.get_or_insert_with(|| format!("{invocation:032x}")).clone();
        Ok((namespace, ordinal))
    }

    pub(crate) fn stage(&mut self, event: crate::dao::progress::JournalEvent) {
        self.events.push(event);
    }
}

pub trait Prepare: Send {
    fn prepare<'a, 'tx>(
        &'a mut self,
        tx: &'a Transaction<'tx>,
        context: &'a mut PrepareCx,
    ) -> TxFuture<'a, (), PrepareError>;
}

pub trait AfterCommit: Send {
    fn after_commit(&mut self, registry: &Registry, committed: &HashSet<String>) -> AfterCommitHandle;
}

pub(crate) async fn prepare_batch<O: Prepare>(outbox: &mut O, tx: &Transaction<'_>, invocation: u128) -> Result<HashSet<String>, PrepareError> {
    let mut context = PrepareCx { invocation, namespace: None, ordinal: 0, events: Vec::new() };
    outbox.prepare(tx, &mut context).await?;
    let inserted = crate::dao::progress::insert_batch(tx, &context.events).await?;
    Ok(inserted.into_iter().filter_map(|(key, _, is_new)| is_new.then_some(key)).collect())
}

impl Prepare for () {
    fn prepare<'a, 'tx>(&'a mut self, _: &'a Transaction<'tx>, _: &'a mut PrepareCx) -> TxFuture<'a, (), PrepareError> {
        Box::pin(async { Ok(()) })
    }
}
impl AfterCommit for () {
    fn after_commit(&mut self, registry: &Registry, _: &HashSet<String>) -> AfterCommitHandle {
        AfterCommitHandle::empty(registry)
    }
}
impl Prepare for std::convert::Infallible {
    fn prepare<'a, 'tx>(&'a mut self, _: &'a Transaction<'tx>, _: &'a mut PrepareCx) -> TxFuture<'a, (), PrepareError> {
        match *self {}
    }
}
impl AfterCommit for std::convert::Infallible {
    fn after_commit(&mut self, _: &Registry, _: &HashSet<String>) -> AfterCommitHandle { match *self {} }
}

// Leaf operations contain no nested outboxes; each owner opts in explicitly.
macro_rules! leaf_effect {
    ($type:ty) => {
        impl crate::progress::Prepare for $type {
            fn prepare<'a, 'tx>(&'a mut self, _: &'a tokio_postgres::Transaction<'tx>, _: &'a mut crate::progress::PrepareCx)
                -> crate::db::transaction::TxFuture<'a, (), crate::progress::PrepareError> {
                Box::pin(async { Ok(()) })
            }
        }
        impl crate::progress::AfterCommit for $type {
            fn after_commit(&mut self, registry: &crate::progress::Registry, _: &std::collections::HashSet<String>) -> crate::progress::AfterCommitHandle {
                crate::progress::AfterCommitHandle::empty(registry)
            }
        }
    };
}
pub(crate) use leaf_effect;

leaf_effect!(Option<crate::space::Ball>);
leaf_effect!((i64, i64));

/// Server maintenance policy: bound each progress drain pass.
const DRAIN_BATCH: i64 = 64;
pub(crate) const DRAIN_KIND: &str = "periodic:progress.drain";

pub(crate) fn lua_observer(event: &GameEvent) {
    crate::lua::on_event(event.clone());
}

pub(crate) async fn reconcile(db: &crate::db::Db) -> Result<(), crate::db::Error> {
    let registry = db.progress_registry.clone();
    db.transaction(
        |tx, _: &mut ()| {
            let registry = registry.clone();
            Box::pin(async move { crate::dao::progress::reconcile(tx, &registry).await })
        },
        |(), ()| async { Ok(()) },
    ).await
}

pub(crate) async fn seed(db: &crate::db::Db) -> Result<(), crate::db::Error> {
    let now = crate::client::filetime_now();
    let job = crate::scheduler::Job { id: DRAIN_KIND.into(), kind: DRAIN_KIND.into(), due_at: now, payload: "{}".into() };
    crate::dao::jobs::seed(db, &job, now).await?;
    crate::scheduler::wake();
    Ok(())
}

pub(crate) fn drain(db: crate::db::Db, world: std::sync::Arc<crate::space::World>, _: crate::scheduler::Job)
    -> std::pin::Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send>> {
    Box::pin(async move {
        let due = crate::dao::progress::due(&db, crate::client::filetime_now(), DRAIN_BATCH, &db.progress_registry().durable_keys).await?;
        for key in due {
            let registration = db.progress_registry().registration(&key.consumer_key).copied()
                .ok_or_else(|| anyhow::anyhow!("missing progress consumer {}", key.consumer_key))?;
            let sde = world.sde().clone();
            let plan = match registration.mode {
                DeliveryMode::Direct(_) => Ok(None),
                DeliveryMode::Prepared(prepare) => {
                    let resources = crate::lua::retained_policies().map(|policies| ContentProgressResources {
                        db: db.clone(), sde: sde.clone(), world: world.clone(), policies,
                    });
                    match resources {
                        Err(error) => Err(error),
                        Ok(resources) => match prepare(resources, key.clone()).await {
                            Ok(PrepareOutcome::Ready(plan)) => Ok(Some(plan)),
                            Ok(PrepareOutcome::AlreadyApplied) => match crate::dao::progress::delivery(&db, &key).await {
                                Ok(crate::dao::progress::DeliveryState::Applied) => continue,
                                Ok(_) => Err(CallError::Internal(anyhow::anyhow!("unproven applied progress delivery"))),
                                Err(error) => Err(error.into()),
                            },
                            Err(error) => Err(error),
                        },
                    }
                }
            };
            let plan = match plan {
                Ok(plan) => plan,
                Err(error) => {
                    crate::dao::progress::failed(&db, &key, crate::client::filetime_now(), &format!("{error:?}")).await?;
                    continue;
                }
            };
            let dispatch_world = world.clone();
            let result: Result<(), ApplyError> = db.transaction(
                |tx, outbox: &mut Outbox| {
                    let key = key.clone();
                    let sde = sde.clone();
                    let plan = plan.clone();
                    Box::pin(async move {
                        if matches!(registration.mode, DeliveryMode::Direct(_))
                            && !crate::dao::progress::try_claim(tx, &key, crate::client::filetime_now()).await? {
                            return Ok(());
                        }
                        apply_once_tx(tx, outbox, &sde, &registration, key, plan.as_deref(), ClaimMode::Background).await?;
                        Ok(())
                    })
                },
                |(), outbox| async move { crate::db::outbox::dispatch_world((), outbox, &dispatch_world).map_err(ApplyError::Call) },
            ).await;
            if let Err(error) = result {
                crate::dao::progress::failed(&db, &key, crate::client::filetime_now(), &format!("{error:?}")).await?;
            }
        }
        Ok(())
    })
}

#[cfg(test)] pub(crate) mod tests;
