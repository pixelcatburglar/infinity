//! Applying unallocated skill points for skillsvc.py.
//! Uses game.characters, skills, history, queue and jobs; changes dispatch after commit.
use super::{db_err, training, CallError};
use infinity_core::{consts::dogma::MAX_SKILL_LEVEL, messages};
use crate::{client::{filetime_now, Client}, dao::{characters::attributes, skills::{history, queue, trading, training as skill_dao}}, db::{executor::Executor, outbox::Outbox}, services::{effects::online::{self, OnlineChanged}, notify_error}};
use marshal::Value;

fn refusal() -> CallError { notify_error(messages::skills::INVALID_ECONOMY_REQUEST) }
fn missing_skill(sde: &crate::sde::Sde, type_id: i32) -> CallError {
    let Some(name) = sde.types.get(type_id).and_then(|ty| ty.name.as_deref()) else { return refusal(); };
    crate::services::user_error(messages::MSG_CANNOT_APPLY_FREE_POINTS_DO_NOT_HAVE_SKILL, vec![("skillName".into(), Value::unicode(name))])
}
type Target = (i32, i32);

fn distribute(rows: &[(i32, i32)], skills: &[skill_dao::Skill], free: i64, sde: &crate::sde::Sde, caps: Option<&[(i32, i64)]>) -> Result<Vec<(i32, i64, i32)>, CallError> {
    let mut simulated = skills.iter().map(|s| (s.type_id, s.skill_points)).collect::<std::collections::HashMap<_, _>>();
    let mut remaining = free;
    let mut allocations = Vec::new();
    for &(type_id, target) in rows {
        let skill = skills.iter().find(|s| s.type_id == type_id).ok_or_else(|| missing_skill(sde, type_id))?;
        let rank = training::skill_type(sde, type_id).rank;
        let target_sp = super::formulas::sp_for_level(rank, i64::from(target.min(MAX_SKILL_LEVEL)));
        let current = simulated.get(&type_id).copied().unwrap_or(skill.skill_points);
        let mut need = target_sp.saturating_sub(current).min(super::formulas::sp_for_level(rank, i64::from(MAX_SKILL_LEVEL)).saturating_sub(current));
        if let Some(cap) = caps.and_then(|caps| caps.iter().find(|(id, _)| *id == type_id)) { need = need.min(cap.1); }
        let points = need.min(remaining);
        if points > 0 {
            simulated.insert(type_id, current + points);
            remaining -= points;
            let level = super::formulas::level_from_points(current + points, rank);
            if let Some(existing) = allocations.iter_mut().find(|(id, _, _)| *id == type_id) { existing.1 += points; existing.2 = level; } else { allocations.push((type_id, points, level)); }
        }
        if remaining == 0 { break; }
    }
    Ok(allocations)
}

async fn apply_tx(tx: &(impl Executor + ?Sized), outbox: &mut Outbox<OnlineChanged>, sde: &std::sync::Arc<crate::sde::Sde>, fences: &online::OnlineFences, char_id: i64, targets: &[Target], caps: Option<&[(i32, i64)]>, now: i64) -> Result<i64, CallError> {
    crate::dao::characters::selection::lock_character(tx, char_id).await.map_err(db_err)?;
    let progress = super::completion::advance_tx(tx, outbox, sde, char_id, now).await?;
    let accrued = training::in_training_sp(tx, char_id, now).await?;
    skill_dao::write_skills(tx, char_id, &training::plan_skills(&[], accrued)).await.map_err(db_err)?;
    let skills = skill_dao::current(tx, char_id).await.map_err(db_err)?;
    let free = attributes::free_points(tx, char_id).await.map_err(db_err)?;
    let allocations = distribute(targets, &skills, free, sde, caps)?;
    if let Some(caps) = caps {
        for &(type_id, requested) in caps {
            let allocated = allocations.iter().find(|(id, _, _)| *id == type_id).map_or(0, |(_, points, _)| *points);
            if requested <= 0 || allocated != requested { return Err(refusal()); }
        }
    }
    let used = allocations.iter().try_fold(0_i64, |sum, (_, points, _)| sum.checked_add(*points).ok_or_else(refusal))?;
    if !attributes::replace_free_points(tx, char_id, free, free - used).await.map_err(db_err)? { return Err(refusal()); }
    let mut changed = Vec::with_capacity(allocations.len());
    for &(type_id, points, level) in &allocations {
        let skill = skills.iter().find(|s| s.type_id == type_id).ok_or_else(refusal)?;
        let new_sp = skill.skill_points.checked_add(points).ok_or_else(refusal)?;
        if !trading::replace_skill(tx, char_id, type_id, skill.level, skill.skill_points, level, new_sp).await.map_err(db_err)? { return Err(refusal()); }
        history::append(tx, char_id, type_id, super::history::FREE_POINTS_USED, new_sp, now).await.map_err(db_err)?;
        changed.push(type_id);
    }
    if !changed.is_empty() {
        outbox.event(crate::lua::events::GameEvent::SkillStateChanged { char_id });
    }
    let queue_rows = queue::list(tx, char_id).await.map_err(db_err)?;
    let levels = allocations.iter().map(|(type_id, _, level)| (*type_id, *level)).collect::<std::collections::HashMap<_, _>>();
    let activate = queue_rows.first().is_some_and(|row| row.start_time.is_some());
    let entries = queue_rows.into_iter().filter(|row| levels.get(&row.type_id).is_none_or(|level| row.to_level > *level)).enumerate().map(|(pos, row)| (pos as i64, i64::from(row.type_id), i64::from(row.to_level))).collect();
    training::save_queue_tx(tx, outbox, sde, fences, char_id, entries, activate, true, now, progress, &changed).await?;
    outbox.notify_client("OnFreeSkillPointsChanged", vec![Value::Int(free - used)]);
    Ok(free - used)
}

