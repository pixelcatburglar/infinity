//! Durable, guarded application of the shared skill and augmentation timeline,
//! consumed by the skill handler and its characterskills/util.py client.
//! Persists game.skills, game.skill_queue, game.items, skill history and game.jobs;
//! one job per character uses the shared planner and reconciles fitted resources.
//! Legacy boosters without expiry have no scheduled deadline.
use super::{db_err, timeline, view::skill_info, CallError};
use crate::{client::{filetime_now, BRAIN_REFRESH}, consts::{IX_LOCATION_ID, LOCATION_JUNKYARD}, dao::{items::{brain, loading::{Item, ItemQuery}}, skills::{queue, training}}, db::{executor::Executor, outbox::Outbox, Db}, scheduler::{self, Job}, sde::Sde, services::effects::online::{self, OnlineChanged}, space::World};
use marshal::Value;
use std::{collections::{BTreeMap, BTreeSet}, future::Future, pin::Pin, sync::Arc};

pub(crate) const KIND: &str = "skills.complete";

#[derive(Default)]
pub(super) struct AdvanceResult {
    pub completed: Vec<(i32, i32)>,
    pub queue_changed: bool,
    pub brain_changed: bool,
    pub boosters_changed: bool,
}
impl AdvanceResult {
    pub(super) fn merge(&mut self, other: Self) {
        self.completed.extend(other.completed);
        self.queue_changed |= other.queue_changed;
        self.brain_changed |= other.brain_changed;
        self.boosters_changed |= other.boosters_changed;
    }
}

pub(super) async fn reschedule(tx: &(impl Executor + ?Sized), char_id: i64) -> Result<(), CallError> {
    let id = format!("{KIND}:{char_id}");
    let queue_due = queue::active(tx, char_id).await.map_err(db_err)?.and_then(|row| row.end_time);
    let booster_due = brain::earliest_expiry(tx, char_id).await.map_err(db_err)?;
    if let Some(due_at) = queue_due.into_iter().chain(booster_due).min() {
        scheduler::schedule(tx, &Job { id, kind: KIND.into(), due_at, payload: char_id.to_string() }).await.map_err(db_err)?;
    } else { scheduler::cancel(tx, &id).await.map_err(db_err)?; }
    Ok(())
}

pub(crate) async fn seed(db: &Db) -> Result<(), CallError> {
    let mut chars: BTreeSet<_> = queue::active_characters(db).await.map_err(db_err)?.into_iter().collect();
    chars.extend(brain::timed_characters(db).await.map_err(db_err)?);
    for char_id in chars {
        db.transaction(|tx, _outbox: &mut Outbox| Box::pin(async move {
            crate::dao::characters::selection::lock_character(tx, char_id).await.map_err(db_err)?;
            reschedule(tx, char_id).await
        }), |value, outbox| async move { let _ = outbox; Ok(value) }).await?;
    }
    scheduler::wake();
    Ok(())
}

/// Caller holds the market guard and character lock in the transaction.
/// The observations are checked before any writes; a stale nonempty plan is refused.
pub(super) async fn apply_plan_tx<E>(tx: &(impl Executor + ?Sized), outbox: &mut Outbox<E>, sde: &Arc<Sde>, plan: &timeline::AdvancePlan) -> Result<AdvanceResult, CallError> {
    let char_id = plan.char_id;
    let mut current = training::current(tx, char_id).await.map_err(db_err)?;
    current.sort_unstable_by_key(|row| row.type_id);
    if current != plan.initial_skills || queue::list(tx, char_id).await.map_err(db_err)? != plan.initial_queue || brain::list(tx, char_id).await.map_err(db_err)? != plan.initial_brain {
        return Err(CallError::Internal(anyhow::anyhow!("stale skill timeline plan for character {char_id}")));
    }
    let mut expiry_credits = BTreeMap::new();
    for event in &plan.events {
        if let timeline::Event::Expiry { at, .. } = event {
            let before = at.checked_sub(1).ok_or_else(|| CallError::Internal(anyhow::anyhow!("booster expiry time underflow")))?;
            let before = super::trading::available_credits(tx, sde, char_id, before).await?;
            let after = super::trading::available_credits(tx, sde, char_id, *at).await?;
            expiry_credits.insert(*at, (before, after));
        }
    }
    let mut writes = BTreeMap::new();
    for skill in &plan.skills {
        if current.binary_search_by_key(&skill.type_id, |old| old.type_id).map_or(true, |index| current[index] != *skill) { writes.insert(skill.type_id, (Some(skill.level), skill.skill_points)); }
    }
    training::write_skills(tx, char_id, &writes).await.map_err(db_err)?;
    if plan.queue_changed {
        queue::clear(tx, char_id).await.map_err(db_err)?;
        for row in &plan.queue { queue::insert(tx, char_id, row).await.map_err(db_err)?; }
    }
    let mut removed: Vec<_> = plan.events.iter().flat_map(|event| match event { timeline::Event::Expiry { items, .. } => items.as_slice(), _ => &[] }).collect();
    removed.sort_unstable_by_key(|item| item.item_id);
    let mut payloads = BTreeMap::new();
    for item in removed {
        let loaded = crate::services::items::load_items(tx, sde, ItemQuery::Id(item.item_id)).await.map_err(db_err)?.into_iter().next().ok_or_else(|| CallError::Internal(anyhow::anyhow!("missing expired item {}", item.item_id)))?;
        if !brain::remove(tx, char_id, item).await.map_err(db_err)? { return Err(CallError::Internal(anyhow::anyhow!("stale expired item {}", item.item_id))); }
        payloads.insert(item.item_id, Item { location_id: LOCATION_JUNKYARD, ..loaded });
    }
    let mut result = AdvanceResult { queue_changed: plan.queue_changed, brain_changed: plan.brain_changed, boosters_changed: plan.boosters_changed, ..Default::default() };
    for event in &plan.events {
        match event {
            timeline::Event::Completion(done) => {
                crate::dao::skills::history::append(tx, char_id, done.type_id, super::history::TRAINING_COMPLETE, done.points, done.end_time).await.map_err(db_err)?;
                result.completed.push((done.type_id, done.level));
                super::training::queue_training_events(outbox, char_id, &[(done.type_id, done.level)]);
            }
            timeline::Event::Expiry { at, items } => {
                for item in items {
                    let id = item.item_id;
                    let gone = payloads.remove(&id).ok_or_else(|| CallError::Internal(anyhow::anyhow!("missing removed item {id}")))?;
                    outbox.notify_pilot(char_id, "OnItemChange", vec![gone.row(), Value::Dict(vec![(Value::Int(IX_LOCATION_ID), Value::Int(char_id))]), Value::None]);
                }
                let (before, after) = expiry_credits[at];
                if before > after {
                    outbox.notify_pilot(char_id, "OnNonDiminishingInjectionsRemoved", vec![]);
                    if after > 0 { outbox.notify_pilot(char_id, "OnNonDiminishingInjectionsAdded", vec![Value::Int(after)]); }
                }
            }
        }
    }
    Ok(result)
}

