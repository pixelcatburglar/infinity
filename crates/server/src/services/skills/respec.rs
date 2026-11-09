//! Attribute remapping for eve/client/script/ui/shared/neocom/attributes.py.
//! Uses game.character_respec, skills, skill_queue, skill_history and jobs;
//! timed and bonus entitlements serialize with skill queue mutations.
use super::{completion, db_err, keyval, training, CallError};
use infinity_core::consts::FILETIME_DAY;
use crate::{client::{filetime_now, Client}, dao::{characters::attributes, skills::{queue, training as skill_dao}}, db::{executor::Executor, outbox::{dispatch_client, Outbox}}, services::{effects::online::{self, OnlineChanged}, notify_error}};
use infinity_core::consts::dogma::CHAR_ATTRIBUTES;
use marshal::Value;

/// Client eve/common/lib/appConst.py:1582-1585 remap interval and bounds.
const INTERVAL: i64 = 365 * FILETIME_DAY;
const MINIMUM: i64 = 17;
const MAXIMUM: i64 = 27;
const TOTAL: i64 = 99;

#[derive(Debug, PartialEq)]
enum Invalid { Malformed, Low, High, Misallocated }

fn parse(value: &Value) -> Result<[i32; 5], Invalid> {
    let Value::Dict(pairs) = value else { return Err(Invalid::Malformed) };
    if pairs.len() != CHAR_ATTRIBUTES.len() { return Err(Invalid::Malformed); }
    let mut numbers = [0_i64; 5];
    let mut seen = [false; 5];
    for (key, value) in pairs {
        let Value::Int(id) = key else { return Err(Invalid::Malformed) };
        let Value::Int(n) = value else { return Err(Invalid::Malformed) };
        let pos = CHAR_ATTRIBUTES.iter().position(|a| a == id).ok_or(Invalid::Malformed)?;
        if seen[pos] { return Err(Invalid::Malformed); }
        seen[pos] = true;
        numbers[pos] = *n;
    }
    if numbers.iter().any(|n| *n < MINIMUM) { return Err(Invalid::Low); }
    if numbers.iter().any(|n| *n > MAXIMUM) { return Err(Invalid::High); }
    if numbers.iter().sum::<i64>() != TOTAL { return Err(Invalid::Misallocated); }
    Ok(numbers.map(|n| n as i32))
}

fn invalid_error(reason: Invalid) -> CallError {
    let msg = match reason {
        Invalid::Malformed => return notify_error(infinity_core::messages::skills::INVALID_REMAP),
        Invalid::Low => infinity_core::messages::MSG_RESPEC_ATTRIBUTES_TOO_LOW,
        Invalid::High => infinity_core::messages::MSG_RESPEC_ATTRIBUTES_TOO_HIGH,
        Invalid::Misallocated => infinity_core::messages::MSG_RESPEC_ATTRIBUTES_MISALLOCATED,
    };
    CallError::User { msg: msg.into(), dict: vec![] }
}

fn too_soon(next: i64) -> CallError {
    CallError::User { msg: infinity_core::messages::MSG_RESPEC_TOO_SOON.into(), dict: vec![("nextTime".into(), Value::Int(next))] }
}

fn info(row: attributes::Respec, now: i64) -> Value {
    keyval([
        ("freeRespecs", Value::Int(row.free_respecs)),
        ("lastRespecDate", row.last_respec_date.map(Value::Int).unwrap_or(Value::None)),
        ("nextTimedRespec", Value::Int(row.next_timed_respec.unwrap_or(now))),
    ])
}

pub(super) async fn get(c: &mut Client, char_id: i64) -> Result<Value, CallError> {
    let now = filetime_now();
    let db = c.db.clone();
    db.transaction(|tx, _outbox: &mut Outbox| Box::pin(async move {
        crate::dao::characters::selection::lock_character(tx, char_id).await.map_err(db_err)?;
        let row = attributes::initialize_respec(tx, char_id, crate::config::game().characters.free_respecs).await.map_err(db_err)?;
        Ok(info(row, now))
    }), |value, outbox| async move { dispatch_client(value, outbox, c) }).await
}

async fn apply_tx(tx: &(impl Executor + ?Sized), outbox: &mut Outbox<OnlineChanged>, sde: &std::sync::Arc<crate::sde::Sde>, fences: &online::OnlineFences, char_id: i64, bases: &[i32; 5], now: i64) -> Result<Value, CallError> {
    crate::dao::characters::selection::lock_character(tx, char_id).await.map_err(db_err)?;
    let state = attributes::initialize_respec(tx, char_id, crate::config::game().characters.free_respecs).await.map_err(db_err)?;
    let timed = state.next_timed_respec.is_none_or(|date| date <= now);
    if !timed && state.free_respecs <= 0 { return Err(too_soon(state.next_timed_respec.unwrap_or(now))); }
    let next = if timed { now + INTERVAL } else { state.next_timed_respec.unwrap_or(now) };
    let free = if timed { state.free_respecs } else { state.free_respecs - 1 };
    let mut progress = completion::advance_tx(tx, outbox, sde, char_id, now).await?;
    let banked = training::in_training_sp(tx, char_id, now).await?;
    skill_dao::write_skills(tx, char_id, &training::plan_skills(&[], banked)).await.map_err(db_err)?;
    let rows = queue::list(tx, char_id).await.map_err(db_err)?;
    let active = rows.first().is_some_and(|row| row.end_time.is_some());
    let entries = rows.into_iter().map(|r| (i64::from(r.position), i64::from(r.type_id), i64::from(r.to_level))).collect();
    if !attributes::save_respec(tx, char_id, bases, &state, free, now, next).await.map_err(db_err)? {
        return Err(notify_error(infinity_core::messages::skills::REMAP_UNAVAILABLE));
    }
    progress.brain_changed = true;
    training::save_queue_tx(tx, outbox, sde, fences, char_id, entries, active, true, now, progress, &[]).await?;
    outbox.notify_client("OnRespecInfoChanged", vec![]);
    Ok(Value::None)
}

pub(super) async fn apply(c: &mut Client, char_id: i64, input: &Value) -> Result<Value, CallError> {
    let bases = parse(input).map_err(invalid_error)?;
    let now = filetime_now();
    let db = c.db.clone();
    let sde = c.sde.clone();
    let _market = crate::services::market::lock().await;
    let fences = online::capture_online_fences(&c.world, &[char_id]).await;
    db.transaction(|tx, outbox: &mut Outbox<OnlineChanged>| {
        let sde = sde.clone();
        let fences = fences.clone();
        Box::pin(async move { apply_tx(tx, outbox, &sde, &fences, char_id, &bases, now).await })
    }, |value, outbox| async move {
        crate::scheduler::wake();
        online::dispatch_changed(value, outbox, c)
    }).await
}

#[cfg(test)] mod tests;
