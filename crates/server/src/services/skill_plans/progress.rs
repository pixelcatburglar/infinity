//! Viewer-specific derived milestone progress and durable invalidation handling.
//! Reads trained skill levels, queue projections and startup SDE; expert grants are excluded.
//! Completion is reversible and no client completion notification is produced.
//! Cyclic targets produce invalid, incomplete snapshots without blocking delivery.
use std::collections::BTreeMap;
use crate::{dao::skills::{plan_milestones, plan_progress, plans, queue, training as skills}, db::{executor::Executor, outbox::Outbox, transaction::TxFuture}, lua::events::GameEvent, progress::{DurableRegistration, EventEnvelope}, sde::Sde, services::{db_err, skills::{formulas, training}, CallError}};

pub(crate) fn registration() -> DurableRegistration {
    DurableRegistration { consumer_key: "personal-skill-plan-milestones", kinds: &["SkillTrained", "SkillStateChanged"], mode: crate::progress::DeliveryMode::Direct(handler) }
}
fn handler<'a, 'tx>(tx: &'a tokio_postgres::Transaction<'tx>, _: &'a mut Outbox, sde: &'a Sde, event: &'a EventEnvelope) -> TxFuture<'a, (), CallError> {
    Box::pin(async move {
        let character = match &event.event { GameEvent::SkillTrained { char_id, .. } | GameEvent::SkillStateChanged { char_id } => *char_id, _ => return Err(CallError::Internal(anyhow::anyhow!("unexpected skill plan event"))) };
        reconcile_tx(tx, sde, character, None).await
    })
}
pub(crate) async fn reconcile_tx(tx: &(impl Executor + ?Sized), sde: &Sde, character: i64, parent: Option<&[u8]>) -> Result<(), CallError> {
    crate::dao::characters::selection::lock_character(tx, character).await.map_err(db_err)?;
    let milestones = if let Some(parent) = parent {
        plans::lock(tx, parent).await.map_err(db_err)?;
        plan_milestones::list(tx, parent).await.map_err(db_err)?
    } else {
        let initial = plan_milestones::for_viewer(tx, character).await.map_err(db_err)?;
        if initial.is_empty() { return Ok(()); }
        let mut parents: Vec<_> = initial.iter().map(|m| m.plan_id.clone()).collect();
        parents.sort(); parents.dedup();
        plans::lock_many(tx, &parents).await.map_err(db_err)?;
        plan_milestones::for_viewer(tx, character).await.map_err(db_err)?
    };
    if milestones.is_empty() { return Ok(()); }
    let trained = skills::current(tx, character).await.map_err(db_err)?;
    let queue = queue::list(tx, character).await.map_err(db_err)?;
    let now = crate::client::filetime_now();
    let trained: BTreeMap<_, _> = trained.into_iter().map(|s| (s.type_id, s)).collect();
    let mut queued = BTreeMap::<i32, i32>::new();
    for row in &queue { queued.entry(row.type_id).and_modify(|v| *v = (*v).max(row.to_level)).or_insert(row.to_level); }
    let snapshots = milestones.into_iter().map(|milestone| evaluate(sde, milestone, &trained, &queue, &queued, now)).collect::<Vec<_>>();
    plan_progress::upsert(tx, character, &snapshots, now).await.map_err(db_err)
}

pub(super) fn evaluate(sde: &Sde, milestone: plan_milestones::Milestone, trained: &BTreeMap<i32, skills::Skill>, queue: &[queue::QueueEntry], queued: &BTreeMap<i32, i32>, now: i64) -> plan_progress::Snapshot {
    let Some(required) = super::validation::required(sde, milestone.type_id, milestone.level) else {
        return plan_progress::Snapshot { milestone_id: milestone.milestone_id, required_sp: 0, trained_sp: 0, queued_sp: 0, completed: false, valid: false };
    };
    let (required_sp, trained_sp, queued_sp, completed) = evaluate_requirements(&required, |id| training::skill_type(sde, id).rank, trained, queue, queued, now);
    plan_progress::Snapshot { milestone_id: milestone.milestone_id, required_sp, trained_sp, queued_sp, completed, valid: true }
}

pub(super) fn evaluate_requirements(required: &BTreeMap<i32, i32>, rank_for: impl Fn(i32) -> f64, trained: &BTreeMap<i32, skills::Skill>, queue: &[queue::QueueEntry], queued: &BTreeMap<i32, i32>, now: i64) -> (i64, i64, i64, bool) {
    let mut total = 0i64; let mut have = 0i64; let mut projected = 0i64;
    let mut complete = !required.is_empty();
    for (&type_id, &level) in required {
        let rank = rank_for(type_id);
        let goal = formulas::sp_for_level(rank, i64::from(level));
        let skill = trained.get(&type_id);
        let mut points = skill.map_or(0, |s| s.skill_points);
        if let Some(row) = queue.first().filter(|row| row.type_id == type_id) {
            if let (Some(start), Some(end)) = (row.start_time, row.end_time) { points = points.max(training::points_at(row.start_sp, row.dest_sp, start, end, now)); }
        }
        let points = points.clamp(0, goal);
        let queue_level = queued.get(&type_id).copied().unwrap_or(0).min(level);
        // min(queue_points, points) makes this identical to subtracting the
        // target-clamped estimate and saturating at zero, since queue_points <= goal.
        let queue_points = formulas::sp_for_level(rank, i64::from(queue_level)).saturating_sub(points).max(0);
        total = total.saturating_add(goal); have = have.saturating_add(points); projected = projected.saturating_add(queue_points);
        complete &= skill.map_or(0, |s| s.level) >= level;
    }
    (total, have, projected, complete)
}
