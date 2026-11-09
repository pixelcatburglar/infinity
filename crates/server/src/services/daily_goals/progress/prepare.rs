//! Preparation of native daily-goal delivery plans.
//! Client contribution methods use frozen daily goals and retained content;
//! delivery receipts remain owned by the shared progress driver.
use std::{collections::{BTreeMap, HashMap}, future::Future, pin::Pin, sync::Arc};
use crate::{dao::{accounts::subscriptions::{self, CloneGrade}, characters::persistence, daily_goals::{instances, periods::PeriodRow, rewards}, progress::{self as journal, DeliveryState}}, db::{outbox::Outbox, transaction::TxFuture}, lua::content::PinnedPolicy, progress::{ApplyError, ContentProgressResources, DeliveryKey, EventEnvelope, PrepareOutcome, PreparedDelivery}, sde::Sde, services::CallError};
use tokio_postgres::Transaction;
use super::{apply, source, state::{self, Change, GoalMeta, ReadSet, VirtualGoal, VirtualState}};
use super::super::{assignment, frozen, Error, Resources};

/// Server policy limiting goal locks in a paid catch-up operation.
pub(crate) const MAX_PAYMENT_AFFECTED_GOALS: usize = 13;
/// Server policy limiting receipts in a paid catch-up operation.
pub(crate) const MAX_PAYMENT_PENDING_DELIVERIES: usize = 13;

fn failure(error: Error) -> CallError {
    match error { Error::Internal(error) => error, other => CallError::Internal(anyhow::anyhow!("daily preparation: {other:?}")) }
}

struct Identity {
    event_id: String, schema_version: i32, kind: String, payload: String,
    occurred_at: i64, recorded_at: i64, namespace: String, source_key: String,
    ordinal: i64, context: String, causation: Option<String>, origin: String, subject: Option<i64>,
}
impl Identity {
    fn new(e: &EventEnvelope) -> Self { Self { event_id: e.event_id.clone(), schema_version: e.schema_version, kind: e.kind.clone(), payload: e.payload.clone(), occurred_at: e.occurred_at, recorded_at: e.recorded_at, namespace: e.source_namespace.clone(), source_key: e.source_key.clone(), ordinal: e.ordinal, context: e.context.clone(), causation: e.causation.clone(), origin: e.origin.clone(), subject: e.subject_character_id } }
    fn matches(&self, e: &EventEnvelope) -> bool { self.event_id == e.event_id && self.schema_version == e.schema_version && self.kind == e.kind && self.payload == e.payload && self.occurred_at == e.occurred_at && self.recorded_at == e.recorded_at && self.namespace == e.source_namespace && self.source_key == e.source_key && self.ordinal == e.ordinal && self.context == e.context && self.causation == e.causation && self.origin == e.origin && self.subject == e.subject_character_id }
}

/// A delivery owns only the canonical identity and its prepared state, never World.
pub(crate) struct ConcretePreparedDelivery { key: DeliveryKey, identity: Identity, change: Option<PreparedChange> }
impl PreparedDelivery for ConcretePreparedDelivery {
    fn key(&self) -> &DeliveryKey { &self.key }
    fn prelock<'a, 'tx>(&'a self, tx: &'a Transaction<'tx>) -> TxFuture<'a, (), ApplyError> {
        Box::pin(async move { if let Some(change) = &self.change { change.prelock(tx).await?; } Ok(()) })
    }
    fn apply<'a, 'tx>(&'a self, tx: &'a Transaction<'tx>, outbox: &'a mut Outbox, _sde: &'a Sde, event: &'a EventEnvelope) -> TxFuture<'a, (), ApplyError> {
        Box::pin(async move {
            if !self.identity.matches(event) { return Err(ApplyError::Reprepare); }
            if let Some(change) = &self.change { change.apply(tx, outbox).await?; }
            Ok(())
        })
    }
}