pub(super) async fn advance_tx<E>(tx: &(impl Executor + ?Sized), outbox: &mut Outbox<E>, sde: &Arc<Sde>, char_id: i64, through: i64) -> Result<AdvanceResult, CallError> {
    let plan = timeline::plan_tx(tx, sde, char_id, through).await?;
    apply_plan_tx(tx, outbox, sde, &plan).await
}

pub(super) async fn finish_tx<E: From<OnlineChanged> + Send>(tx: &(impl Executor + ?Sized), outbox: &mut Outbox<E>, sde: &Arc<Sde>, char_id: i64, now: i64, progress: &AdvanceResult, additional: &[i32]) -> Result<(), CallError> {
    reschedule(tx, char_id).await?;
    online::reconcile_owner(tx, sde, outbox, char_id).await?;
    let types: BTreeSet<_> = progress.completed.iter().map(|(type_id, _)| *type_id).chain(additional.iter().copied()).collect();
    if !types.is_empty() {
        let infos: Vec<_> = training::current(tx, char_id).await.map_err(db_err)?.into_iter().filter(|skill| types.contains(&skill.type_id)).map(|skill| (Value::Int(skill.type_id as i64), skill_info(sde, skill.type_id, skill.level, skill.skill_points, now))).collect();
        if !infos.is_empty() { outbox.notify_pilot(char_id, "OnServerSkillsChanged", vec![Value::Dict(infos), Value::None, Value::Int(now)]); }
    }
    if !types.is_empty() || progress.brain_changed { outbox.notify_pilot(char_id, BRAIN_REFRESH, vec![]); }
    if progress.queue_changed { outbox.notify_pilot(char_id, "OnNewSkillQueueSaved", vec![super::skill_handler::queue_value(&queue::list(tx, char_id).await.map_err(db_err)?)]); }
    if progress.boosters_changed { outbox.notify_pilot(char_id, "OnServerBoostersChanged", vec![]); }
    Ok(())
}

pub(crate) fn handle(db: Db, world: Arc<World>, job: Job) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send>> {
    Box::pin(async move {
        let char_id: i64 = serde_json::from_str(&job.payload)?;
        let now = filetime_now();
        let _market = crate::services::market::lock().await;
        let result = db.transaction(|tx, outbox: &mut Outbox<OnlineChanged>| {
            let sde = world.sde().clone();
            Box::pin(async move {
                crate::dao::characters::selection::lock_character(tx, char_id).await.map_err(db_err)?;
                let progress = advance_tx(tx, outbox, &sde, char_id, now).await?;
                finish_tx(tx, outbox, &sde, char_id, now, &progress, &[]).await
            })
        }, |value, outbox| {
            let world = &world;
            async move { let result = online::dispatch_changed_world(value, outbox, world); scheduler::wake(); result }
        }).await;
        result.map_err(|error| match error { CallError::Internal(error) => error, CallError::User { msg, .. } => anyhow::anyhow!("skill completion refused: {msg}") })
    })
}

#[cfg(test)] mod tests;
