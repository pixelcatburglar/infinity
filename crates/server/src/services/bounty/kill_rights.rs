//! Kill-right lifecycle and durable expiry, mirroring the client kill-right UI.
//! Rights are persisted in game.kill_rights; suspect flags remain in memory.
use super::*;
use infinity_core::{consts::{FILETIME_DAY, FILETIME_SECOND}, messages};
use crate::{client::filetime_now, dao::kill_rights as dao, db::{Db, executor::Executor, outbox::{dispatch_world, Outbox}}, scheduler::{self, Job}, space::World};
use crate::services::{db_err, keyval, market, notify_error, user_error, wallets, CallError};
use marshal::Value;
use std::{future::Future, pin::Pin, sync::Arc};

/// Client notificationconst kill-right group and types.
const GROUP: i32 = 4;
const SOLD_RESTRICTED: i32 = 115;
const SOLD_OPEN: i32 = 116;
const CREATED: i32 = 117;
const USED: i32 = 118;
const CANCEL_RESTRICTED: i32 = 119;
const CANCEL_OPEN: i32 = 120;
pub(in crate::services) const EXPIRY_KIND: &str = "kill-right-expiry";
/// Exclusive storage limit of a nonnegative NUMERIC(36,2) sale price.
const PRICE_STORAGE_LIMIT: f64 = 1e34;

fn option_id(value: Option<i64>) -> Value { value.map(Value::Int).unwrap_or(Value::None) }
fn row(r: &dao::KillRight) -> Value {
    keyval([("killRightID", Value::Int(r.kill_right_id)), ("fromID", Value::Int(r.from_id)),
        ("toID", Value::Int(r.to_id)), ("expiryTime", Value::Int(r.expiry)),
        ("price", r.price.map(Value::Float).unwrap_or(Value::None)), ("restrictedTo", option_id(r.restricted_to))])
}
fn viewer(c: &Client) -> Vec<i64> {
    ["charid", "corpid", "allianceid"].iter().filter_map(|key| c.session.int(key)).collect()
}
fn error(key: &str) -> CallError { user_error(key, vec![]) }
fn expired() -> CallError { error(messages::MSG_KILL_RIGHT_EXPIRED) }
fn invalid() -> CallError { error(messages::MSG_NO_VALID_KILL_RIGHT) }
fn deadline(now: i64) -> i64 { now + crate::config::game().bounty.kill_right_suspect_secs * FILETIME_SECOND }

pub(super) fn sale_price(price: f64) -> Option<f64> {
    if !price.is_finite() { return None; }
    let price = market::round2(price);
    (price.is_finite() && (0.0..PRICE_STORAGE_LIMIT).contains(&price)).then_some(price)
}

pub(super) async fn mine(c: &Client) -> CallResult {
    let me = c.session.int("charid").unwrap_or(0);
    Ok(Value::list(dao::mine(&c.db, me, filetime_now()).await.map_err(db_err)?.iter().map(row)))
}
pub(super) async fn visible(c: &Client, targets: &[i64]) -> CallResult {
    Ok(Value::list(dao::visible(&c.db, targets, &viewer(c), filetime_now()).await.map_err(db_err)?.iter().map(row)))
}

pub(in crate::services) async fn earn(db: &Db, world: &Arc<World>, from: i64, to: i64) -> Result<(), CallError> {
    let now = filetime_now();
    let expiry = now + crate::config::game().bounty.kill_right_lifetime_days * FILETIME_DAY;
    db.transaction(|tx, outbox: &mut Outbox| Box::pin(async move {
        if let Some(right) = dao::create(tx, from, to, now, expiry).await.map_err(db_err)? {
            let id = right.kill_right_id;
            let job = Job { id: format!("{EXPIRY_KIND}:{id}"), kind: EXPIRY_KIND.into(), due_at: expiry, payload: id.to_string() };
            scheduler::schedule(tx, &job).await.map_err(db_err)?;
            for receiver in [from, to] {
                outbox.notify_pilot(receiver, "OnKillRightCreated", vec![Value::Int(id), Value::Int(from), Value::Int(to), Value::Int(expiry)]);
            }
            super::super::mail::notify(tx, outbox, from, CREATED, GROUP, to, &format!("{{charID: {to}}}")).await?;
        }
        Ok(())
    }), |value, outbox| async move { let result = dispatch_world(value, outbox, world); scheduler::wake(); result }).await
}

pub(in crate::services) fn expire(db: Db, _world: Arc<World>, job: Job) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send>> {
    Box::pin(async move {
        let id: i64 = job.payload.parse()?;
        dao::delete_expired(&db, id, filetime_now()).await?;
        Ok(())
    })
}

pub(super) async fn sell(c: &Client, id: i64, price: f64, restriction: Option<i64>) -> CallResult {
    let price = sale_price(price).ok_or_else(|| notify_error(messages::bounty::INVALID_PRICE))?;
    let me = c.session.int("charid").unwrap_or(0);
    let now = filetime_now();
    c.db.transaction(|tx, outbox: &mut Outbox| Box::pin(async move {
        let original = dao::get(tx, id).await.map_err(db_err)?.ok_or_else(expired)?;
        if original.from_id != me { return Err(notify_error(messages::bounty::NOT_RIGHT_OWNER)); }
        if original.expiry <= now || original.used_at.is_some() { return Err(expired()); }
        if let Some(target) = restriction {
            if crate::dao::characters::social::owners(tx, &[target], bounties::CHARACTER as i64, bounties::CORPORATION as i64, bounties::ALLIANCE as i64).await.map_err(db_err)?.is_empty() { return Err(notify_error(messages::bounty::INVALID_RESTRICTION)); }
        }
        let right = dao::sell(tx, id, me, price, restriction, now).await.map_err(db_err)?.ok_or_else(invalid)?;
        let (type_id, data) = match restriction {
            Some(target) => (SOLD_RESTRICTED, format!("{{charID: {me}, price: {price}, toEntityID: {target}}}")),
            None => (SOLD_OPEN, format!("{{charID: {me}, price: {price}}}")),
        };
        super::super::mail::notify(tx, outbox, right.to_id, type_id, GROUP, me, &data).await?;
        Ok(Value::list([row(&right)]))
    }), |value, outbox| async move { dispatch_world(value, outbox, &c.world) }).await
}