/// A direct mutation prepared outside SQL locks.
pub(crate) struct PreparedChange { read: ReadSet, change: Change, grade: CloneGrade, at: i64 }
impl PreparedChange {
    pub(super) fn new(read: ReadSet, change: Change, grade: CloneGrade, at: i64) -> Self { Self { read, change, grade, at } }
    pub(crate) async fn prelock(&self, tx: &Transaction<'_>) -> Result<(), ApplyError> { apply::prelock(tx, &self.read).await }
    pub(crate) async fn apply(&self, tx: &Transaction<'_>, outbox: &mut Outbox) -> Result<(), ApplyError> { apply::apply(tx, outbox, self.read.owner, &self.grade, self.at, &self.change).await }
}

pub(super) async fn load_state(resources: &Resources, user: i64, character: i64, periods: BTreeMap<(String, i64), PeriodRow>, mut pins: HashMap<(String, String), PinnedPolicy>, bounded: bool) -> Result<VirtualState, Error> {
    let mut rows = Vec::new();
    if bounded {
        let days: Vec<_> = periods.keys().filter(|(kind, _)| kind == "day").map(|(_, start)| *start).collect();
        let months: Vec<_> = periods.keys().filter(|(kind, _)| kind == "month").map(|(_, start)| *start).collect();
        rows = instances::bounded_period_union(&resources.db, character, &days, &months, (MAX_PAYMENT_AFFECTED_GOALS + 1) as i64).await?;
        if rows.len() > MAX_PAYMENT_AFFECTED_GOALS { return Err(Error::Forbidden); }
    } else {
        for (kind, start) in periods.keys() { rows.extend(instances::period(&resources.db, character, kind, *start).await?); }
    }
    // Assignment persists four ordinary slots and one bonus, or eight monthly slots.
    for (kind, start) in periods.keys() {
        let members: Vec<_> = rows.iter().filter(|row| &row.kind == kind && row.period_start == *start).collect();
        let slots: &[&str] = match kind.as_str() {
            "day" => &["ordinary:0", "ordinary:1", "ordinary:2", "ordinary:3", "bonus"],
            "month" => &["base:0", "base:1", "base:2", "base:3", "omega:0", "omega:1", "omega:2", "omega:3"],
            _ => return Err(Error::Invalid),
        };
        if members.len() != slots.len() || slots.iter().any(|slot| members.iter().filter(|row| row.slot_key == *slot).count() != 1) { return Err(Error::Unavailable); }
    }
    let ids: Vec<_> = rows.iter().map(|row| row.goal_id.clone()).collect();
    let mut assets = BTreeMap::<String, Vec<_>>::new();
    for reward in rewards::for_goals(&resources.db, character, &ids).await? { assets.entry(reward.goal_id.clone()).or_default().push(reward); }
    let mut goals = BTreeMap::new();
    for row in rows {
        let period = periods.get(&(row.kind.clone(), row.period_start)).ok_or(Error::Invalid)?.clone();
        let key = (row.namespace.clone(), row.version.clone());
        if !pins.contains_key(&key) { pins.insert(key.clone(), resources.policies.resolve(&key.0, &key.1).await?); }
        let policy = pins.get(&key).ok_or(Error::Unavailable)?.clone();
        let snapshot = frozen::validate(&row)?;
        let id = row.goal_id.clone();
        let meta = GoalMeta { baseline: row, snapshot, rewards: assets.remove(&id).unwrap_or_default(), policy, period };
        goals.insert(id, VirtualGoal::new(Arc::new(meta)));
    }
    let mut bonus_sources = BTreeMap::new();
    for period in periods.values().filter(|p| p.kind == "month") {
        bonus_sources.insert(period.period_start, instances::completed_sources(&resources.db, character, period.period_start, period.period_end).await?.into_iter().collect());
    }
    Ok(VirtualState { goals, bonus_sources, grade: subscriptions::by_user(&resources.db, user).await? })
}

/// Per-operation period cache, reusable across all facts for one owner.
#[derive(Default)]
pub(super) struct PeriodCache { days: BTreeMap<i64, assignment::EnsureOutcome> }

pub(super) async fn ensure_fact(resources: &Resources, user: i64, fact: &mut source::Fact, now: i64, cache: &mut PeriodCache, periods: &mut BTreeMap<(String, i64), PeriodRow>, pins: &mut HashMap<(String, String), PinnedPolicy>) -> Result<Vec<(i64, i64)>, Error> {
    let mut retained = Vec::new();
    let mut pairs = Vec::new();
    for slice in std::mem::take(&mut fact.slices) {
        let day = slice.occurred_at.div_euclid(infinity_core::consts::FILETIME_DAY);
        if !cache.days.contains_key(&day) {
            let notify = day == now.div_euclid(infinity_core::consts::FILETIME_DAY);
            let outcome = assignment::ensure_at(resources, user, fact.character, slice.occurred_at, notify).await?;
            cache.days.insert(day, outcome);
        }
        let outcome = cache.days.get(&day).ok_or(Error::Invalid)?;
        if outcome.day.is_some() && outcome.month.is_none() { return Err(Error::Unavailable); }
        if let (Some(day), Some(month)) = (&outcome.day, &outcome.month) {
            periods.insert((day.kind.clone(), day.period_start), day.clone());
            periods.insert((month.kind.clone(), month.period_start), month.clone());
            pins.extend(outcome.pins.clone());
            pairs.push((outcome.bounds.day_start, outcome.bounds.month_start));
            retained.push(slice);
        }
    }
    fact.slices = retained;
    Ok(pairs)
}

/// Capture this stage immediately before simulating the next canonical receipt.
pub(super) fn prepared(key: DeliveryKey, event: &EventEnvelope, owner: i64, state: &mut VirtualState, fact: Option<&source::Fact>, pairs: &[(i64, i64)], at: i64) -> Result<ConcretePreparedDelivery, Error> {
    let read = ReadSet::capture(owner, state)?;
    let change = match fact { Some(fact) => state::plan_fact(state, fact, pairs, at)?, None => Change::default() };
    Ok(ConcretePreparedDelivery { key, identity: Identity::new(event), change: Some(PreparedChange::new(read, change, state.grade.clone(), at)) })
}

/// Prepare a canonical receipt before entering the shared driver's transaction.
pub(crate) fn delivery(resources: ContentProgressResources, key: DeliveryKey) -> Pin<Box<dyn Future<Output = Result<PrepareOutcome, CallError>> + Send>> {
    Box::pin(async move {
        let event = match journal::delivery(&resources.db, &key).await? {
            DeliveryState::Applied => return Ok(PrepareOutcome::AlreadyApplied),
            DeliveryState::Missing => return Err(CallError::Internal(anyhow::anyhow!("daily delivery missing"))),
            DeliveryState::Pending(event) => event,
        };
        let identity = Identity::new(&event);
        let mut fact = source::map(&event, &resources.sde)?;
        let daily = Resources { db: resources.db, world: resources.world, sde: resources.sde, policies: resources.policies };
        if let Some(fact) = fact.as_mut() {
            if let Some(user) = persistence::user_for_character(&daily.db, fact.character).await? {
                let at = crate::client::filetime_now();
                let mut periods = BTreeMap::new(); let mut pins = HashMap::new();
                let pairs = ensure_fact(&daily, user, fact, at, &mut PeriodCache::default(), &mut periods, &mut pins).await.map_err(failure)?;
                if !pairs.is_empty() {
                    let mut state = load_state(&daily, user, fact.character, periods, pins, false).await.map_err(failure)?;
                    let plan = prepared(key, &event, fact.character, &mut state, Some(fact), &pairs, at).map_err(failure)?;
                    return Ok(PrepareOutcome::Ready(Arc::new(plan)));
                }
            }
        }
        Ok(PrepareOutcome::Ready(Arc::new(ConcretePreparedDelivery { key, identity, change: None })))
    })
}


#[cfg(test)] mod tests;
