//! Cancellation, repricing and scheduled expiry for `marketProxy`
//! (`eve/client/script/ui/services/marketsvc.py`), using game market orders and wallets.
//! Only the original issuer with current authority can modify or cancel a corporate order;
//! passive expiry refunds its original owner and custody. Structure markets are unsupported.
//! Overdue owned orders cannot be repriced.
use super::*;
use super::fees::{skills, station_broker_rate};
use super::trading::{consume, hit_bids, lift_asks};
use crate::dao::market::trading as trading_dao;
use std::future::Future;
use std::sync::Arc;
use crate::services::identity::Identity;

/// Give a closed order's escrow back: a sell order's items, a buy order's ISK.
async fn refund<X: Send>(db: &(impl Executor + ?Sized), sde: &Arc<crate::sde::Sde>, outbox: &mut Outbox<X>, o: &Order, now: i64) -> Result<(), CallError> {
    let Some(party) = TradeParty::from_order(o) else { return Ok(()) };
    if o.bid {
        journal::credit(db, outbox, party.financial, o.escrow, &journal_entry(party.issuer, REF_MARKET_ESCROW, party.financial.id, OWNER_SCC, Some(o.id), None, now, 0)).await
    } else if o.vol_remaining > 0 {
        assets::refund(db, sde, outbox, o, party).await
    } else {
        Ok(())
    }
}
fn owned_order_active(o: &Order, now: i64) -> bool {
    o.expires_at.map(i128::from).or_else(|| o.issued.map(|issued|
        i128::from(issued) + i128::from(o.duration) * i128::from(FILETIME_DAY)))
        .is_some_and(|deadline| deadline > i128::from(now))
}

/// `ModifyCharOrder`: reprice an open order, paying the modification fee (and topping up or
/// releasing a buy order's escrow). A price that crosses the book trades at once.
pub(crate) async fn modify_order(identity: &Identity, db: &Db, world: &World, sde: &Arc<crate::sde::Sde>, request: ModifyRequest) -> Result<(), CallError> {
    let me = identity.char_id;
    let new_price = crate::isk::IskAmount::from_isk(valid_price(request.new_price)?)
        .map_err(|_| notify_error(messages::INVALID_PRICE))?;
    let old_price = crate::isk::IskAmount::from_isk(request.old_price)
        .map_err(|_| notify_error(messages::MARKET_ORDER_CLOSED))?;
    let _trading = lock().await;
    let now = filetime_now();
    let identity = *identity;
    db.transaction(|db, outbox: &mut Outbox<MarketEffect>| {
    let sde = Arc::clone(&sde);
    Box::pin(async move {
    let mut o = my_order(db, me, request.order_id).await?;
    if o.scope != request.requested_scope { return Err(notify_error(messages::MARKET_ORDER_CLOSED)); }
    let party = authorization::authorize_order(db, &identity, &o).await?;
    if o.bid != request.bid || o.at.id != request.station_id || o.at.system != request.system_id
        || o.price != old_price || o.range != request.range || o.vol_remaining != request.remaining
        || o.issued.unwrap_or(now / FILETIME_DAY * FILETIME_DAY) != request.issued {
        return Err(notify_error(messages::MARKET_ORDER_CLOSED));
    }
    if !owned_order_active(&o, now) {
        return Err(notify_error(messages::MARKET_ORDER_CLOSED));
    }
    if o.price == new_price {
        return Ok(());
    }
    let delay = crate::config::game().market.modification_delay_seconds;
    if delay > 0 && o.issued.is_some_and(|issued| {
        i128::from(now) - i128::from(issued) < i128::from(delay) * i128::from(infinity_core::consts::FILETIME_SECOND)
    }) {
        return Err(notify_error(messages::market::ORDER_MODIFICATION_COOLDOWN));
    }
    let expires = o.duration.checked_mul(FILETIME_DAY).and_then(|v| now.checked_add(v))
        .ok_or_else(|| notify_error(messages::MARKET_ORDER_CLOSED))?;
    let financial_owner = party.financial;
    let skills = skills(db, &sde, me, now).await?;
    if o.scope != orders::MarketScope::GlobalPlex {
        super::fees::check_remote(&identity, &sde, &o.at, skills.daytrading)?;
    }
    let owner = station(&sde, o.at.id)?.owner;
    let broker_rate = station_broker_rate(db, &sde, me, owner, skills.broker_fee).await?;
    let invalid = || notify_error(messages::INVALID_PRICE);
    let old_amount = o.price.checked_mul(o.vol_remaining).ok_or_else(invalid)?;
    let new_amount = new_price.checked_mul(o.vol_remaining).ok_or_else(invalid)?;
    let fee = super::fees::modification_fee(broker_rate, old_amount, new_amount, skills.modification_discount)?;
    let escrow = if o.bid { new_amount } else { crate::isk::IskAmount::ZERO };
    let top_up = escrow.checked_sub(o.escrow).ok_or_else(invalid)?;
    let charge = top_up.max(crate::isk::IskAmount::ZERO);
    let total = fee.checked_add(charge).ok_or_else(invalid)?;
    journal::debit_order_charge(db, outbox, financial_owner, total, charge, &journal_entry(me, REF_MARKET_ESCROW, financial_owner.id, owner, Some(o.id), None, now, 0)).await?;
    let release = o.escrow.checked_sub(escrow).ok_or_else(invalid)?.max(crate::isk::IskAmount::ZERO);
    journal::credit(db, outbox, financial_owner, release, &journal_entry(me, REF_MARKET_ESCROW, financial_owner.id, OWNER_SCC, Some(o.id), None, now, 0)).await?;
    if trading_dao::reprice(db, &o, new_price, escrow, now).await.map_err(db_err)? != 1 {
        return Err(notify_error(messages::MARKET_ORDER_CLOSED));
    }
    o.price = new_price;
    o.escrow = escrow;
    o.issued = Some(now);
    o.expires_at = Some(expires);
    outbox.operation(MarketEffect::Repriced { owner: me, order: o.id, price: new_price.wire_isk() });
    let traded = if o.bid {
        let mut seller_taxes = HashMap::from([(me, skills.sales_tax)]);
        let (bought, spent) = lift_asks(db, outbox, &sde, party, &o, false, &mut seller_taxes, now).await?;
        let final_escrow = o.price.checked_mul(o.vol_remaining - bought).ok_or_else(invalid)?;
        let improvement = escrow.checked_sub(spent).and_then(|v| v.checked_sub(final_escrow)).ok_or_else(invalid)?;
        journal::credit(db, outbox, financial_owner, improvement, &journal_entry(me, REF_MARKET_ESCROW, financial_owner.id, OWNER_SCC, Some(o.id), None, now, 0)).await?;
        bought
    } else {
        hit_bids(db, outbox, &sde, party, &o, skills.sales_tax, now).await?
    };
    if traded > 0 {
        consume(db, outbox, &mut o, traded, now).await?;
    } else {
        tell_owner(db, outbox, &o, "Modify").await?;
    }
    Ok(())
    }) }, |value, outbox| async move { dispatch_market(value, outbox, world) }).await
}

