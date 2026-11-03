//! GM skill mutations, using the same SDE ranks and race seed as character creation.
//! Mirrors skillsvc.py notifications and persists game.skills and game.skill_queue.
//! Skill queues for changed skills are removed; unrelated entries retain their order.
//! Completed training and GM grants share one skills-changed notification per batch.
//! Skill reductions reconcile the owner's online fitted ships after the batch.
use super::{completion::{self, AdvanceResult}, db_err, CallError};
use super::{formulas::sp_for_level, starting::starting_levels, training::skill_type, view::skill_info};
use crate::{client::{Client, filetime_now}, consts::CATEGORY_SKILL, dao::skills::{queue, training}, db::{executor::Executor, outbox::Outbox}, sde::Sde};
use crate::services::effects::online::{self, OnlineChanged};
use marshal::Value;
use std::collections::{BTreeMap, BTreeSet};


pub(crate) fn is_skill(sde: &Sde, type_id: i32) -> bool {
    sde.types.get(type_id).is_some_and(|t| t.category_id == Some(CATEGORY_SKILL))
}

pub(crate) fn all_levels(sde: &Sde, published: bool, level: i32) -> Vec<(i32, i32)> {
    sde.types.iter().filter(|t| t.category_id == Some(CATEGORY_SKILL) && (!published || t.published == Some(true))).map(|t| (t.type_id, level)).collect()
}


async fn clean_queue(tx: &(impl Executor + ?Sized), char_id: i64, types: &BTreeSet<i32>, now: i64, progress: &mut AdvanceResult) -> Result<(), CallError> {
    let rows = queue::list(tx, char_id).await.map_err(db_err)?;
    if rows.iter().any(|r| types.contains(&r.type_id)) {
        queue::clear(tx, char_id).await.map_err(db_err)?;
        for row in retain_queue(rows, types, now) {
            queue::insert(tx, char_id, &row).await.map_err(db_err)?;
        }
        progress.queue_changed = true;
    }
    Ok(())
}

fn retain_queue(rows: Vec<queue::QueueEntry>, types: &BTreeSet<i32>, now: i64) -> Vec<queue::QueueEntry> {
    let mut shift = 0i64;
    let mut kept = Vec::new();
    for mut row in rows {
        if types.contains(&row.type_id) {
            if let (Some(start), Some(end)) = (row.start_time, row.end_time) {
                shift += (end - start.max(now)).max(0);
            }
        } else {
            row.position = kept.len() as i32;
            row.start_time = row.start_time.map(|t| t - shift);
            row.end_time = row.end_time.map(|t| t - shift);
            kept.push(row);
        }
    }
    kept
}

pub(in crate::services) async fn set_levels<E: From<OnlineChanged> + Send>(tx: &(impl Executor + ?Sized), outbox: &mut Outbox<E>, sde: &std::sync::Arc<Sde>, char_id: i64, levels: &[(i32, i32)], now: i64) -> Result<(), CallError> {
    crate::dao::characters::selection::lock_character(tx, char_id).await.map_err(db_err)?;
    let progress = completion::advance_tx(tx, outbox, sde, char_id, now).await?;
    write_levels(tx, outbox, sde, char_id, levels, now, progress).await
}

async fn write_levels<E: From<OnlineChanged> + Send>(tx: &(impl Executor + ?Sized), outbox: &mut Outbox<E>, sde: &std::sync::Arc<Sde>, char_id: i64, levels: &[(i32, i32)], now: i64, mut progress: AdvanceResult) -> Result<(), CallError> {
    let plan: BTreeMap<_, _> = levels.iter().copied().collect();
    let types: BTreeSet<_> = plan.keys().copied().collect();
    clean_queue(tx, char_id, &types, now, &mut progress).await?;
    let before: BTreeMap<_, _> = training::current(tx, char_id).await.map_err(db_err)?.into_iter().map(|s| (s.type_id, (s.level, s.skill_points))).collect();
    for (type_id, level) in plan {
        let sp = sp_for_level(skill_type(sde, type_id).rank, level as i64);
        training::set_level(tx, char_id, type_id, level, sp).await.map_err(db_err)?;
        if before.get(&type_id).is_none_or(|&(old_level, old_sp)| old_level != level || old_sp != sp) {
            crate::dao::skills::history::append(tx, char_id, type_id, super::history::GM_GIVE, sp, now).await.map_err(db_err)?;
        }
    }
    progress.brain_changed = true;
    completion::finish_tx(tx, outbox, sde, char_id, now, &progress, &types.into_iter().collect::<Vec<_>>()).await
}

