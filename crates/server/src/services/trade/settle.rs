//! Atomic trade settlement and cancellation over `game.trades`, `game.trade_items`
//! and character wallets. `pvptradesvc.py` consumes the emitted events.
//! Terminal transitions cancel durable idle jobs in the same transaction.
use infinity_core::{consts::{FLAG_HANGAR, IX_LOCATION_ID, IX_OWNER_ID}};
use crate::{dao::{items::{loading::ItemQuery, transfers}, trades::{self, Trade}}, db::{executor::Executor, outbox::Outbox, Db}, sde::Sde, services::{db_err, items::load_items, market, wallets, CallError}, space::World};
use std::sync::Arc;
use marshal::Value;
use super::{TradeEffect, dispatch, timeout};

pub(super) fn tell(outbox: &mut Outbox<TradeEffect>, trade: &Trade, event: &str, args: Vec<Value>) {
    for char_id in [trade.a, trade.b] { outbox.notify_pilot(char_id, event, args.clone()); }
}

async fn changes(ex: &(impl Executor + ?Sized), sde: &Sde, outbox: &mut Outbox<TradeEffect>, trade: &Trade, before: &[crate::dao::items::loading::Item]) -> Result<(), CallError> {
    let ids: Vec<_> = before.iter().map(|item| item.item_id).collect();
    let after = load_items(ex, sde, ItemQuery::Ids(&ids)).await.map_err(db_err)?;
    for (old, new) in before.iter().zip(after.iter()) {
        if old.item_id != new.item_id { return Err(CallError::Internal(anyhow::anyhow!("trade item disappeared"))); }
        let mut changes = Vec::new();
        if old.location_id != new.location_id { changes.push((Value::Int(IX_LOCATION_ID), Value::Int(old.location_id))); }
        if old.owner_id != new.owner_id { changes.push((Value::Int(IX_OWNER_ID), Value::Int(old.owner_id))); }
        if !changes.is_empty() {
            tell(outbox, trade, "OnItemChange", vec![new.row(), Value::Dict(changes), Value::None]);
        }
    }
    if before.len() != after.len() { return Err(CallError::Internal(anyhow::anyhow!("trade item disappeared"))); }
    Ok(())
}

pub(super) async fn abort(ex: &(impl Executor + ?Sized), sde: &Sde, outbox: &mut Outbox<TradeEffect>, trade: &mut Trade, notify: bool) -> Result<(), CallError> {
    let roots = trades::items(ex, trade.id).await.map_err(db_err)?;
    let ids: Vec<_> = roots.iter().map(|item| item.id).collect();
    let before = load_items(ex, sde, ItemQuery::Ids(&ids)).await.map_err(db_err)?;
    if notify && before.len() != roots.len() {
        return Err(CallError::Internal(anyhow::anyhow!("trade item missing or type absent from SDE")));
    }
    for item in &roots {
        if !trades::relocate(ex, item.id, item.owner, trade.id, item.origin, item.flag).await.map_err(db_err)? {
            return Err(CallError::Internal(anyhow::anyhow!("staged trade item missing")));
        }
    }
    if notify { changes(ex, sde, outbox, trade, &before).await?; }
    trade.state = "cancelled".into();
    if !trades::update(ex, trade, crate::client::filetime_now()).await.map_err(db_err)? { return Err(CallError::Internal(anyhow::anyhow!("trade is no longer open"))); }
    trades::clear_items(ex, trade.id).await.map_err(db_err)?;
    timeout::cancel(ex, trade.id).await?;
    outbox.operation(TradeEffect::Remove(trade.id));
    if notify { tell(outbox, trade, "OnTradeCancel", vec![Value::Int(trade.id)]); }
    Ok(())
}
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Settled { Done, Underfunded }


