//! GM skill mutations, using the same SDE ranks and race seed as character creation.
//! Mirrors skillsvc.py notifications and persists game.skills and game.skill_queue.
//! Skill queues for changed skills are removed; unrelated entries retain their order.
//! Completed training and GM grants share one skills-changed notification per batch.
//! Skill reductions reconcile the owner's online fitted ships after the batch.
use super::{db_err, keyval, CallError};
use super::{formulas::sp_for_level, starting::starting_levels, training::{advance_training, queue_list_db, queue_training_events, skill_type}};
use crate::{client::{Client, filetime_now, BRAIN_REFRESH}, consts::CATEGORY_SKILL, dao::{items::{fit, ship}, skills::{queue, training}}, db::{executor::Executor, outbox::Outbox}, sde::Sde};
use crate::services::effects::online::{self, OnlineChanged};
use marshal::Value;
use std::collections::{BTreeMap, BTreeSet};


pub(crate) fn is_skill(sde: &Sde, type_id: i32) -> bool {
    sde.types.get(type_id).is_some_and(|t| t.category_id == Some(CATEGORY_SKILL))
}

pub(crate) fn all_levels(sde: &Sde, published: bool, level: i32) -> Vec<(i32, i32)> {
    sde.types.iter().filter(|t| t.category_id == Some(CATEGORY_SKILL) && (!published || t.published == Some(true))).map(|t| (t.type_id, level)).collect()
}

fn skill_info(sde: &Sde, type_id: i32, level: i32, points: i64, now: i64) -> Value {
    keyval([
        ("typeID", Value::Int(type_id as i64)),
        ("trainedSkillLevel", Value::Int(level as i64)),
        ("effectiveSkillLevel", Value::Int(level as i64)),
        ("virtualSkillLevel", Value::None),
        ("trainedSkillPoints", Value::Int(points)),
        ("skillPoints", Value::Int(points)),
        ("skillLevel", Value::Int(level as i64)),
        ("skillRank", Value::Float(skill_type(sde, type_id).rank)),
        ("timeStamp", Value::Int(now)),
    ])
}

