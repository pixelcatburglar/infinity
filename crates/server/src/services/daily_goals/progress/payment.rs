//! Bounded, outside-lock preparation of a paid completion and pending deliveries.
//! The daily-goal client pays against the current day; source facts retain their
//! occurrence periods and recorded content. Receipts remain in the progress driver.
use std::collections::{BTreeMap, BTreeSet, HashMap};
use crate::{dao::{daily_goals::{instances, periods}, progress::{self as journal, DeliveryState}}, progress::{ApplyError, DeliveryKey, EventEnvelope, PreparedDelivery}, services::daily_goals::{activation, Error, Resources}};
use tokio_postgres::Transaction;
use super::{apply, prepare::{self, ConcretePreparedDelivery, PreparedChange, PeriodCache, MAX_PAYMENT_AFFECTED_GOALS, MAX_PAYMENT_PENDING_DELIVERIES}, source::{self, Fact}, state::{self, ReadSet}};
/// Four ordinary slots and a bonus are assigned per daily period.
const DAILY_SLOTS: usize = 5;
/// Four base and four Omega slots are assigned per monthly period.
const MONTHLY_SLOTS: usize = 8;

pub(crate) enum BatchError { Reprepare, Domain(Error) }
impl From<Error> for BatchError { fn from(value: Error) -> Self { Self::Domain(value) } }
impl From<crate::db::Error> for BatchError { fn from(value: crate::db::Error) -> Self { Self::Domain(value.into()) } }
impl From<crate::services::CallError> for BatchError { fn from(value: crate::services::CallError) -> Self { Self::Domain(value.into()) } }

struct Pending { key: DeliveryKey, event: EventEnvelope, fact: Option<Fact>, pairs: Vec<(i64, i64)> }