pub(super) async fn complete(ex: &(impl Executor + ?Sized), sde: &Sde, outbox: &mut Outbox<TradeEffect>, trade: &mut Trade) -> Result<Settled, CallError> {
    let money_a = crate::isk::IskAmount::from_isk(trade.money_a)?;
    let money_b = crate::isk::IskAmount::from_isk(trade.money_b)?;
    for (char_id, amount) in [(trade.a, money_a), (trade.b, money_b)] {
        if !crate::dao::wallets::characters::covers(ex, char_id, amount).await.map_err(db_err)? {
            trade.accepted_a = false;
            trade.accepted_b = false;
            trade.updated_at = crate::client::filetime_now();
            if !trades::update(ex, trade, trade.updated_at).await.map_err(db_err)? { return Err(CallError::Internal(anyhow::anyhow!("trade is no longer open"))); }
            timeout::refresh(ex, outbox, trade).await?;
            outbox.operation(TradeEffect::Put(trade.clone()));
            tell(outbox, trade, "OnTradeOfferReset", vec![Value::Int(trade.id), Value::Bool(false)]);
            return Ok(Settled::Underfunded);
        }
    }
    let roots = trades::items(ex, trade.id).await.map_err(db_err)?;
    let ids: Vec<_> = roots.iter().map(|item| item.id).collect();
    let all = trades::descendants(ex, &ids).await.map_err(db_err)?;
    let captured = crate::dao::items::parent_locks::capture(ex, &all).await.map_err(db_err)?;
    let ancestry = crate::dao::items::deliveries::ancestry_many(ex, &ids).await.map_err(db_err)?;
    let mut requests: Vec<_> = all.iter().map(|&item_id| crate::dao::items::parent_locks::LockRequest {
        item_id, strength: crate::dao::items::parent_locks::LockStrength::Update,
    }).collect();
    requests.extend(ancestry.iter().filter(|row| !all.contains(&row.ancestor.item_id)).map(|row| crate::dao::items::parent_locks::LockRequest {
        item_id: row.ancestor.item_id,
        strength: if sde.types.get(row.ancestor.type_id).is_some_and(|ty| ty.category_id == Some(infinity_core::consts::CATEGORY_SHIP)) {
            crate::dao::items::parent_locks::LockStrength::Update
        } else { crate::dao::items::parent_locks::LockStrength::Share },
    }));
    let locks = crate::services::inventory::destination::lock_items(ex, sde, &requests, &[]).await?;
    if captured.len() != all.len() || captured.iter().any(|old| !locks.row(old.item_id).is_some_and(|row|
        row.type_id == old.type_id && row.owner_id == old.owner_id && row.location_id == old.location_id
        && row.flag_id == old.flag_id && row.quantity == old.quantity && row.singleton == old.singleton
        && row.jump_clone_id == old.jump_clone_id)) {
        return Err(CallError::Internal(anyhow::anyhow!("trade item metadata changed")));
    }
    if trades::descendants(ex, &ids).await.map_err(db_err)? != all
        || crate::dao::items::deliveries::ancestry_many(ex, &ids).await.map_err(db_err)? != ancestry {
        return Err(CallError::Internal(anyhow::anyhow!("trade item closure changed")));
    }
    let before = load_items(ex, sde, ItemQuery::Ids(&all)).await.map_err(db_err)?;
    if before.len() != all.len() {
        return Err(CallError::Internal(anyhow::anyhow!("trade item type missing from SDE")));
    }
    let mut plan = Vec::with_capacity(roots.len());
    for item in &roots {
        if !before.iter().any(|root| root.item_id == item.id && root.location_id == trade.id && root.owner_id == item.owner) {
            return Err(CallError::Internal(anyhow::anyhow!("staged trade item missing or changed")));
        }
        let recipient = if item.owner == trade.a { trade.b } else if item.owner == trade.b { trade.a } else { return Err(CallError::Internal(anyhow::anyhow!("invalid trade item owner"))) };
        plan.push(transfers::Transfer { item_id: item.id, owner: recipient, location: trade.station, flag: FLAG_HANGAR });
    }
    transfers::hand_over(ex, &plan, &all, &locks, &[]).await.map_err(db_err)?;
    changes(ex, sde, outbox, trade, &before).await?;
    wallets::debit(ex, outbox, trade.a, money_a).await?;
    wallets::credit(ex, outbox, trade.b, money_a).await?;
    wallets::debit(ex, outbox, trade.b, money_b).await?;
    wallets::credit(ex, outbox, trade.a, money_b).await?;
    trade.state = "done".into();
    if !trades::update(ex, trade, crate::client::filetime_now()).await.map_err(db_err)? { return Err(CallError::Internal(anyhow::anyhow!("trade is no longer open"))); }
    trades::clear_items(ex, trade.id).await.map_err(db_err)?;
    timeout::cancel(ex, trade.id).await?;
    outbox.operation(TradeEffect::Remove(trade.id));
    tell(outbox, trade, "OnTradeComplete", vec![Value::Int(trade.id), Value::list(ids.into_iter().map(Value::Int))]);
    Ok(Settled::Done)
}

pub(super) async fn abort_for_char(ex: &(impl Executor + ?Sized), sde: &Sde, outbox: &mut Outbox<TradeEffect>, char_id: i64) -> Result<(), CallError> {
    if let Some(mut trade) = trades::open_for(ex, char_id).await.map_err(db_err)? {
        abort(ex, sde, outbox, &mut trade, true).await?;
    }
    Ok(())
}

pub(super) async fn leave(db: &Db, world: &World, sde: Arc<Sde>, char_id: i64) -> Result<(), CallError> {
    let _lock = market::lock().await;
    leave_locked(db, world, sde, char_id).await
}

pub(super) async fn leave_locked(db: &Db, world: &World, sde: Arc<Sde>, char_id: i64) -> Result<(), CallError> {
    db.transaction(|tx, outbox| {
        let sde = sde.clone();
        Box::pin(async move {
            abort_for_char(tx, &sde, outbox, char_id).await
        })
    }, |value, outbox| async move { dispatch(value, outbox, world) }).await
}

pub(super) async fn recover(db: &Db, world: &World, sde: Arc<Sde>) -> Result<(), CallError> {
    let _lock = market::lock().await;
    db.transaction(|tx, outbox| {
        let sde = sde.clone();
        Box::pin(async move {
            for mut trade in trades::open(tx).await.map_err(db_err)? { abort(tx, &sde, outbox, &mut trade, false).await?; }
            Ok(())
        })
    }, |value, outbox| async move { dispatch(value, outbox, world) }).await
}