async fn clean_queue<E>(tx: &(impl Executor + ?Sized), outbox: &mut Outbox<E>, sde: &Sde, char_id: i64, types: &BTreeSet<i32>, now: i64, finished: &[(i32, i32)]) -> Result<Vec<(Value, Value)>, CallError> {
    let changed: BTreeSet<_> = finished.iter().map(|(type_id, _)| *type_id).filter(|id| !types.contains(id)).collect();
    let infos = if changed.is_empty() {
        Vec::new()
    } else {
        training::current(tx, char_id).await.map_err(db_err)?.into_iter()
            .filter(|skill| changed.contains(&skill.type_id))
            .map(|skill| (Value::Int(skill.type_id as i64), skill_info(sde, skill.type_id, skill.level, skill.skill_points, now))).collect()
    };
    let rows = queue::list(tx, char_id).await.map_err(db_err)?;
    if !rows.iter().any(|r| types.contains(&r.type_id)) {
        if !finished.is_empty() {
            outbox.notify_pilot(char_id, "OnNewSkillQueueSaved", vec![queue_list_db(tx, char_id).await?]);
        }
        return Ok(infos);
    }
    queue::clear(tx, char_id).await.map_err(db_err)?;
    let rows = retain_queue(rows, types, now);
    for row in rows {
        queue::insert(tx, char_id, &row).await.map_err(db_err)?;
    }
    outbox.notify_pilot(char_id, "OnNewSkillQueueSaved", vec![queue_list_db(tx, char_id).await?]);
    Ok(infos)
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

pub(crate) async fn set_levels<E>(tx: &(impl Executor + ?Sized), outbox: &mut Outbox<E>, sde: &Sde, char_id: i64, levels: &[(i32, i32)]) -> Result<(), CallError> {
    let now = filetime_now();
    let finished = advance_training(tx, char_id, now).await?;
    queue_training_events(outbox, char_id, &finished);
    write_levels(tx, outbox, sde, char_id, levels, now, &finished).await
}

async fn write_levels<E>(tx: &(impl Executor + ?Sized), outbox: &mut Outbox<E>, sde: &Sde, char_id: i64, levels: &[(i32, i32)], now: i64, finished: &[(i32, i32)]) -> Result<(), CallError> {
    let plan: BTreeMap<_, _> = levels.iter().copied().collect();
    let types = plan.keys().copied().collect();
    let mut infos = clean_queue(tx, outbox, sde, char_id, &types, now, finished).await?;
    for (type_id, level) in plan {
        let sp = sp_for_level(skill_type(sde, type_id).rank, level as i64);
        training::set_level(tx, char_id, type_id, level, sp).await.map_err(db_err)?;
        infos.push((Value::Int(type_id as i64), skill_info(sde, type_id, level, sp, now)));
    }
    if !infos.is_empty() {
        outbox.notify_pilot(char_id, "OnServerSkillsChanged", vec![Value::Dict(infos), Value::None, Value::Int(now)]);
    }
    outbox.notify_pilot(char_id, BRAIN_REFRESH, vec![]);
    Ok(())
}

pub(crate) async fn remove<E>(tx: &(impl Executor + ?Sized), outbox: &mut Outbox<E>, sde: &Sde, char_id: i64, type_ids: &[i32]) -> Result<(), CallError> {
    let now = filetime_now();
    let types: BTreeSet<_> = type_ids.iter().copied().collect();
    let finished = advance_training(tx, char_id, now).await?;
    queue_training_events(outbox, char_id, &finished);
    let changed = clean_queue(tx, outbox, sde, char_id, &types, now, &finished).await?;
    if !changed.is_empty() {
        outbox.notify_pilot(char_id, "OnServerSkillsChanged", vec![Value::Dict(changed), Value::None, Value::Int(now)]);
    }
    let sorted: Vec<_> = types.into_iter().collect();
    let removed = training::remove(tx, char_id, &sorted).await.map_err(db_err)?;
    let infos: Vec<_> = removed.into_iter().map(|s| (Value::Int(s.type_id as i64), skill_info(sde, s.type_id, 0, -1, now))).collect();
    if !infos.is_empty() {
        outbox.notify_pilot(char_id, "OnServerSkillsRemoved", vec![Value::Dict(infos), Value::Int(now)]);
    }
    outbox.notify_pilot(char_id, BRAIN_REFRESH, vec![]);
    Ok(())
}

async fn grant_starters<E>(tx: &(impl Executor + ?Sized), outbox: &mut Outbox<E>, sde: &Sde, char_id: i64) -> Result<(), CallError> {
    let now = filetime_now();
    let finished = advance_training(tx, char_id, now).await?;
    queue_training_events(outbox, char_id, &finished);
    let race = training::race(tx, char_id).await.map_err(db_err)?.ok_or_else(|| CallError::Internal(anyhow::anyhow!("character disappeared")))?;
    let existing: BTreeSet<_> = training::current(tx, char_id).await.map_err(db_err)?.into_iter().map(|s| s.type_id).collect();
    let seed: Vec<_> = starting_levels(sde, race).filter(|(type_id, _)| !existing.contains(type_id)).collect();
    write_levels(tx, outbox, sde, char_id, &seed, now, &finished).await
}

pub(crate) enum Change { Set(Vec<(i32, i32)>), Remove(Vec<i32>), RemoveAll, Starters }

pub(crate) async fn apply(c: &mut Client, char_id: i64, change: Change) -> Result<(), CallError> {
    let _lock = crate::services::market::lock().await;
    let db = c.db.clone();
    let sde = c.sde.clone();
    db.transaction(|tx, outbox: &mut Outbox<OnlineChanged>| {
        let sde = sde.clone();
        let change = match &change {
            Change::Set(v) => Change::Set(v.clone()), Change::Remove(v) => Change::Remove(v.clone()),
            Change::RemoveAll => Change::RemoveAll, Change::Starters => Change::Starters,
        };
        Box::pin(async move {
            let candidates = fit::owner_online_ships(tx, char_id).await.map_err(db_err)?;
            let ships: Vec<_> = crate::services::items::load_items(tx, &sde, crate::dao::items::loading::ItemQuery::Ids(&candidates)).await.map_err(db_err)?
                .into_iter().filter(|item| item.singleton && item.category_id == i64::from(crate::consts::CATEGORY_SHIP)).map(|item| item.item_id).collect();
            for &hull in &ships {
                if !ship::lock_reload_ship(tx, hull).await.map_err(db_err)? {
                    return Err(CallError::Internal(anyhow::anyhow!("ship disappeared")));
                }
            }
            match change {
                Change::Set(levels) => set_levels(tx, outbox, &sde, char_id, &levels).await,
                Change::Remove(types) => remove(tx, outbox, &sde, char_id, &types).await,
                Change::RemoveAll => {
                    let mut types: BTreeSet<_> = training::current(tx, char_id).await.map_err(db_err)?.into_iter().map(|s| s.type_id).collect();
                    types.extend(queue::list(tx, char_id).await.map_err(db_err)?.into_iter().map(|s| s.type_id));
                    let types: Vec<_> = types.into_iter().collect();
                    remove(tx, outbox, &sde, char_id, &types).await
                }
                Change::Starters => grant_starters(tx, outbox, &sde, char_id).await,
            }?;
            for hull in ships {
                online::reconcile_core(tx, &sde, outbox, char_id, hull).await?;
            }
            Ok(())
        })
    }, |(), outbox| async move { online::dispatch_changed((), outbox, c) }).await
}

#[cfg(test)] mod tests;
