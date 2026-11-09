//! Atomic book injection and start-training for `skillsvc.py` and `dogmaIM`.
//! Plain injection learns an accessible published skill at level zero without prerequisites;
//! start-training then delegates prerequisite validation to the shared skill queue.
//! Uses startup SDE types and game items, skills, history and queue; clone-grade restrictions
//! and other client training-policy checks not represented by the shared queue are unsupported.
use super::{db_err, learning, trading::{consume, refusal, source}, training, CallError};
use infinity_core::{consts::CATEGORY_SKILL, messages};
use crate::{client::{filetime_now, Client}, dao::skills::training as skill_dao, db::{executor::Executor, outbox::Outbox}, sde::Sde, services::{effects::online::{self, OnlineChanged}, user_error}};
use marshal::Value;
use std::{collections::{BTreeSet, HashMap}, sync::Arc};

async fn inject_tx(tx: &(impl Executor + ?Sized), outbox: &mut Outbox<OnlineChanged>, sde: &Arc<Sde>, fences: &online::OnlineFences, char_id: i64, ship: Option<i64>, ids: &[i64], now: i64, start: Option<i64>) -> Result<Value, CallError> {
    let progress = learning::begin_tx(tx, outbox, sde, char_id, now).await?;
    let known: HashMap<_, _> = skill_dao::current(tx, char_id).await.map_err(db_err)?.into_iter().map(|s| (s.type_id, s.level)).collect();
    let mut types = BTreeSet::new();
    let mut items = Vec::new();
    for &id in ids {
        let item = source(tx, sde, char_id, ship, id, false).await?;
        if start.is_some_and(|location| location != item.location_id) { return Err(refusal()); }
        let data = sde.types.get(item.type_id).ok_or_else(refusal)?;
        if data.category_id != Some(CATEGORY_SKILL) || data.published != Some(true) {
            return Err(user_error(messages::MSG_ITEM_NOT_A_SKILL, vec![("skillName".into(), Value::Int(i64::from(item.type_id)))]));
        }
        if known.contains_key(&item.type_id) || !types.insert(item.type_id) { return Err(refusal()); }
        items.push(item);
    }
    for item in items {
        consume(tx, outbox, item, 1).await?;
    }
    learning::learn_tx(tx, char_id, &types, now).await?;
    let changed: Vec<_> = types.into_iter().collect();
    if start.is_some() {
        let type_id = *changed.first().ok_or_else(refusal)?;
        training::save_queue_tx(tx, outbox, sde, fences, char_id, vec![(0, i64::from(type_id), 1)], true, true, now, progress, &changed).await?;
    } else {
        super::completion::finish_tx(tx, outbox, sde, fences, char_id, now, &progress, &changed).await?;
    }
    Ok(Value::None)
}

pub(crate) async fn inject(c: &mut Client, args: &[Value]) -> Result<Value, CallError> {
    let values = args.first().and_then(Value::items).ok_or_else(refusal)?;
    let mut ids: Vec<_> = values.iter().map(|v| v.as_int().ok_or_else(refusal)).collect::<Result<_, _>>()?;
    if ids.is_empty() { return Err(refusal()); }
    ids.sort_unstable();
    if ids.windows(2).any(|pair| pair[0] == pair[1]) { return Err(refusal()); }
    let char_id = c.session.int("charid").unwrap_or(0);
    let ship = c.session.int("shipid");
    let now = filetime_now();
    let _lock = crate::services::market::lock().await;
    let db = c.db.clone();
    let sde = c.sde.clone();
    let fences = online::capture_online_fences(&c.world, &[char_id]).await;
    db.transaction(|tx, outbox: &mut Outbox<OnlineChanged>| { let sde = sde.clone(); let ids = ids.clone(); let fences = fences.clone(); Box::pin(async move { inject_tx(tx, outbox, &sde, &fences, char_id, ship, &ids, now, None).await }) }, |value, outbox| async move { crate::scheduler::wake(); online::dispatch_changed(value, outbox, c) }).await
}

/// skillsvc.py Train: inject the accessible book and replace the active queue.
pub(super) async fn start_training(c: &mut Client, char_id: i64, args: &[Value]) -> Result<Value, CallError> {
    let id = args.first().and_then(Value::as_int).ok_or_else(refusal)?;
    let location = args.get(1).and_then(Value::as_int).ok_or_else(refusal)?;
    let ship = c.session.int("shipid");
    let now = filetime_now();
    let _lock = crate::services::market::lock().await;
    let db = c.db.clone();
    let sde = c.sde.clone();
    let fences = online::capture_online_fences(&c.world, &[char_id]).await;
    db.transaction(|tx, outbox: &mut Outbox<OnlineChanged>| {
        let sde = sde.clone();
        let fences = fences.clone();
        Box::pin(async move { inject_tx(tx, outbox, &sde, &fences, char_id, ship, &[id], now, Some(location)).await })
    }, |value, outbox| async move { crate::scheduler::wake(); online::dispatch_changed(value, outbox, c) }).await
}

#[cfg(test)] mod tests;
