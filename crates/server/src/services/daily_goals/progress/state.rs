//! Pure virtual daily-goal mutation over frozen rows and retained policy.
//! Client contribution methods are described by `dailygoals/client/utils.py`.
//! Inputs are owner-bound persisted rows; no database, actors or settlement are used.
use std::{collections::{BTreeMap, BTreeSet}, sync::Arc};
use crate::{dao::{accounts::subscriptions::CloneGrade, daily_goals::{instances::{CompletedSource, GoalRow}, periods::PeriodRow, rewards::RewardRow}}, lua::{content::{PinnedPolicy, daily_goals::{model::{Category, Period}, plans::{self, Candidate, CascadeInput, FactInput, MatchInput}}}, events::{combat::{BinaryAmount, amount::accumulate_capped}, goals::CompletionReason, resources::FactOrigin}}, services::subscriptions::rules};
use super::{super::{Error, GoalSnapshot}, source::{Fact, Increment}};

pub(super) struct GoalMeta {
    pub baseline: GoalRow,
    pub snapshot: GoalSnapshot,
    pub rewards: Vec<RewardRow>,
    pub policy: PinnedPolicy,
    pub period: PeriodRow,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct GoalState {
    pub progress: u64, pub remainder: BinaryAmount, pub completed_at: Option<i64>,
    pub earned_omega: bool, pub paid: bool, pub has_unclaimed: bool, pub revision: i64,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct RewardState { pub earned: bool, pub revision: i64 }
pub(super) struct VirtualGoal { pub meta: Arc<GoalMeta>, pub goal: GoalState, pub rewards: Vec<RewardState> }
pub(super) struct VirtualState {
    pub goals: BTreeMap<String, VirtualGoal>,
    pub bonus_sources: BTreeMap<i64, BTreeSet<CompletedSource>>,
    pub grade: CloneGrade,
}
pub(super) struct GoalChange {
    pub meta: Arc<GoalMeta>, pub before: GoalState, pub after: GoalState,
    pub earned: Vec<(i32, i64)>, pub reason: CompletionReason, pub origin: FactOrigin, pub at: i64,
}
#[derive(Default)]
pub(super) struct Change { pub steps: Vec<GoalChange> }
pub(super) struct ReadGoal { pub meta: Arc<GoalMeta>, pub goal: GoalState, pub rewards: Vec<RewardState> }
pub(super) struct ReadSet {
    pub owner: i64, pub goals: Vec<ReadGoal>,
    pub bonus_sources: BTreeMap<i64, BTreeSet<CompletedSource>>,
}
impl ReadSet {
    /// Capture the state immediately before an operation, sharing frozen metadata.
    pub(super) fn capture(owner: i64, state: &VirtualState) -> Result<Self, Error> {
        let mut goals = Vec::with_capacity(state.goals.len());
        for (id, goal) in &state.goals {
            if id != &goal.meta.baseline.goal_id || goal.meta.baseline.character_id != owner || goal.rewards.len() != goal.meta.rewards.len() { return Err(Error::Invalid); }
            goals.push(ReadGoal { meta: Arc::clone(&goal.meta), goal: goal.goal.clone(), rewards: goal.rewards.clone() });
        }
        for month in state.bonus_sources.keys() {
            if !goals.iter().any(|g| g.meta.period.kind == "month" && g.meta.period.period_start == *month) { return Err(Error::Invalid); }
        }
        Ok(Self { owner, goals, bonus_sources: state.bonus_sources.clone() })
    }
}
impl VirtualGoal {
    pub(super) fn new(meta: Arc<GoalMeta>) -> Self {
        let g = &meta.baseline;
        let goal = GoalState { progress: g.progress, remainder: g.remainder.clone(), completed_at: g.completed_at, earned_omega: g.earned_omega, paid: g.paid, has_unclaimed: g.has_unclaimed, revision: g.revision };
        let rewards = meta.rewards.iter().map(|r| RewardState { earned: r.earned, revision: r.revision }).collect();
        Self { meta, goal, rewards }
    }
}
fn zero() -> BinaryAmount { BinaryAmount { coefficient: "0".into(), exponent: 0 } }
fn mutate(goal: &mut VirtualGoal, progress: u64, remainder: BinaryAmount, reason: CompletionReason, origin: FactOrigin, at: i64, omega: bool, changes: &mut Change) -> Result<(), Error> {
    if goal.goal.completed_at.is_some() || goal.meta.baseline.voided_at.is_some() { return Ok(()); }
    let target = goal.meta.baseline.target;
    let progress = progress.max(goal.goal.progress).min(target);
    if progress == goal.goal.progress && remainder == goal.goal.remainder { return Ok(()); }
    let before = goal.goal.clone();
    let crossing = progress == target;
    goal.goal.progress = progress;
    goal.goal.remainder = if crossing { zero() } else { remainder };
    let mut earned = Vec::new();
    if crossing {
        goal.goal.completed_at = Some(at);
        goal.goal.earned_omega = omega;
        goal.goal.paid = reason == CompletionReason::Paid;
        for (row, state) in goal.meta.rewards.iter().zip(&mut goal.rewards) {
            if !state.earned {
                if row.claimed_at.is_some() { return Err(Error::Invalid); }
                earned.push((row.reward_index, state.revision));
                state.revision = state.revision.checked_add(1).ok_or(Error::Invalid)?;
                state.earned = true;
            }
        }
        goal.goal.has_unclaimed = goal.meta.rewards.iter().any(|r| r.claimed_at.is_none());
    }
    goal.goal.revision = goal.goal.revision.checked_add(1).ok_or(Error::Invalid)?;
    changes.steps.push(GoalChange { meta: Arc::clone(&goal.meta), before, after: goal.goal.clone(), earned, reason, origin, at });
    Ok(())
}
fn capped(current: u64, target: u64, remainder: &BinaryAmount, amount: &Increment) -> Result<(u64, BinaryAmount), Error> {
    let remaining = target.checked_sub(current).ok_or(Error::Invalid)?;
    let (delta, fraction) = match amount {
        Increment::Units(v) => ((*v).min(remaining), remainder.clone()),
        Increment::Hp(v) => accumulate_capped(remainder, v, remaining)?,
    };
    Ok((current + delta, if delta == remaining { zero() } else { fraction }))
}
fn increment(goal: &mut VirtualGoal, amount: &Increment, reason: CompletionReason, origin: FactOrigin, at: i64, omega: bool, changes: &mut Change) -> Result<(), Error> {
    if goal.goal.completed_at.is_some() || goal.meta.baseline.voided_at.is_some() { return Ok(()); }
    let (progress, remainder) = capped(goal.goal.progress, goal.meta.baseline.target, &goal.goal.remainder, amount)?;
    mutate(goal, progress, remainder, reason, origin, at, omega, changes)
}
/// Match each authoritative source against all targets sharing its retained policy.
fn qualified(
    targets: &[&VirtualGoal], sources: &[CompletedSource], category: Category, period: Period,
) -> Result<BTreeMap<String, BTreeSet<CompletedSource>>, Error> {
    let mut groups: BTreeMap<(&str, &str), Vec<&VirtualGoal>> = BTreeMap::new();
    for goal in targets {
        groups.entry((&goal.meta.baseline.namespace, &goal.meta.baseline.version)).or_default().push(goal);
    }
    let mut result = BTreeMap::<String, BTreeSet<CompletedSource>>::new();
    for group in groups.values() {
        let candidates: Vec<_> = group.iter().map(|g| Candidate { key: g.meta.baseline.goal_id.clone(), template: g.meta.snapshot.template.key.clone() }).collect();
        for source in sources {
            let fact = FactInput { method: "CompleteGoal".into(), goal: Some(source.goal_id.clone()), category: Some(category.clone()), period: Some(period.clone()), ..Default::default() };
            for id in plans::matches(&group[0].meta.policy, &MatchInput { fact, candidates: candidates.clone() }, None)?.keys {
                result.entry(id).or_default().insert(source.clone());
            }
        }
    }
    Ok(result)
}
fn cascade(state: &mut VirtualState, day: i64, month: i64, at: i64, origin: FactOrigin, changes: &mut Change) -> Result<(), Error> {
    let ordinary: Vec<_> = state.goals.values().filter(|g| g.meta.baseline.kind == "day" && g.meta.baseline.period_start == day && g.meta.baseline.category == "ordinary" && g.goal.completed_at.is_some() && g.meta.baseline.voided_at.is_none()).map(|g| CompletedSource { goal_id: g.meta.baseline.goal_id.clone(), period_start: day }).collect();
    let targets: Vec<_> = state.goals.values().filter(|g| g.meta.baseline.kind == "day" && g.meta.baseline.period_start == day && g.meta.baseline.category == "bonus" && g.goal.completed_at.is_none() && g.meta.baseline.voided_at.is_none()).collect();
    let matched = qualified(&targets, &ordinary, Category::Ordinary, Period::Day)?;
    let ids: Vec<_> = targets.iter().map(|g| g.meta.baseline.goal_id.clone()).collect();
    let omega = rules::effective_grade(&state.grade, at) == rules::OMEGA;
    for id in ids {
        let g = state.goals.get_mut(&id).ok_or(Error::Invalid)?;
        let ordinary_ids = matched.get(&id).into_iter().flatten().map(|s| s.goal_id.clone()).collect();
        let count = plans::cascade(&g.meta.policy, &CascadeInput { ordinary_ids, bonus_dates: Vec::new() }, None)?.bonus;
        mutate(g, count, zero(), CompletionReason::Derived, origin, at, omega, changes)?;
    }
    for g in state.goals.values().filter(|g| g.meta.baseline.kind == "day" && g.meta.baseline.period_start == day && g.meta.baseline.category == "bonus" && g.goal.completed_at.is_some() && g.meta.baseline.voided_at.is_none()) {
        state.bonus_sources.entry(month).or_default().insert(CompletedSource { goal_id: g.meta.baseline.goal_id.clone(), period_start: day });
    }
    let sources: Vec<_> = state.bonus_sources.get(&month).into_iter().flatten().cloned().collect();
    let targets: Vec<_> = state.goals.values().filter(|g| g.meta.baseline.kind == "month" && g.meta.baseline.period_start == month && g.meta.baseline.category == "month" && g.goal.completed_at.is_none() && g.meta.baseline.voided_at.is_none()).collect();
    let matched = qualified(&targets, &sources, Category::Bonus, Period::Month)?;
    let ids: Vec<_> = targets.iter().map(|g| g.meta.baseline.goal_id.clone()).collect();
    for id in ids {
        let g = state.goals.get_mut(&id).ok_or(Error::Invalid)?;
        let dates: BTreeSet<_> = matched.get(&id).into_iter().flatten().map(|s| s.period_start).collect();
        let input = CascadeInput { ordinary_ids: Vec::new(), bonus_dates: dates.iter().map(i64::to_string).collect() };
        let count = plans::cascade(&g.meta.policy, &input, None)?.month;
        mutate(g, count.min(g.meta.baseline.target), zero(), CompletionReason::Derived, origin, at, omega, changes)?;
    }
    Ok(())
}
/// Monthly slots are the shared serialization boundary for every daily completion.
fn require_month(state: &VirtualState, day: i64, month: i64) -> Result<(), Error> {
    const SLOTS: [&str; 8] = ["base:0", "base:1", "base:2", "base:3", "omega:0", "omega:1", "omega:2", "omega:3"];
    let rows: Vec<_> = state.goals.values().filter(|g| g.meta.baseline.kind == "month" && g.meta.baseline.period_start == month).collect();
    if rows.len() != SLOTS.len() || !state.bonus_sources.contains_key(&month)
        || SLOTS.iter().any(|slot| rows.iter().filter(|g| g.meta.baseline.slot_key == *slot).count() != 1)
        || rows.iter().any(|g| g.meta.baseline.category != "month" || day < g.meta.baseline.period_start || day >= g.meta.baseline.period_end
            || g.meta.period.kind != "month" || g.meta.period.period_start != month) { return Err(Error::Invalid); }
    Ok(())
}
/// Each entry identifies the occurrence day/month of one canonical source slice.
pub(super) fn plan_fact(state: &mut VirtualState, fact: &Fact, periods: &[(i64, i64)], at: i64) -> Result<Change, Error> {
    if periods.len() != fact.slices.len() { return Err(Error::Invalid); }
    for &(day, month) in periods { require_month(state, day, month)?; }
    let mut change = Change::default();
    let omega = rules::effective_grade(&state.grade, at) == rules::OMEGA;
    let days: BTreeSet<i64> = periods.iter().map(|(day, _)| *day).collect();
    let mut groups: BTreeMap<(i64, &str, &str), Vec<&VirtualGoal>> = BTreeMap::new();
    for goal in state.goals.values() {
        let row = &goal.meta.baseline;
        if row.character_id == fact.character && row.kind == "day" && days.contains(&row.period_start)
            && row.category == "ordinary" && goal.goal.completed_at.is_none() && row.voided_at.is_none() {
            groups.entry((row.period_start, &row.namespace, &row.version)).or_default().push(goal);
        }
    }
    let mut matched = BTreeSet::new();
    for goals in groups.values() {
        let candidates = goals.iter().map(|g| Candidate { key: g.meta.baseline.goal_id.clone(), template: g.meta.snapshot.template.key.clone() }).collect();
        let input = MatchInput { fact: fact.input.clone(), candidates };
        matched.extend(plans::matches(&goals[0].meta.policy, &input, None)?.keys);
    }
    for (slice, &(day, month)) in fact.slices.iter().zip(periods) {
        let first = change.steps.len();
        let ids: Vec<String> = state.goals.iter().filter(|(_, g)| g.meta.baseline.character_id == fact.character && g.meta.baseline.kind == "day" && g.meta.baseline.period_start == day && g.meta.baseline.category == "ordinary" && g.goal.completed_at.is_none() && g.meta.baseline.voided_at.is_none()).map(|(id, _)| id.clone()).collect();
        for id in ids {
            let g = state.goals.get_mut(&id).ok_or(Error::Invalid)?;
            if matched.contains(&id) { increment(g, &slice.amount, CompletionReason::Natural, FactOrigin::Natural, at, omega, &mut change)?; }
        }
        if change.steps[first..].iter().any(|step| step.after.completed_at.is_some()) {
            cascade(state, day, month, at, FactOrigin::Natural, &mut change)?;
        }
    }
    Ok(change)
}
/// The caller supplies the target's real day and month boundaries.
pub(super) fn plan_paid(state: &mut VirtualState, id: &str, day: i64, month: i64, at: i64) -> Result<Change, Error> {
    let mut change = Change::default();
    let omega = rules::effective_grade(&state.grade, at) == rules::OMEGA;
    let target = state.goals.get(id).ok_or(Error::Missing)?;
    if target.meta.baseline.kind == "day" {
        if target.meta.baseline.period_start != day { return Err(Error::Invalid); }
        require_month(state, day, month)?;
    }
    let g = state.goals.get_mut(id).ok_or(Error::Missing)?;
    let target = g.meta.baseline.target;
    mutate(g, target, zero(), CompletionReason::Paid, FactOrigin::Natural, at, omega, &mut change)?;
    if change.steps.iter().any(|step| step.after.completed_at.is_some() && step.meta.baseline.kind == "day") {
        cascade(state, day, month, at, FactOrigin::Natural, &mut change)?;
    }
    Ok(change)
}
pub(super) fn plan_qa(state: &mut VirtualState, id: &str, amount: u64, day: i64, month: i64, at: i64) -> Result<Change, Error> {
    let mut change = Change::default();
    let omega = rules::effective_grade(&state.grade, at) == rules::OMEGA;
    let target = state.goals.get(id).ok_or(Error::Missing)?;
    if target.meta.baseline.kind == "day" {
        if target.meta.baseline.period_start != day { return Err(Error::Invalid); }
        require_month(state, day, month)?;
    }
    let g = state.goals.get_mut(id).ok_or(Error::Missing)?;
    increment(g, &Increment::Units(amount), CompletionReason::Qa, FactOrigin::Qa, at, omega, &mut change)?;
    if change.steps.iter().any(|step| step.after.completed_at.is_some() && step.meta.baseline.kind == "day") {
        cascade(state, day, month, at, FactOrigin::Qa, &mut change)?;
    }
    Ok(change)
}

#[cfg(test)] mod tests;
