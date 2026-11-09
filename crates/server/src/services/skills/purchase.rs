//! Direct skill purchases for characterskills/client/purchase.py and characterskills/purchase.py.
//! Uses imported SDE types and type lists, game characters, skills, history, jobs and wallet journal.
//! Prices use server cent rounding; charged skills are journalled individually. Skillbook consumption and prerequisites are not involved.
use super::{completion, db_err, learning, trading::refusal, CallError};
use infinity_core::{consts::{CATEGORY_SKILL, GROUP_CLOUD_COMPUTING, GROUP_SHIP_ADAPTION, GROUP_CHARACTER_ENHANCEMENT, OWNER_UNOWNED, TYPE_LIST_SKILLS_AVAILABLE_FOR_PURCHASE}, messages};
use crate::{client::{filetime_now, Client}, dao::{self, skills::training, wallets::journal::Entry}, db::{executor::Executor, outbox::Outbox}, sde::Sde, services::{effects::online::{self, OnlineChanged}, market, user_error, wallets::{self, ownership::FinancialOwner}}};
use marshal::Value;
use std::{collections::{BTreeMap, BTreeSet, HashSet}, sync::Arc};

/// characterskills/common/purchase.py: direct purchase surcharge.
const DIRECT_PURCHASE_TAX_RATE: f64 = 0.3;
/// eve/common/lib/appConst.py: refSkillPurchase.
const REF_SKILL_PURCHASE: i32 = 141;

fn parse(args: &[Value], kwargs: &Value) -> Result<Vec<i64>, CallError> {
    if !matches!(kwargs, Value::None) && !matches!(kwargs, Value::Dict(entries) if entries.is_empty()) { return Err(refusal()); }
    let [Value::List(values)] = args else { return Err(refusal()) };
    values.iter().map(|value| match value { Value::Int(id) => Ok(*id), _ => Err(refusal()) }).collect()
}

fn error(key: &str, argument: &str, id: i64) -> CallError {
    user_error(key, vec![(argument.into(), Value::Int(id))])
}

fn purchase_price(published: Option<bool>, group: Option<i32>, listed: bool, base: Option<f64>) -> Option<f64> {
    if published != Some(true) || group.is_some_and(|id| [GROUP_CLOUD_COMPUTING, GROUP_SHIP_ADAPTION, GROUP_CHARACTER_ENHANCEMENT].contains(&id)) || !listed { return None; }
    base.filter(|price| price.is_finite() && *price >= 0.0)
        .map(|price| market::round2(price * (1.0 + DIRECT_PURCHASE_TAX_RATE)))
        .filter(|price| price.is_finite())
}

fn validate(ids: &[i64], known: &HashSet<i32>, is_skill: impl Fn(i64) -> bool, price: impl Fn(i64) -> Option<f64>) -> Result<(BTreeMap<i32, f64>, f64), CallError> {
    let mut unique = HashSet::new();
    if ids.iter().any(|id| !unique.insert(*id)) { return Err(user_error(messages::MSG_SKILL_PURCHASE_UNKNOWN_ERROR, vec![])); }
    for &id in ids {
        if !is_skill(id) {
            return Err(error(messages::MSG_ITEM_NOT_A_SKILL, "skillName", id));
        }
    }
    let mut total = 0.0;
    let mut prices = BTreeMap::new();
    for &id in ids {
        let type_id = i32::try_from(id).map_err(|_| refusal())?;
        let price = price(id).ok_or_else(|| error(messages::MSG_SKILL_UNAVAILABLE_FOR_PURCHASE, "type", id))?;
        total += price;
        if !total.is_finite() { return Err(error(messages::MSG_SKILL_UNAVAILABLE_FOR_PURCHASE, "type", id)); }
        prices.insert(type_id, price);
    }
    for &id in ids {
        if i32::try_from(id).ok().is_some_and(|id| known.contains(&id)) { return Err(error(messages::MSG_CHARACTER_ALREADY_KNOWS_SKILL, "skillName", id)); }
    }
    Ok((prices, market::round2(total)))
}

async fn purchase_tx(tx: &(impl Executor + ?Sized), outbox: &mut Outbox<OnlineChanged>, sde: &Arc<Sde>, fences: &online::OnlineFences, char_id: i64, ids: &[i64], now: i64) -> Result<Value, CallError> {
    let progress = learning::begin_tx(tx, outbox, sde, char_id, now).await?;
    let known = training::current(tx, char_id).await.map_err(db_err)?.into_iter().map(|skill| skill.type_id).collect();
    let (prices, total) = validate(ids, &known,
        |id| i32::try_from(id).ok().and_then(|id| sde.types.get(id)).is_some_and(|ty| ty.category_id == Some(CATEGORY_SKILL)),
        |id| {
            let type_id = i32::try_from(id).ok()?;
            let ty = sde.types.get(type_id)?;
            purchase_price(ty.published, ty.group_id, sde.typelists.contains(TYPE_LIST_SKILLS_AVAILABLE_FOR_PURCHASE, type_id, ty.group_id, ty.category_id), ty.base_price)
        })?;
    let total = crate::isk::IskAmount::from_isk(total)?;
    if !total.is_zero() && !dao::wallets::characters::covers(tx, char_id, total).await.map_err(db_err)? {
        return Err(wallets::not_enough_money(wallets::balance(tx, char_id).await?, total));
    }
    for (&type_id, &price) in &prices {
        if price > 0.0 {
            let entry = Entry {
                actor_id: char_id, entry_type_id: REF_SKILL_PURCHASE,
                owner_id1: char_id, owner_id2: OWNER_UNOWNED,
                reference_id: i64::from(type_id), created_at: now, description: "",
                market_transaction_id: None, order_id: None, side: 0,
            };
            wallets::journal::debit(tx, outbox, FinancialOwner::cash(char_id), crate::isk::IskAmount::from_isk(price)?, &entry).await?;
        }
    }
    let types: BTreeSet<_> = prices.into_keys().collect();
    learning::learn_tx(tx, char_id, &types, now).await?;
    let changed: Vec<_> = types.into_iter().collect();
    completion::finish_tx(tx, outbox, sde, fences, char_id, now, &progress, &changed).await?;
    Ok(Value::list(ids.iter().copied().map(Value::Int)))
}

pub(super) async fn purchase(c: &mut Client, char_id: i64, args: &[Value], kwargs: &Value) -> Result<Value, CallError> {
    let ids = parse(args, kwargs)?;
    if ids.is_empty() { return Ok(Value::list([])); }
    let now = filetime_now();
    let _lock = market::lock().await;
    let db = c.db.clone();
    let sde = c.sde.clone();
    let fences = online::capture_online_fences(&c.world, &[char_id]).await;
    db.transaction(|tx, outbox: &mut Outbox<OnlineChanged>| {
        let ids = ids.clone();
        let sde = sde.clone();
        let fences = fences.clone();
        Box::pin(async move { purchase_tx(tx, outbox, &sde, &fences, char_id, &ids, now).await })
    }, |value, outbox| async move { crate::scheduler::wake(); online::dispatch_changed(value, outbox, c) }).await
}

#[cfg(test)] mod tests;