/// A snapshot owns the complete initial lock set and successive virtual stages.
pub(crate) struct PreparedBatch {
    initial: ReadSet,
    keys: Vec<DeliveryKey>,
    plans: Vec<ConcretePreparedDelivery>,
    paid: Option<PreparedChange>,
    target: instances::GoalRow,
    bonus_progress: u64,
    bonus_id: String,
    omega_required: bool,
}
impl PreparedBatch {
    pub(crate) async fn prelock(&self, tx: &Transaction<'_>) -> Result<(), ApplyError> { apply::prelock(tx, &self.initial).await }
    pub(crate) fn keys(&self) -> &[DeliveryKey] { &self.keys }
    pub(crate) fn plans(&self) -> impl Iterator<Item = &dyn PreparedDelivery> { self.plans.iter().map(|p| p as &dyn PreparedDelivery) }
    pub(crate) fn paid(&self) -> Option<&PreparedChange> { self.paid.as_ref() }
    pub(crate) fn target(&self) -> &instances::GoalRow { &self.target }
    pub(crate) fn bonus_progress(&self) -> u64 { self.bonus_progress }
    pub(crate) fn bonus_id(&self) -> &str { &self.bonus_id }
    pub(crate) fn omega_required(&self) -> bool { self.omega_required }
}

/// Validate existing and eligible missing periods before creating any of them.
async fn preflight(resources: &Resources, owner: i64, bounds: &BTreeMap<(String, i64), i64>) -> Result<(), BatchError> {
    let days: Vec<_> = bounds.keys().filter(|(kind, _)| kind == "day").map(|(_, start)| *start).collect();
    let months: Vec<_> = bounds.keys().filter(|(kind, _)| kind == "month").map(|(_, start)| *start).collect();
    let rows = instances::bounded_period_union(&resources.db, owner, &days, &months, (MAX_PAYMENT_AFFECTED_GOALS + 1) as i64).await?;
    if rows.len() > MAX_PAYMENT_AFFECTED_GOALS { return Err(Error::Forbidden.into()); }
    let mut count = rows.len();
    let mut history = activation::load(resources).await?;
    for ((kind, start), end) in bounds {
        let period = periods::get(&resources.db, owner, kind, *start).await?;
        let members = rows.iter().filter(|r| r.kind == *kind && r.period_start == *start).count();
        if period.is_none() {
            if members != 0 { return Err(BatchError::Reprepare); }
            if activation::resolve(resources, &mut history, kind, *start, *end).await?.is_none() { return Err(Error::Unavailable.into()); }
            count += if kind == "day" { DAILY_SLOTS } else { MONTHLY_SLOTS };
            if count > MAX_PAYMENT_AFFECTED_GOALS { return Err(Error::Forbidden.into()); }
        } else if members != if kind == "day" { DAILY_SLOTS } else { MONTHLY_SLOTS } { return Err(BatchError::Reprepare); }
    }
    Ok(())
}

pub(crate) async fn prepare(resources: &Resources, user: i64, owner: i64, id: &str, at: i64) -> Result<PreparedBatch, BatchError> {
    let target = instances::get(&resources.db, owner, id).await?.ok_or(Error::Missing)?;
    let current = periods::bounds(&resources.db, at).await?;
    if target.kind != "day" || target.category != "ordinary" || target.period_start != current.day_start || at < target.period_start || at >= target.period_end || target.voided_at.is_some() { return Err(Error::Forbidden.into()); }
    let keys = journal::pending_for_character(&resources.db, super::CONSUMER_KEY, owner, (MAX_PAYMENT_PENDING_DELIVERIES + 1) as i64).await?;
    if keys.len() > MAX_PAYMENT_PENDING_DELIVERIES { return Err(Error::Forbidden.into()); }
    let mut pending = Vec::with_capacity(keys.len());
    let mut bounds = BTreeMap::new();
    bounds.insert(("day".to_owned(), current.day_start), current.day_end);
    bounds.insert(("month".to_owned(), current.month_start), current.month_end);
    let mut history = activation::load(resources).await?;
    let mut eligible_days = BTreeMap::new();
    for key in &keys {
        let event = match journal::delivery(&resources.db, key).await? { DeliveryState::Pending(event) => event, DeliveryState::Applied => return Err(BatchError::Reprepare), DeliveryState::Missing => return Err(Error::Unavailable.into()) };
        let mut fact = source::map(&event, &resources.sde)?;
        if let Some(fact) = &mut fact {
            if fact.character != owner { return Err(Error::Invalid.into()); }
            let mut retained = Vec::new();
            for slice in std::mem::take(&mut fact.slices) {
                let b = periods::bounds(&resources.db, slice.occurred_at).await?;
                let eligible = if let Some(eligible) = eligible_days.get(&b.day_start) { *eligible } else {
                    let eligible = periods::get(&resources.db, owner, "day", b.day_start).await?.is_some()
                        || activation::resolve(resources, &mut history, "day", b.day_start, b.day_end).await?.is_some();
                    eligible_days.insert(b.day_start, eligible);
                    eligible
                };
                if eligible {
                    bounds.insert(("day".to_owned(), b.day_start), b.day_end);
                    bounds.insert(("month".to_owned(), b.month_start), b.month_end);
                    retained.push(slice);
                }
            }
            fact.slices = retained;
        }
        pending.push(Pending { key: key.clone(), event, fact, pairs: Vec::new() });
    }
    preflight(resources, owner, &bounds).await?;
    let mut by_period = BTreeMap::new();
    for (kind, start) in [("day", current.day_start), ("month", current.month_start)] {
        let period = periods::get(&resources.db, owner, kind, start).await?.ok_or(Error::Unavailable)?;
        by_period.insert((kind.to_owned(), start), period);
    }
    let mut pins = HashMap::new();
    let mut cache = PeriodCache::default();
    for item in &mut pending {
        if let Some(fact) = &mut item.fact {
            item.pairs = prepare::ensure_fact(resources, user, fact, at, &mut cache, &mut by_period, &mut pins).await?;
        }
    }
    let mut state = prepare::load_state(resources, user, owner, by_period, pins, true).await?;
    let initial = ReadSet::capture(owner, &state)?;
    let mut plans = Vec::with_capacity(pending.len());
    for item in &pending { plans.push(prepare::prepared(item.key.clone(), &item.event, owner, &mut state, item.fact.as_ref(), &item.pairs, at)?); }
    let goal = state.goals.get(id).ok_or(Error::Missing)?;
    let omega_required = goal.meta.snapshot.template.omega_required;
    let mut target = goal.meta.baseline.clone();
    target.progress = goal.goal.progress; target.completed_at = goal.goal.completed_at; target.paid = goal.goal.paid;
    target.revision = goal.goal.revision; target.earned_omega = goal.goal.earned_omega; target.has_unclaimed = goal.goal.has_unclaimed; target.remainder = goal.goal.remainder.clone();
    let bonuses: Vec<_> = state.goals.values().filter(|g| g.meta.baseline.kind == "day" && g.meta.baseline.period_start == current.day_start && g.meta.baseline.category == "bonus").collect();
    if bonuses.len() != 1 { return Err(Error::Unavailable.into()); }
    let bonus_progress = bonuses[0].goal.progress;
    let bonus_id = bonuses[0].meta.baseline.goal_id.clone();
    let paid = if target.completed_at.is_none() {
        let read = ReadSet::capture(owner, &state)?;
        let grade = state.grade.clone();
        let change = state::plan_paid(&mut state, id, current.day_start, current.month_start, at)?;
        Some(PreparedChange::new(read, change, grade, at))
    } else { None };
    // Reject duplicate canonical keys rather than silently losing a receipt lock.
    if keys.iter().map(|k| &k.event_id).collect::<BTreeSet<_>>().len() != keys.len() { return Err(Error::Invalid.into()); }
    Ok(PreparedBatch { initial, keys, plans, paid, target, bonus_progress, bonus_id, omega_required })
}