pub(in crate::services) async fn remove<E: From<OnlineChanged> + Send>(tx: &(impl Executor + ?Sized), outbox: &mut Outbox<E>, sde: &std::sync::Arc<Sde>, char_id: i64, type_ids: &[i32], now: i64) -> Result<(), CallError> {
    crate::dao::characters::selection::lock_character(tx, char_id).await.map_err(db_err)?;
    let types: BTreeSet<_> = type_ids.iter().copied().collect();
    let mut progress = completion::advance_tx(tx, outbox, sde, char_id, now).await?;
    clean_queue(tx, char_id, &types, now, &mut progress).await?;
    let sorted: Vec<_> = types.into_iter().collect();
    let removed = training::remove(tx, char_id, &sorted).await.map_err(db_err)?;
    let mut infos = Vec::new();
    for s in removed {
        crate::dao::skills::history::append(tx, char_id, s.type_id, super::history::REMOVED, s.skill_points, now).await.map_err(db_err)?;
        infos.push((Value::Int(s.type_id as i64), skill_info(sde, s.type_id, -1, -1, now)));
    }
    if !infos.is_empty() {
        outbox.notify_pilot(char_id, "OnServerSkillsRemoved", vec![Value::Dict(infos), Value::Int(now)]);
    }
    progress.brain_changed = true;
    completion::finish_tx(tx, outbox, sde, char_id, now, &progress, &[]).await
}

async fn grant_starters<E: From<OnlineChanged> + Send>(tx: &(impl Executor + ?Sized), outbox: &mut Outbox<E>, sde: &std::sync::Arc<Sde>, char_id: i64, now: i64) -> Result<(), CallError> {
    crate::dao::characters::selection::lock_character(tx, char_id).await.map_err(db_err)?;
    let progress = completion::advance_tx(tx, outbox, sde, char_id, now).await?;
    let race = training::race(tx, char_id).await.map_err(db_err)?.ok_or_else(|| CallError::Internal(anyhow::anyhow!("character disappeared")))?;
    let existing: BTreeSet<_> = training::current(tx, char_id).await.map_err(db_err)?.into_iter().map(|s| s.type_id).collect();
    let seed: Vec<_> = starting_levels(sde, race).filter(|(type_id, _)| !existing.contains(type_id)).collect();
    write_levels(tx, outbox, sde, char_id, &seed, now, progress).await
}

pub(crate) enum Change { Set(Vec<(i32, i32)>), Remove(Vec<i32>), RemoveAll, Starters }

pub(crate) async fn apply(c: &mut Client, char_id: i64, change: Change) -> Result<(), CallError> {
    let _lock = crate::services::market::lock().await;
    let db = c.db.clone();
    let sde = c.sde.clone();
    let now = filetime_now();
    db.transaction(|tx, outbox: &mut Outbox<OnlineChanged>| {
        let sde = sde.clone();
        let change = match &change {
            Change::Set(v) => Change::Set(v.clone()), Change::Remove(v) => Change::Remove(v.clone()),
            Change::RemoveAll => Change::RemoveAll, Change::Starters => Change::Starters,
        };
        Box::pin(async move {
            match change {
                Change::Set(levels) => set_levels(tx, outbox, &sde, char_id, &levels, now).await,
                Change::Remove(types) => remove(tx, outbox, &sde, char_id, &types, now).await,
                Change::RemoveAll => {
                    crate::dao::characters::selection::lock_character(tx, char_id).await.map_err(db_err)?;
                    let mut types: BTreeSet<_> = training::current(tx, char_id).await.map_err(db_err)?.into_iter().map(|s| s.type_id).collect();
                    types.extend(queue::list(tx, char_id).await.map_err(db_err)?.into_iter().map(|s| s.type_id));
                    let types: Vec<_> = types.into_iter().collect();
                    remove(tx, outbox, &sde, char_id, &types, now).await
                }
                Change::Starters => grant_starters(tx, outbox, &sde, char_id, now).await,
            }
        })
    }, |(), outbox| async move { let result = online::dispatch_changed((), outbox, c); crate::scheduler::wake(); result }).await
}

#[cfg(test)] mod tests;
