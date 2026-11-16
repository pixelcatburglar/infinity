//! GM skill mutations, using the same SDE ranks and race seed as character creation.
//! Mirrors skillsvc.py notifications and persists game.skills and game.skill_queue.
//! Skill queues for changed skills are removed; unrelated entries retain their order.
//! A queue head with a start time is active even when a zero rate left its end
//! open; removing the blocking entry banks only the finite head that survives
//! and retimes the rest from now, preserving that active intent.
//! The starter reset instead replaces the trained set with the race seed,
//! clears the whole queue and zeroes free skill points in one transaction;
//! the old queue head is discarded without banking. Expert systems, implants,
//! boosters, attributes and remap entitlements are preserved.
//! Completed training and GM grants share one skills-changed notification per batch.
//! Skill reductions reconcile the owner's online fitted ships after the batch.
use super::{completion::{self, AdvanceResult}, db_err, CallError};
use super::{formulas::sp_for_level, starting::starting_levels, training::skill_type};
use infinity_core::{consts::CATEGORY_SKILL, messages};
use crate::{client::{Client, filetime_now}, dao::skills::{queue, training}, db::{executor::Executor, outbox::Outbox}, sde::Sde};
use crate::services::effects::online::{self, OnlineChanged};
use crate::services::notify_error;
use marshal::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;


pub(crate) fn is_skill(sde: &Sde, type_id: i32) -> bool {
    sde.types.get(type_id).is_some_and(|t| t.category_id == Some(CATEGORY_SKILL))
}

pub(crate) fn all_levels(sde: &Sde, published: bool, level: i32) -> Vec<(i32, i32)> {
    sde.types.iter().filter(|t| t.category_id == Some(CATEGORY_SKILL) && (!published || t.published == Some(true))).map(|t| (t.type_id, level)).collect()
}


