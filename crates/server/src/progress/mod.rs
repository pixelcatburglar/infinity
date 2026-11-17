//! Durable progress event registration and delivery.
//! The client does not consume this journal; Lua observes committed source events.
//! Domain-specific progress consumers and retention are not provided here.
//! Enabled durable subscriptions are reconciled once at startup into an
//! immutable projection; receipt creation and polling use only that snapshot,
//! so a database `enabled` change applies at the next restart. A receipt whose
//! consumer keeps rejecting it permanently turns terminal and stays recorded
//! until an operator reschedules it; see docs/database.md.

pub(crate) mod codec;
pub(crate) mod migration;

use std::{collections::HashSet, future::Future};
use crate::{db::{outbox::Outbox, transaction::TxFuture}, lua::events::GameEvent, sde::Sde, services::CallError};
use crate::db::Transaction;

pub(crate) type LiveHandler = fn(&GameEvent);
pub(crate) type DurableHandler = for<'a, 'tx> fn(&'a Transaction<'tx>, &'a mut Outbox, &'a Sde, &'a EventEnvelope) -> TxFuture<'a, (), ApplyError>;

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

pub(crate) type PrepareDelivery = fn(ContentProgressResources, DeliveryKey) -> std::pin::Pin<Box<dyn Future<Output = Result<PrepareOutcome, ApplyError>> + Send>>;
pub(crate) enum PrepareOutcome { Ready(std::sync::Arc<dyn PreparedDelivery>), AlreadyApplied }
pub(crate) trait PreparedDelivery: Send + Sync {
    fn key(&self) -> &DeliveryKey;
    /// Lock and revalidate the prepared dependencies only. Do not write domain
    /// state or outbox effects, or invoke Lua before the receipt is claimed.
    fn prelock<'a, 'tx>(&'a self, tx: &'a Transaction<'tx>) -> TxFuture<'a, (), ApplyError>;
    fn apply<'a, 'tx>(&'a self, tx: &'a Transaction<'tx>, outbox: &'a mut Outbox, sde: &'a Sde, event: &'a EventEnvelope) -> TxFuture<'a, (), ApplyError>;
}

/// The classified result of applying one receipt: `Reprepare` and `Call` are
/// transient and retry with bounded backoff, `Permanent` consumes the receipt's
/// capped rejection budget until it turns `Terminal`, and `Terminal` reports a
/// receipt already terminal rather than a new failure.
#[derive(Debug)]
pub(crate) enum ApplyError { Reprepare, Call(CallError), Permanent(CallError), Terminal }
impl From<CallError> for ApplyError { fn from(error: CallError) -> Self { Self::Call(error) } }
impl From<crate::db::Error> for ApplyError { fn from(error: crate::db::Error) -> Self { Self::Call(error.into()) } }
impl crate::db::transaction::TransactionError for ApplyError {
    fn is_retryable_transaction(&self) -> bool {
        match self { Self::Call(error) => crate::db::transaction::TransactionError::is_retryable_transaction(error), _ => false }
    }
}
impl ApplyError {
    /// Fold a non-reprepare apply failure back into its call error; `Reprepare`
    /// and `Terminal` have no call error and surface as explicit internal faults.
    pub(crate) fn into_call(self) -> CallError {
        match self {
            Self::Call(error) | Self::Permanent(error) => error,
            Self::Reprepare => CallError::Internal(anyhow::anyhow!("progress apply required repreparation")),
            Self::Terminal => CallError::Internal(anyhow::anyhow!("progress receipt is terminal")),
        }
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
    live: std::sync::Arc<[LiveHandler]>,
    durable: std::sync::Arc<[DurableRegistration]>,
    enabled_keys: std::sync::Arc<[&'static str]>,
    subscription_consumers: std::sync::Arc<[&'static str]>,
    subscription_kinds: std::sync::Arc<[&'static str]>,
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
        let AfterCommitState { registry, actions } = crate::poison::lock_or_exit(state.into_inner());
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
        Ok(Self {
            live: live.into(), durable: durable.into(),
            enabled_keys: std::sync::Arc::default(),
            subscription_consumers: std::sync::Arc::default(),
            subscription_kinds: std::sync::Arc::default(),
        })
    }
    /// Install the immutable enabled projection read back by startup
    /// reconciliation. Every declared consumer must have a row whose kind set
    /// equals the code declaration; only enabled consumers enter the
    /// projections, borrowing the declaration strings once.
    pub(crate) fn with_subscriptions(mut self, rows: &[crate::dao::progress::SubscriptionRow]) -> Result<Self, RegisterError> {
        let mut enabled_keys = Vec::new();
        let mut consumers = Vec::new();
        let mut kinds = Vec::new();
        for registration in self.durable.iter() {
            let Some(row) = rows.iter().find(|row| row.consumer_key == registration.consumer_key) else {
                return Err(RegisterError(format!("missing subscription row for {}", registration.consumer_key)));
            };
            if row.kinds.len() != registration.kinds.len()
                || !registration.kinds.iter().all(|kind| row.kinds.iter().any(|stored| stored.as_str() == *kind)) {
                return Err(RegisterError(format!("subscription kinds for {} do not match the declared set", registration.consumer_key)));
            }
            if row.enabled {
                enabled_keys.push(registration.consumer_key);
                for kind in registration.kinds {
                    consumers.push(registration.consumer_key);
                    kinds.push(*kind);
                }
            }
        }
        self.enabled_keys = enabled_keys.into();
        self.subscription_consumers = consumers.into();
        self.subscription_kinds = kinds.into();
        Ok(self)
    }
    /// Enabled consumer keys of the installed projection.
    pub(crate) fn enabled_keys(&self) -> &[&str] { &self.enabled_keys }
    /// Paired (consumer, kind) projection borrowed from the declarations.
    pub(crate) fn subscription_arrays(&self) -> (&[&str], &[&str]) {
        (&self.subscription_consumers, &self.subscription_kinds)
    }
    pub(crate) fn durable(&self) -> &[DurableRegistration] { &self.durable }
    pub(crate) fn registration(&self, key: &str) -> Option<&DurableRegistration> {
        self.durable.iter().find(|r| r.consumer_key == key)
    }
    pub(crate) fn publish_committed(&self, event: &GameEvent) {
        for handler in self.live.iter() {
            if std::panic::catch_unwind(|| handler(event)).is_err() {
                tracing::warn!(kind = event.kind(), "progress observer panicked");
            }
        }
    }
}
impl Default for Registry {
    fn default() -> Self {
        Self {
            live: std::sync::Arc::default(), durable: std::sync::Arc::default(),
            enabled_keys: std::sync::Arc::default(),
            subscription_consumers: std::sync::Arc::default(),
            subscription_kinds: std::sync::Arc::default(),
        }
    }
}

pub(crate) fn publish_live(registry: &Registry, event: &GameEvent) {
    if matches!(event, GameEvent::Targeted { .. } | GameEvent::Damaged { .. } | GameEvent::ModuleCycleEnded { .. } | GameEvent::WarpArrived { .. }) {
        registry.publish_committed(event);
    }
}

#[derive(PartialEq)]
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
                    crate::dao::progress::DeliveryState::Terminal => Err(ApplyError::Terminal),
                    crate::dao::progress::DeliveryState::Missing => Err(CallError::Internal(anyhow::anyhow!("progress delivery does not exist")).into()),
                };
            }
        }
        _ => return Err(CallError::Internal(anyhow::anyhow!("progress delivery mode or key mismatch")).into()),
    }
    let event = match crate::dao::progress::lock_delivery(tx, &key).await? {
        crate::dao::progress::DeliveryState::Pending(event) => event,
        crate::dao::progress::DeliveryState::Applied => return Ok(ApplyOutcome::AlreadyApplied),
        crate::dao::progress::DeliveryState::Terminal => return Err(ApplyError::Terminal),
        crate::dao::progress::DeliveryState::Missing => return Err(CallError::Internal(anyhow::anyhow!("progress delivery does not exist")).into()),
    };
    let applied = match (registration.mode, prepared) {
        (DeliveryMode::Direct(handler), None) => handler(tx, outbox, sde, &event).await,
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

pub(crate) async fn prepare_batch<O: Prepare>(outbox: &mut O, tx: &Transaction<'_>, invocation: u128, registry: &Registry) -> Result<HashSet<String>, PrepareError> {
    let mut context = PrepareCx { invocation, namespace: None, ordinal: 0, events: Vec::new() };
    outbox.prepare(tx, &mut context).await?;
    let (consumers, kinds) = registry.subscription_arrays();
    let inserted = crate::dao::progress::insert_batch(tx, &context.events, consumers, kinds).await?;
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
            fn prepare<'a, 'tx>(&'a mut self, _: &'a crate::db::Transaction<'tx>, _: &'a mut crate::progress::PrepareCx)
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

/// Reconcile declared subscriptions, then install the resulting immutable
/// enabled projection. The snapshot is installed only after the transaction
/// succeeded; the returned `Db` carries it while the pre-reconcile `Db` keeps
/// the declaration-only registry.
pub(crate) async fn reconcile(db: crate::db::Db) -> Result<crate::db::Db, crate::db::Error> {
    let registry = db.progress_registry.clone();
    let rows = db.transaction(
        |tx, _: &mut ()| {
            let registry = registry.clone();
            Box::pin(async move { crate::dao::progress::reconcile(tx, &registry).await })
        },
        |rows, ()| async move { Ok(rows) },
    ).await?;
    let projected = registry.as_ref().clone().with_subscriptions(&rows)
        .map_err(|error| crate::db::Error::Progress(anyhow::anyhow!(error.to_string())))?;
    for row in rows.iter().filter(|row| !row.enabled) {
        tracing::info!(consumer = %row.consumer_key, "progress consumer disabled");
    }
    Ok(db.with_progress_registry(projected))
}

pub(crate) async fn seed(db: &crate::db::Db) -> Result<(), crate::db::Error> {
    let now = crate::client::filetime_now();
    let job = crate::scheduler::Job { id: DRAIN_KIND.into(), kind: DRAIN_KIND.into(), due_at: now, payload: "{}".into() };
    crate::dao::jobs::seed(db, &job, now).await?;
    crate::scheduler::wake();
    Ok(())
}

/// Record one failed delivery pass after the apply transaction returned;
/// only an update that actually turned the receipt terminal warns once.
async fn record_failure(
    db: &crate::db::Db, attempt: &crate::dao::progress::DeliveryAttempt,
    error: ApplyError, permanent_retry_limit: i32,
) -> Result<(), crate::db::Error> {
    let permanent = matches!(error, ApplyError::Permanent(_));
    if let Some(update) = crate::dao::progress::failed(
        db, attempt, crate::client::filetime_now(), &format!("{error:?}"),
        permanent, permanent_retry_limit,
    ).await? {
        if update.terminal {
            tracing::warn!(consumer = %attempt.key.consumer_key, event_id = %attempt.key.event_id,
                attempts = update.attempts, permanent_rejections = update.permanent_rejections,
                permanent_retry_limit, ?error, "progress delivery terminal");
        }
    }
    Ok(())
}

pub(crate) fn drain(db: crate::db::Db, world: std::sync::Arc<crate::space::World>, _: crate::scheduler::Job)
    -> std::pin::Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send>> {
    Box::pin(async move {
        let permanent_retry_limit = crate::config::game().progress.permanent_retry_limit;
        let due = crate::dao::progress::due(&db, crate::client::filetime_now(), DRAIN_BATCH, db.progress_registry().enabled_keys()).await?;
        for attempt in due {
            let key = &attempt.key;
            let registration = db.progress_registry().registration(&key.consumer_key).copied()
                .ok_or_else(|| anyhow::anyhow!("missing progress consumer {}", key.consumer_key))?;
            crate::runtime_watchdog::watch("progress", registration.consumer_key, async {
                let sde = world.sde().clone();
                let plan = match registration.mode {
                    DeliveryMode::Direct(_) => Ok(None),
                    DeliveryMode::Prepared(prepare) => {
                        let resources = crate::lua::retained_policies().map(|policies| ContentProgressResources {
                            db: db.clone(), sde: sde.clone(), world: world.clone(), policies,
                        }).map_err(ApplyError::Call);
                        match resources {
                            Err(error) => Err(error),
                            Ok(resources) => match prepare(resources, key.clone()).await {
                                Ok(PrepareOutcome::Ready(plan)) => Ok(Some(plan)),
                                Ok(PrepareOutcome::AlreadyApplied) => match crate::dao::progress::delivery(&db, key).await {
                                    Ok(crate::dao::progress::DeliveryState::Applied) => return Ok(()),
                                    Ok(crate::dao::progress::DeliveryState::Terminal) => return Ok(()),
                                    Ok(_) => Err(ApplyError::Call(CallError::Internal(anyhow::anyhow!("unproven applied progress delivery")))),
                                    Err(error) => Err(error.into()),
                                },
                                Err(error) => Err(error),
                            },
                        }
                    }
                };
                let plan = match plan {
                    Ok(plan) => plan,
                    Err(ApplyError::Terminal) => return Ok(()),
                    Err(error) => {
                        record_failure(&db, &attempt, error, permanent_retry_limit).await?;
                        return Ok(());
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
                    if !matches!(error, ApplyError::Terminal) {
                        record_failure(&db, &attempt, error, permanent_retry_limit).await?;
                    }
                }
                Ok::<(), anyhow::Error>(())
            }).await?;
        }
        Ok(())
    })
}

#[cfg(all(test, any(feature = "tests-combat", feature = "tests-content", feature = "tests-economy", feature = "tests-gateway", feature = "tests-lua", feature = "tests-skills", feature = "tests-storage")))] pub(crate) mod tests;