fn parse_targets(value: &Value) -> Result<Vec<Target>, CallError> {
    let (Value::List(values) | Value::Tuple(values)) = value else { return Err(refusal()); };
    values.iter().map(|value| {
        let (Value::Tuple(pair) | Value::List(pair)) = value else { return Err(refusal()); };
        let [Value::Int(type_id), Value::Int(level)] = pair.as_slice() else { return Err(refusal()); };
        Ok((i32::try_from(*type_id).map_err(|_| refusal())?, i32::try_from(*level).map_err(|_| refusal())?))
    }).collect()
}

pub(super) async fn preview(c: &mut Client, char_id: i64, targets: Vec<Target>) -> Result<Value, CallError> {
    let free = attributes::free_points(&c.db, char_id).await.map_err(db_err)?;
    let training = training::in_training_sp(&c.db, char_id, filetime_now()).await?;
    let mut skills = skill_dao::current(&c.db, char_id).await.map_err(db_err)?;
    if let Some((type_id, points)) = training { if let Some(skill) = skills.iter_mut().find(|s| s.type_id == type_id) { skill.skill_points = skill.skill_points.max(points); } }
    Ok(Value::Dict(distribute(&targets, &skills, free, &c.sde, None)?.into_iter().map(|(type_id, points, _)| (Value::Int(i64::from(type_id)), Value::Int(points))).collect()))
}

pub(super) async fn apply_targets(c: &mut Client, char_id: i64, targets: Vec<Target>, caps: Option<Vec<(i32, i64)>>) -> Result<Value, CallError> {
    let now = filetime_now(); let _lock = crate::services::market::lock().await; let db = c.db.clone(); let sde = c.sde.clone();
    let fences = online::capture_online_fences(&c.world, &[char_id]).await;
    db.transaction(|tx, outbox: &mut Outbox<OnlineChanged>| { let sde = sde.clone(); let targets = targets.clone(); let caps = caps.clone(); let fences = fences.clone(); Box::pin(async move { apply_tx(tx, outbox, &sde, &fences, char_id, &targets, caps.as_deref(), now).await }) }, |value, outbox| async move { crate::scheduler::wake(); online::dispatch_changed(value, outbox, c) }).await.map(Value::Int)
}

pub(super) async fn preview_queue(c: &mut Client, char_id: i64) -> Result<Value, CallError> {
    let targets = queue::list(&c.db, char_id).await.map_err(db_err)?
        .into_iter().map(|row| (row.type_id, row.to_level)).collect();
    preview(c, char_id, targets).await
}

pub(super) async fn preview_skills(c: &mut Client, char_id: i64, value: &Value) -> Result<Value, CallError> {
    preview(c, char_id, parse_targets(value)?).await
}

pub(super) async fn apply_queue(c: &mut Client, char_id: i64) -> Result<Value, CallError> {
    let targets = queue::list(&c.db, char_id).await.map_err(db_err)?
        .into_iter().map(|row| (row.type_id, row.to_level)).collect();
    apply_targets(c, char_id, targets, None).await
}

pub(super) async fn apply_skills(c: &mut Client, char_id: i64, value: &Value) -> Result<Value, CallError> {
    apply_targets(c, char_id, parse_targets(value)?, None).await
}

pub(super) async fn apply(c: &mut Client, char_id: i64, args: &[Value]) -> Result<Value, CallError> {
    let type_id = args.first().and_then(Value::as_int).and_then(|v| i32::try_from(v).ok()).ok_or_else(refusal)?;
    let points = args.get(1).and_then(Value::as_int).ok_or_else(refusal)?;
    apply_targets(c, char_id, vec![(type_id, MAX_SKILL_LEVEL)], Some(vec![(type_id, points)])).await
}

#[cfg(test)] mod tests;