pub(super) async fn cancel(c: &Client, id: i64, target: i64) -> CallResult {
    let me = c.session.int("charid").unwrap_or(0);
    let now = filetime_now();
    c.db.transaction(|tx, outbox: &mut Outbox| Box::pin(async move {
        let original = dao::get(tx, id).await.map_err(db_err)?.ok_or_else(expired)?;
        if original.from_id != me { return Err(notify_error(messages::bounty::NOT_RIGHT_OWNER)); }
        if original.expiry <= now || original.used_at.is_some() { return Err(expired()); }
        let prior = dao::cancel(tx, id, me, target, now).await.map_err(db_err)?.ok_or_else(invalid)?;
        if prior.price.is_some() {
            let (type_id, data) = match prior.restricted_to {
                Some(restriction) => (CANCEL_RESTRICTED, format!("{{charID: {me}, toEntityID: {restriction}}}")),
                None => (CANCEL_OPEN, format!("{{charID: {me}}}")),
            };
            super::super::mail::notify(tx, outbox, target, type_id, GROUP, me, &data).await?;
        }
        Ok(Value::None)
    }), |value, outbox| async move { dispatch_world(value, outbox, &c.world) }).await
}

async fn used<X: Send>(tx: &(impl Executor + ?Sized), outbox: &mut Outbox<X>, right: &dao::KillRight) -> Result<(), CallError> {
    let mut recipients = vec![right.from_id, right.to_id];
    if let Some(actor) = right.activated_by { recipients.push(actor); }
    recipients.sort_unstable(); recipients.dedup();
    for receiver in recipients { outbox.notify_pilot(receiver, "OnKillRightUsed", vec![Value::Int(right.kill_right_id), Value::Int(right.to_id)]); }
    super::super::mail::notify(tx, outbox, right.from_id, USED, GROUP, right.to_id, &format!("{{charID: {}}}", right.to_id)).await
}
pub(in crate::services) async fn consume<X: Send>(tx: &(impl Executor + ?Sized), outbox: &mut Outbox<X>, target: i64, now: i64) -> Result<(), CallError> {
    for right in dao::consume_active(tx, target, now).await.map_err(db_err)? { used(tx, outbox, &right).await?; }
    Ok(())
}

pub(super) async fn buy(c: &Client, id: i64, price: f64) -> CallResult {
    let me = c.session.int("charid").unwrap_or(0);
    let viewers = viewer(c);
    let _lock = market::lock().await;
    let now = filetime_now();
    c.db.transaction(|tx, outbox: &mut Outbox| {
        let viewers = viewers.clone();
        Box::pin(async move {
            let old = dao::get(tx, id).await.map_err(db_err)?.ok_or_else(expired)?;
            if old.expiry <= now || old.used_at.is_some() { return Err(expired()); }
            let listed = old.price.ok_or_else(|| error(messages::MSG_KILL_RIGHT_NOT_FOR_SALE))?;
            if !price.is_finite() || price != listed || me == old.from_id { return Err(invalid()); }
            let buyback = me == old.to_id;
            let right = dao::buy(tx, id, price, me, &viewers, now, deadline(now), buyback).await.map_err(db_err)?.ok_or_else(invalid)?;
            let amount = crate::isk::IskAmount::from_isk(price)?;
            if me < old.from_id { wallets::debit(tx, outbox, me, amount).await?; wallets::credit(tx, outbox, old.from_id, amount).await?; }
            else { wallets::credit(tx, outbox, old.from_id, amount).await?; wallets::debit(tx, outbox, me, amount).await?; }
            outbox.notify_pilot(old.from_id, "OnKillRightForYouSold", vec![Value::Int(id)]);
            if buyback { used(tx, outbox, &right).await?; }
            Ok((right.to_id, right.activated_until, buyback))
        })
    }, |(target, until, buyback), outbox| async move {
        dispatch_world((), outbox, &c.world)?;
        if !buyback {
            if let Some(until) = until { super::super::crimewatch::flag_suspect(&c.db, &c.world, target, until).await; }
            c.world.notify_pilot(me, "OnKillRightActivated", vec![]);
        }
        Ok(Value::None)
    }).await
}

pub(super) async fn activate(c: &Client, id: i64) -> CallResult {
    let me = c.session.int("charid").unwrap_or(0);
    let now = filetime_now();
    c.db.transaction(|tx, _outbox: &mut Outbox| Box::pin(async move {
        let right = dao::get(tx, id).await.map_err(db_err)?.ok_or_else(expired)?;
        if right.expiry <= now || right.used_at.is_some() { return Err(expired()); }
        let right = dao::activate(tx, id, me, now, deadline(now)).await.map_err(db_err)?.ok_or_else(invalid)?;
        Ok((right.to_id, right.activated_until))
    }), |(target, until), outbox| async move {
        dispatch_world((), outbox, &c.world)?;
        if let Some(until) = until { super::super::crimewatch::flag_suspect(&c.db, &c.world, target, until).await; }
        c.world.notify_pilot(me, "OnKillRightActivated", vec![]);
        Ok(Value::None)
    }).await
}