async fn clean_queue(tx: &(impl Executor + ?Sized), sde: &Arc<Sde>, char_id: i64, types: &BTreeSet<i32>, now: i64, progress: &mut AdvanceResult) -> Result<(), CallError> {
    let mut rows = queue::list(tx, char_id).await.map_err(db_err)?;
    if !rows.iter().any(|r| types.contains(&r.type_id)) { return Ok(()); }
    let active = rows.first().is_some_and(|row| row.start_time.is_some());
    let removed_blocker = rows.iter().any(|r| r.start_time.is_some() && r.end_time.is_none() && types.contains(&r.type_id));
    if active && removed_blocker {
        rows.retain(|r| !types.contains(&r.type_id));
        if !rows.is_empty() {
            // The deleted entry ended the time cursor; re-derive the kept
            // queue's times from now. Only a surviving finite head banks its
            // old interval.
            if let Some(head) = rows.first_mut() {
                if let (Some(start), Some(end)) = (head.start_time, head.end_time) {
                    let points = super::training::points_at(head.start_sp, head.dest_sp, start, end, now);
                    training::write_skills(tx, char_id, &super::training::plan_skills(&[], Some((head.type_id, points)))).await.map_err(db_err)?;
                    let banked = training::current(tx, char_id).await.map_err(db_err)?.into_iter().find(|skill| skill.type_id == head.type_id).map_or(points, |skill| skill.skill_points.max(points));
                    head.start_sp = banked.min(head.dest_sp);
                }
            }
            for (position, row) in rows.iter_mut().enumerate() { row.position = position as i32; }
            let attrs = super::attributes::character_attributes_at(tx, sde, char_id, now).await?;
            super::training::schedule_queue(sde, &mut rows, &attrs, Some(now))?;
        }
        queue::clear(tx, char_id).await.map_err(db_err)?;
        for row in &rows { queue::insert(tx, char_id, row).await.map_err(db_err)?; }
    } else {
        let kept = retain_queue(rows, types, now);
        queue::clear(tx, char_id).await.map_err(db_err)?;
        for row in &kept { queue::insert(tx, char_id, row).await.map_err(db_err)?; }
    }
    progress.queue_changed = true;
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

pub(in crate::services) async fn set_levels<E: From<OnlineChanged> + Send>(tx: &(impl Executor + ?Sized), outbox: &mut Outbox<E>, sde: &std::sync::Arc<Sde>, fences: &online::OnlineFences, char_id: i64, levels: &[(i32, i32)], now: i64) -> Result<(), CallError> {
    crate::dao::characters::selection::lock_character(tx, char_id).await.map_err(db_err)?;
    let progress = completion::advance_tx(tx, outbox, sde, char_id, now).await?;
    write_levels(tx, outbox, sde, fences, char_id, levels, now, progress).await
}

async fn write_levels<E: From<OnlineChanged> + Send>(tx: &(impl Executor + ?Sized), outbox: &mut Outbox<E>, sde: &std::sync::Arc<Sde>, fences: &online::OnlineFences, char_id: i64, levels: &[(i32, i32)], now: i64, mut progress: AdvanceResult) -> Result<(), CallError> {
    let plan: BTreeMap<_, _> = levels.iter().copied().collect();
    let types: BTreeSet<_> = plan.keys().copied().collect();
    clean_queue(tx, sde, char_id, &types, now, &mut progress).await?;
    let before: BTreeMap<_, _> = training::current(tx, char_id).await.map_err(db_err)?.into_iter().map(|s| (s.type_id, (s.level, s.skill_points))).collect();
    let mut changed = false;
    for (type_id, level) in plan {
        let sp = sp_for_level(skill_type(sde, type_id).rank, level as i64);
        training::set_level(tx, char_id, type_id, level, sp).await.map_err(db_err)?;
        if before.get(&type_id).is_none_or(|&(old_level, old_sp)| old_level != level || old_sp != sp) {
            crate::dao::skills::history::append(tx, char_id, type_id, super::history::GM_GIVE, sp, now).await.map_err(db_err)?;
            changed = true;
        }
    }
    if changed { outbox.event(crate::lua::events::GameEvent::SkillStateChanged { char_id }); }
    progress.brain_changed = true;
    completion::finish_tx(tx, outbox, sde, fences, char_id, now, &progress, &types.into_iter().collect::<Vec<_>>()).await
}

pub(in crate::services) async fn remove<E: From<OnlineChanged> + Send>(tx: &(impl Executor + ?Sized), outbox: &mut Outbox<E>, sde: &std::sync::Arc<Sde>, fences: &online::OnlineFences, char_id: i64, type_ids: &[i32], now: i64) -> Result<(), CallError> {
    crate::dao::characters::selection::lock_character(tx, char_id).await.map_err(db_err)?;
    let types: BTreeSet<_> = type_ids.iter().copied().collect();
    let mut progress = completion::advance_tx(tx, outbox, sde, char_id, now).await?;
    clean_queue(tx, sde, char_id, &types, now, &mut progress).await?;
    let sorted: Vec<_> = types.into_iter().collect();
    let removed = training::remove(tx, char_id, &sorted).await.map_err(db_err)?;
    let mut changed = Vec::new();
    for s in removed {
        crate::dao::skills::history::append(tx, char_id, s.type_id, super::history::REMOVED, s.skill_points, now).await.map_err(db_err)?;
        changed.push(s.type_id);
    }
    if !changed.is_empty() { outbox.event(crate::lua::events::GameEvent::SkillStateChanged { char_id }); }
    progress.brain_changed = true;
    completion::finish_tx(tx, outbox, sde, fences, char_id, now, &progress, &changed).await
}

async fn grant_starters<E: From<OnlineChanged> + Send>(tx: &(impl Executor + ?Sized), outbox: &mut Outbox<E>, sde: &std::sync::Arc<Sde>, fences: &online::OnlineFences, char_id: i64, now: i64) -> Result<(), CallError> {
    crate::dao::characters::selection::lock_character(tx, char_id).await.map_err(db_err)?;
    let progress = completion::advance_tx(tx, outbox, sde, char_id, now).await?;
    let race = training::race(tx, char_id).await.map_err(db_err)?.ok_or_else(|| CallError::Internal(anyhow::anyhow!("character disappeared")))?;
    let existing: BTreeSet<_> = training::current(tx, char_id).await.map_err(db_err)?.into_iter().map(|s| s.type_id).collect();
    let seed: Vec<_> = starting_levels(sde, race).filter(|(type_id, _)| !existing.contains(type_id)).collect();
    write_levels(tx, outbox, sde, fences, char_id, &seed, now, progress).await
}

/// Restore the race's starter set: delete nonstarters, reseed changed starters,
/// and discard the queue and free points without banking the old head.
async fn reset_to_starters<E: From<OnlineChanged> + Send>(tx: &(impl Executor + ?Sized), outbox: &mut Outbox<E>, sde: &std::sync::Arc<Sde>, fences: &online::OnlineFences, char_id: i64, now: i64) -> Result<(), CallError> {
    crate::dao::characters::selection::lock_character(tx, char_id).await.map_err(db_err)?;
    // race() also proves the target exists and is not deleted.
    let race = training::race(tx, char_id).await.map_err(db_err)?
        .ok_or_else(|| CallError::User { msg: messages::MSG_CHARACTER_NOT_FOUND.into(), dict: vec![] })?;
    // Creation inserts the seed in order and ignores conflicts: the first row
    // fixes each type's level, which is the target this reset restores.
    let mut seed = BTreeMap::new();
    for (type_id, level) in starting_levels(sde, race) { seed.entry(type_id).or_insert(level); }
    if seed.is_empty() { return Err(CallError::Internal(anyhow::anyhow!("starter skill template is empty for race {race}"))); }
    let before: BTreeMap<_, _> = training::current(tx, char_id).await.map_err(db_err)?.into_iter().map(|s| (s.type_id, (s.level, s.skill_points))).collect();
    let old_queue = queue::list(tx, char_id).await.map_err(db_err)?;
    let free = crate::dao::characters::attributes::free_points(tx, char_id).await.map_err(db_err)?;

    let mut changed = BTreeSet::new();
    let removals: Vec<_> = before.keys().filter(|type_id| !seed.contains_key(type_id)).copied().collect();
    let mut removed = training::remove(tx, char_id, &removals).await.map_err(db_err)?;
    removed.sort_unstable_by_key(|skill| skill.type_id);
    for skill in &removed {
        crate::dao::skills::history::append(tx, char_id, skill.type_id, super::history::REMOVED, skill.skill_points, now).await.map_err(db_err)?;
        changed.insert(skill.type_id);
    }
    for (type_id, level) in seed {
        let sp = sp_for_level(skill_type(sde, type_id).rank, level as i64);
        if before.get(&type_id).is_none_or(|&(old_level, old_sp)| old_level != level || old_sp != sp) {
            training::set_level(tx, char_id, type_id, level, sp).await.map_err(db_err)?;
            crate::dao::skills::history::append(tx, char_id, type_id, super::history::GM_GIVE, sp, now).await.map_err(db_err)?;
            changed.insert(type_id);
        }
    }
    // The reset discards every old interval; nothing is banked or retimed.
    if !old_queue.is_empty() { queue::clear(tx, char_id).await.map_err(db_err)?; }
    if free != 0 {
        if !crate::dao::characters::attributes::replace_free_points(tx, char_id, free, 0).await.map_err(db_err)? {
            return Err(notify_error(messages::skills::INVALID_ECONOMY_REQUEST));
        }
        outbox.notify_pilot(char_id, "OnFreeSkillPointsChanged", vec![Value::Int(0)]);
    }
    if !changed.is_empty() { outbox.event(crate::lua::events::GameEvent::SkillStateChanged { char_id }); }
    if changed.is_empty() && old_queue.is_empty() {
        completion::reschedule(tx, char_id).await
    } else {
        let affected: Vec<_> = changed.into_iter().chain(old_queue.iter().map(|row| row.type_id)).collect::<BTreeSet<_>>().into_iter().collect();
        let progress = AdvanceResult { queue_changed: !old_queue.is_empty(), brain_changed: true, ..Default::default() };
        completion::finish_tx(tx, outbox, sde, fences, char_id, now, &progress, &affected).await
    }
}

pub(crate) enum Change { Set(Vec<(i32, i32)>), Remove(Vec<i32>), RemoveAll, Starters, ResetToStarters }

pub(crate) async fn apply(c: &mut Client, char_id: i64, change: Change) -> Result<(), CallError> {
    let _lock = crate::services::market::lock().await;
    let db = c.db.clone();
    let sde = c.sde.clone();
    let now = filetime_now();
    let fences = online::capture_online_fences(&c.world, &[char_id]).await;
    db.transaction(|tx, outbox: &mut Outbox<OnlineChanged>| {
        let sde = sde.clone();
        let fences = fences.clone();
        let change = match &change {
            Change::Set(v) => Change::Set(v.clone()), Change::Remove(v) => Change::Remove(v.clone()),
            Change::RemoveAll => Change::RemoveAll, Change::Starters => Change::Starters,
            Change::ResetToStarters => Change::ResetToStarters,
        };
        Box::pin(async move {
            match change {
                Change::Set(levels) => set_levels(tx, outbox, &sde, &fences, char_id, &levels, now).await,
                Change::Remove(types) => remove(tx, outbox, &sde, &fences, char_id, &types, now).await,
                Change::RemoveAll => {
                    crate::dao::characters::selection::lock_character(tx, char_id).await.map_err(db_err)?;
                    let mut types: BTreeSet<_> = training::current(tx, char_id).await.map_err(db_err)?.into_iter().map(|s| s.type_id).collect();
                    types.extend(queue::list(tx, char_id).await.map_err(db_err)?.into_iter().map(|s| s.type_id));
                    let types: Vec<_> = types.into_iter().collect();
                    remove(tx, outbox, &sde, &fences, char_id, &types, now).await
                }
                Change::Starters => grant_starters(tx, outbox, &sde, &fences, char_id, now).await,
                Change::ResetToStarters => reset_to_starters(tx, outbox, &sde, &fences, char_id, now).await,
            }
        })
    }, |(), outbox| async move { let result = online::dispatch_changed((), outbox, c); crate::scheduler::wake(); result }).await
}

#[cfg(all(test, feature = "tests-skills"))] mod tests;
