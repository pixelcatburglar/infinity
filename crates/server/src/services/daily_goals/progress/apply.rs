//! Transactional validation and guarded application of prepared daily changes.
//! Client notices mirror `dailygoals/client/utils.py`; state is in daily-goal tables.
//! Lua, actors, payment and receipt ownership remain outside this module.
use crate::{dao::{accounts::subscriptions::{self, CloneGrade}, daily_goals::{instances::{self, ProgressUpdate}, periods, rewards}}, db::outbox::Outbox, progress::ApplyError, services::subscriptions::rules};
use tokio_postgres::Transaction;
use super::{super::{facts, wire}, state::{Change, ReadSet}};

fn invalid(error: super::super::Error) -> ApplyError {
    ApplyError::Call(crate::services::CallError::Internal(anyhow::anyhow!("daily progress output: {error:?}")))
}
/// Lock the complete canonical goal union before claiming delivery receipts.
/// Revalidate period membership and historical bonus sources against the snapshot.
pub(super) async fn prelock(tx: &Transaction<'_>, read: &ReadSet) -> Result<(), ApplyError> {
    let ids: Vec<String> = read.goals.iter().map(|g| g.meta.baseline.goal_id.clone()).collect();
    let locked = instances::lock_sorted(tx, read.owner, &ids).await.map_err(|error| match error {
        crate::db::Error::Progress(_) => ApplyError::Reprepare,
        other => other.into(),
    })?;
    for (actual, expected) in locked.iter().zip(&read.goals) {
        let baseline = &expected.meta.baseline;
        let state = &expected.goal;
        if actual.goal_id != baseline.goal_id || actual.character_id != baseline.character_id
            || actual.kind != baseline.kind || actual.period_start != baseline.period_start || actual.period_end != baseline.period_end
            || actual.slot_key != baseline.slot_key || actual.generation != baseline.generation
            || actual.namespace != baseline.namespace || actual.version != baseline.version || actual.category != baseline.category
            || actual.target != baseline.target || actual.voided_at != baseline.voided_at
            || actual.progress != state.progress || actual.remainder != state.remainder || actual.completed_at != state.completed_at
            || actual.earned_omega != state.earned_omega || actual.paid != state.paid
            || actual.has_unclaimed != state.has_unclaimed || actual.revision != state.revision { return Err(ApplyError::Reprepare); }
    }
    let mut seen = std::collections::BTreeSet::new();
    for goal in &read.goals {
        let expected = &goal.meta.period;
        if !seen.insert((expected.kind.as_str(), expected.period_start)) { continue; }
        if periods::get(tx, read.owner, &expected.kind, expected.period_start).await?.as_ref() != Some(expected) { return Err(ApplyError::Reprepare); }
        let actual = instances::period(tx, read.owner, &expected.kind, expected.period_start).await?;
        let expected_ids: Vec<_> = read.goals.iter().filter(|g| g.meta.baseline.kind == expected.kind && g.meta.baseline.period_start == expected.period_start).map(|g| &g.meta.baseline.goal_id).collect();
        if actual.len() != expected_ids.len() || actual.iter().zip(expected_ids).any(|(g, id)| &g.goal_id != id) { return Err(ApplyError::Reprepare); }
    }
    let actual_rewards = rewards::for_goals(tx, read.owner, &ids).await?;
    let expected_rewards = read.goals.iter().flat_map(|g| g.meta.rewards.iter().zip(&g.rewards));
    let mut count = 0;
    for (actual, (baseline, state)) in actual_rewards.iter().zip(expected_rewards) {
        let mut expected = baseline.clone();
        expected.earned = state.earned;
        expected.revision = state.revision;
        if actual != &expected { return Err(ApplyError::Reprepare); }
        count += 1;
    }
    if count != actual_rewards.len() || count != read.goals.iter().map(|g| g.rewards.len()).sum::<usize>() { return Err(ApplyError::Reprepare); }
    for (start, sources) in &read.bonus_sources {
        let end = read.goals.iter().find(|g| g.meta.period.kind == "month" && g.meta.period.period_start == *start).ok_or(ApplyError::Reprepare)?.meta.period.period_end;
        let actual = instances::completed_sources(tx, read.owner, *start, end).await?;
        if actual.into_iter().collect::<std::collections::BTreeSet<_>>() != *sources { return Err(ApplyError::Reprepare); }
    }
    Ok(())
}
/// Invoke only after all goal and delivery locks have been acquired.
pub(super) async fn apply(tx: &Transaction<'_>, outbox: &mut Outbox, owner: i64, captured: &CloneGrade, captured_at: i64, change: &Change) -> Result<(), ApplyError> {
    let current = subscriptions::lock(tx, captured.user_id).await?;
    let now = crate::client::filetime_now();
    if current.revision != captured.revision || current.grade != captured.grade || current.omega_until != captured.omega_until || current.grace_until != captured.grace_until || rules::effective_grade(&current, now) != rules::effective_grade(captured, captured_at) { return Err(ApplyError::Reprepare); }
    for step in &change.steps {
        let id = &step.meta.baseline.goal_id;
        let after = &step.after;
        let update = ProgressUpdate { progress: after.progress, remainder: after.remainder.clone(), completed_at: after.completed_at, earned_omega: after.earned_omega, paid: after.paid, has_unclaimed: after.has_unclaimed };
        if !instances::update_progress(tx, owner, id, step.before.revision, &update).await? { return Err(ApplyError::Reprepare); }
        for &(index, revision) in &step.earned {
            if !rewards::earn(tx, owner, id, index, revision).await? { return Err(ApplyError::Reprepare); }
        }
        outbox.notify_gateway(wire::progressed_notice(owner, id, after.progress).map_err(invalid)?);
        if step.before.completed_at.is_none() && after.completed_at.is_some() {
            let mut row = step.meta.baseline.clone();
            row.progress = after.progress; row.remainder = after.remainder.clone(); row.completed_at = after.completed_at;
            row.earned_omega = after.earned_omega; row.paid = after.paid; row.has_unclaimed = after.has_unclaimed; row.revision = after.revision;
            facts::completed(outbox, &row, &step.meta.snapshot, step.reason, step.origin, step.at).map_err(invalid)?;
            outbox.notify_gateway(wire::completed_notice(owner, id).map_err(invalid)?);
        }
    }
    Ok(())
}