/// `CancelCharOrder`: close an open order and hand back its escrow (the broker's fee stays paid).
pub(crate) async fn cancel_order(identity: &Identity, db: &Db, world: &World, sde: &Arc<crate::sde::Sde>, order_id: i64, region_id: i64) -> Result<(), CallError> {
    let me = identity.char_id;
    let _trading = lock().await;
    let now = filetime_now();
    let identity = *identity;
    db.transaction(|db, outbox: &mut Outbox<MarketEffect>| { let sde = sde.clone(); Box::pin(async move {
        let sde = &sde;
        let o = my_order(db, me, order_id).await?;
        authorization::authorize_order(db, &identity, &o).await?;
        if o.scope != orders::MarketScope::GlobalPlex && o.at.region != region_id { return Err(notify_error(messages::MARKET_ORDER_CLOSED)); }
        close(db, &o, STATE_CANCELLED, orders::CloseReason::Cancelled, now).await?;
        refund(db, sde, outbox, &o, now).await?;
        outbox.operation(MarketEffect::Cancelled { owner: me, order: o.id });
        tell_owner(db, outbox, &o, "Cancel").await?;
        Ok(())
    })}, |value, outbox| async move { dispatch_market(value, outbox, world) }).await
}

/// Execute one scheduler-driven expiry scan.
pub(crate) fn expire_orders(db: Db, world: Arc<World>, _: crate::scheduler::Job) -> std::pin::Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send>> {
    Box::pin(async move {
        expire(&db, world.sde(), &world).await.map_err(crate::startup_call_error)
    })
}

async fn expire(db: &Db, sde: &Arc<crate::sde::Sde>, world: &World) -> Result<(), CallError> {
    let _trading = lock().await;
    let now = filetime_now();
    db.transaction(|db, outbox: &mut Outbox<MarketEffect>| { let sde = sde.clone(); Box::pin(async move {
        let sde = &sde;
        let rows = orders::expired(db, FILETIME_DAY, now).await.map_err(db_err)?;
        for o in rows {
            close(db, &o, STATE_CLOSED, orders::CloseReason::Expired, now).await?;
            refund(db, sde, outbox, &o, now).await?;
            outbox.operation(MarketEffect::Expired { owner: o.char_id, order: o.id });
            tell_owner(db, outbox, &o, "Expiry").await?;
        }
        Ok(())
    })}, |value, outbox| async move { dispatch_market(value, outbox, world) }).await?;
    Ok(())
}

#[cfg(test)]
mod tests;
