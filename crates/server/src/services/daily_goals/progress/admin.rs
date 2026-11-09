//! Prepare QA progress against persisted periods and retained policy.
//! The client goal messenger supplies the goal and increment; this module uses
//! the same virtual state and cascade planner as ordinary progress.
use std::collections::{BTreeMap, HashMap};
use crate::dao::daily_goals::{instances, periods};
use super::{prepare::{self, PreparedChange}, state::{self, GoalState, ReadSet}};
use super::super::{Error, Resources};

pub(crate) struct PreparedQa {
    pub change: PreparedChange,
    pub before: String,
    pub after: String,
}

fn state_json(state: &GoalState, rewards: &[state::RewardState]) -> serde_json::Value {
    serde_json::json!({"progress": state.progress.to_string(), "remainder_coefficient": state.remainder.coefficient,
        "remainder_exponent": state.remainder.exponent, "completed_at": state.completed_at,
        "earned_omega": state.earned_omega, "paid": state.paid, "has_unclaimed": state.has_unclaimed,
        "revision": state.revision, "rewards": rewards.iter().map(|r| serde_json::json!({"earned": r.earned, "revision": r.revision})).collect::<Vec<_>>()})
}

pub(crate) async fn prepare(resources: &Resources, user: i64, owner: i64, id: &str, amount: u64, at: i64) -> Result<PreparedQa, Error> {
    let goal = instances::get(&resources.db, owner, id).await?.ok_or(Error::Missing)?;
    let bounds = periods::bounds(&resources.db, goal.period_start).await?;
    let mut periods_by_key = BTreeMap::new();
    let month_start = if goal.kind == "day" { bounds.month_start } else if goal.kind == "month" { goal.period_start } else { return Err(Error::Invalid) };
    for (kind, start) in [("month", month_start), (goal.kind.as_str(), goal.period_start)] {
        let period = periods::get(&resources.db, owner, kind, start).await?.ok_or(Error::Unavailable)?;
        periods_by_key.insert((kind.to_owned(), start), period);
    }
    let mut state = prepare::load_state(resources, user, owner, periods_by_key, HashMap::new(), false).await?;
    let read = ReadSet::capture(owner, &state)?;
    let grade = state.grade.clone();
    let change = state::plan_qa(&mut state, id, amount, goal.period_start, month_start, at)?;
    let mut affected = std::collections::BTreeSet::new();
    affected.insert(id.to_owned());
    affected.extend(change.steps.iter().map(|step| step.meta.baseline.goal_id.clone()));
    let before = affected.iter().map(|key| {
        let g = read.goals.iter().find(|g| &g.meta.baseline.goal_id == key).ok_or(Error::Unavailable)?;
        Ok((key.clone(), state_json(&g.goal, &g.rewards)))
    }).collect::<Result<BTreeMap<_, _>, Error>>()?;
    let after = affected.iter().map(|key| {
        let g = state.goals.get(key).ok_or(Error::Unavailable)?;
        Ok((key.clone(), state_json(&g.goal, &g.rewards)))
    }).collect::<Result<BTreeMap<_, _>, Error>>()?;
    Ok(PreparedQa { change: PreparedChange::new(read, change, grade, at),
        before: serde_json::to_string(&before).map_err(anyhow::Error::from)?,
        after: serde_json::to_string(&after).map_err(anyhow::Error::from)? })
}

#[cfg(test)] mod tests;
