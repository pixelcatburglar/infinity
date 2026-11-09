//! Market matching and placement for `eve/client/script/ui/services/marketsvc.py`.
//! Uses game market orders, wallets and inventory with loaded SDE data. Personal and
//! corporate orders share the NPC-station book; equal financial accounts cannot match.
//! NPC liquidity is unlimited. Admission requires a tradable SDE market type. Immediate
//! trades accept any whole-cent limit; orders that can rest, and repricing, need a client
//! price tick. PLEX uses a global book and matching uses exact ISK; structure markets
//! are unsupported.
use super::*;
use super::fees::{broker_fee, check_order_slots, skills, station_broker_rate};
use infinity_core::consts::REF_MARKET_SECURITY_TAX;
use crate::dao::market::trading as trading_dao;
use crate::db::outbox::SourceKey;
use crate::services::identity::Identity;
use crate::services::items::take_items;
use crate::services::wallets::ownership::debit;
use crate::services::user_error;
use crate::sde::Sde;
use std::sync::Arc;
/// `security/common/const.py` MARKET_SECURITY_TAX_RATE = 0.05; `sellMulti.py:370`
/// and `buyThisTypeWindow.py:908` use it for high-security non-immediate orders.
const SECURITY_TAX_NUMERATOR: u128 = 1;
const SECURITY_TAX_DENOMINATOR: u128 = 20;

async fn bound_user(db: &(impl Executor + ?Sized), party: TradeParty, scope: orders::MarketScope) -> Result<Option<i64>, CallError> {
    if scope == orders::MarketScope::GlobalPlex && party.financial.id == party.issuer {
        return Ok(Some(crate::dao::characters::persistence::user_for_character(db, party.issuer).await.map_err(db_err)?
            .ok_or_else(|| user_error(messages::MSG_MKT_ORDER_DID_NOT_MATCH, vec![]))?));
    }
    Ok(None)
}

async fn security_tax(db: &(impl Executor + ?Sized), outbox: &mut Outbox<MarketEffect>, sde: &Sde, system: Option<i64>, party: TradeParty, order: &Order, notional: crate::isk::IskAmount, now: i64) -> Result<(), CallError> {
    if system.and_then(|id| sde.map.system(id)).and_then(|s| s.security)
        .is_some_and(|security| security >= crate::config::game().crimewatch.highsec_threshold) {
        let tax = notional.checked_mul_ratio(SECURITY_TAX_NUMERATOR, SECURITY_TAX_DENOMINATOR).ok_or(crate::isk::AmountError)?;
        journal::debit(db, outbox, party.financial, tax, &journal_entry(party.issuer, REF_MARKET_SECURITY_TAX, party.financial.id, order.at.owner, Some(order.id), None, now, 0)).await?;
    }
    Ok(())
}

async fn finish_placement(db: &(impl Executor + ?Sized), outbox: &mut Outbox<MarketEffect>, order: &mut Order, now: i64) -> Result<(), CallError> {
    let active = order.duration > 0 && order.vol_remaining > 0;
    if active {
        insert_order(db, order).await?;
    } else {
        order.expires_at = Some(now);
        trading_dao::archive_immediate(db, order, STATE_CLOSED, orders::CloseReason::Filled, now).await.map_err(db_err)?;
    }
    let issuer = order.char_id.ok_or_else(|| notify_error(messages::MARKET_ORDER_CLOSED))?;
    outbox.event_from(SourceKey::new("MarketOrderPlaced", order.id), crate::lua::events::GameEvent::MarketOrderPlaced {
        char_id: issuer, order_id: order.id, type_id: order.type_id, quantity: order.vol_entered, price: order.price, is_buy: order.bid,
    });
    outbox.operation(MarketEffect::Placed { buyer: order.bid, owner: issuer, order: order.id, quantity: order.vol_entered, type_id: order.type_id, price: order.price.wire_isk() });
    tell_owner(db, outbox, order, if active { "Add" } else { "Expiry" }).await
}

async fn record(db: &(impl Executor + ?Sized), buy: &Order, sell: &Order, resting: &Order, quantity: i64, buyer: i64, seller: i64, buyer_account_id: Option<i64>, seller_account_id: Option<i64>, at: &Station, now: i64) -> Result<i64, CallError> {
    trading_dao::record(db, &trading_dao::NewTransaction {
        type_id: resting.type_id, quantity, price: resting.price, buyer, seller,
        station: at.id, region: at.region, date: now, buyer_account_id, seller_account_id,
        order_id: Some(resting.id), scope: resting.scope, buy_order_id: Some(buy.id),
        sell_order_id: Some(sell.id), buyer_vault_user_id: buy.vault_user_id,
        seller_vault_user_id: sell.vault_user_id,
    }).await.map_err(db_err)
}

/// `quantity` of a resting order traded: a player's order shrinks, and closes once filled.
pub(super) async fn consume<X: Send>(db: &(impl Executor + ?Sized), outbox: &mut Outbox<X>, o: &mut Order, quantity: i64, now: i64) -> Result<(), CallError> {
    if TradeParty::from_order(o).is_none() { return Ok(()); }
    let remaining = o.vol_remaining.checked_sub(quantity).filter(|&v| quantity > 0 && v >= 0)
        .ok_or_else(|| notify_error(messages::MARKET_ORDER_CLOSED))?;
    let escrow = if o.bid {
        o.price.checked_mul(remaining).ok_or(crate::isk::AmountError)?
    } else {
        crate::isk::IskAmount::ZERO
    };
    let plex_escrow = if !o.bid && o.scope == orders::MarketScope::GlobalPlex { remaining } else { 0 };
    let changed = if remaining == 0 {
        trading_dao::close(db, o, remaining, escrow, plex_escrow, STATE_CLOSED, orders::CloseReason::Filled, now).await.map_err(db_err)?
    } else {
        trading_dao::consume(db, o, remaining, escrow, plex_escrow).await.map_err(db_err)?
    };
    if changed != 1 { return Err(notify_error(messages::MARKET_ORDER_CLOSED)); }
    o.vol_remaining = remaining;
    o.escrow = escrow;
    o.plex_escrow = plex_escrow;
    tell_owner(db, outbox, o, "Modify").await
}

/// Sell the incoming order to eligible bids, at each resting bid's price.
pub(super) async fn hit_bids(db: &(impl Executor + ?Sized), outbox: &mut Outbox<MarketEffect>, sde: &Arc<Sde>, party: TradeParty, incoming: &Order, tax: fees::FeeRate, now: i64) -> Result<i64, CallError> {
    let seller = party.issuer;
    let seller_owner = party.financial;
    let rows = orders::bids(db, now, FILETIME_DAY, incoming.type_id, incoming.at.region, incoming.price).await.map_err(db_err)?;
    let mut remaining = incoming.vol_remaining;
    let mut plan = Vec::new();
    for mut bid in rows {
        if remaining == 0 { break; }
        if TradeParty::from_order(&bid).is_some_and(|other| party.same_account(other)) { continue; }
        if incoming.scope != orders::MarketScope::GlobalPlex && !in_range(&sde.map, bid.range, &bid.at, &incoming.at) { continue; }
        bid.at.owner = sde.map.station(bid.at.id).and_then(|s| s.owner_id).unwrap_or(0);
        let available = if bid.char_id.is_some() { bid.vol_remaining } else { remaining };
        let q = remaining.min(available);
        if q < bid.min_volume && available >= bid.min_volume { continue; }
        remaining -= q;
        plan.push((bid, q));
    }
    let mut consumed = Vec::new();
    for (bid, q) in plan {
        let gross = bid.price.checked_mul(q).ok_or(crate::isk::AmountError)?;
        let net = gross.checked_sub(fees::apply_rate(gross, tax)?).ok_or(crate::isk::AmountError)?;
        let buyer = bid.char_id.unwrap_or(bid.at.owner);
        let buyer_party = TradeParty::from_order(&bid);
        let buyer_owner = buyer_party.map_or(bid.at.owner, |other| other.financial.id);
        let seller_account = accounts::ensure(db, seller_owner.id, seller_owner.wallet_key).await.map_err(db_err)?.id;
        let buyer_account = match buyer_party {
            Some(other) => Some(accounts::ensure(db, other.financial.id, other.financial.wallet_key).await.map_err(db_err)?.id),
            None => None,
        };
        let fill = record(db, &bid, incoming, &bid, q, buyer, seller, buyer_account, Some(seller_account), &incoming.at, now).await?;
        journal::credit_sale(db, outbox, seller_owner, seller_account, gross, net, &journal_entry(seller, REF_MARKET_TRANSACTION, buyer_owner, seller_owner.id, Some(bid.id), Some(fill), now, 1)).await?;
        if let Some(buyer_party) = buyer_party {
            assets::deliver(db, sde, outbox, buyer_party, incoming.at.id, incoming.type_id, q, bid.vault_user_id, bid.id, fill).await?;
        }
        outbox.event_from(SourceKey::new("MarketOrderFilled", fill), crate::lua::events::GameEvent::MarketOrderFilled { order_id: bid.id, buyer_id: buyer, seller_id: seller, type_id: incoming.type_id, quantity: q, price: bid.price });
        outbox.operation(MarketEffect::Sold { seller, quantity: q, type_id: incoming.type_id, station: incoming.at.id, value: gross.wire_isk(), order: bid.id });
        consumed.push((bid, q));
    }
    consumed.sort_unstable_by_key(|(order, _)| order.id);
    for (mut order, q) in consumed {
        consume(db, outbox, &mut order, q, now).await?;
    }
    Ok(incoming.vol_remaining - remaining)
}

/// Buy from eligible asks, returning quantity filled and exact total spent.
#[allow(clippy::too_many_arguments)]
pub(super) async fn lift_asks(db: &(impl Executor + ?Sized), outbox: &mut Outbox<MarketEffect>, sde: &Arc<Sde>, party: TradeParty, incoming: &Order, from_wallet: bool, seller_taxes: &mut HashMap<i64, fees::FeeRate>, now: i64) -> Result<(i64, crate::isk::IskAmount), CallError> {
    let buyer = party.issuer;
    let buyer_owner = party.financial;
    let quantity = incoming.vol_remaining;
    let rows = orders::asks(db, now, FILETIME_DAY, incoming.type_id, incoming.at.region, incoming.price).await.map_err(db_err)?;
    let mut remaining = quantity;
    let mut plan = Vec::new();
    for mut ask in rows {
        if remaining == 0 { break; }
        if incoming.scope != orders::MarketScope::GlobalPlex && !in_range(&sde.map, incoming.range, &incoming.at, &ask.at) { continue; }
        if TradeParty::from_order(&ask).is_some_and(|other| party.same_account(other)) { continue; }
        ask.at.owner = sde.map.station(ask.at.id).and_then(|s| s.owner_id).unwrap_or(0);
        let available = if ask.char_id.is_some() { ask.vol_remaining } else { remaining };
        let q = remaining.min(available);
        if q < ask.min_volume && available >= ask.min_volume { continue; }
        remaining -= q;
        plan.push((ask, q));
    }
    let mut left = quantity;
    let mut spent = crate::isk::IskAmount::ZERO;
    let mut consumed = Vec::new();
    for (ask, q) in plan {
        let gross = ask.price.checked_mul(q).ok_or(crate::isk::AmountError)?;
        if from_wallet {
            match debit(db, outbox, buyer_owner, gross).await {
                Ok(()) => {}
                Err(CallError::User { .. }) if left < quantity => break,
                Err(error) => return Err(error),
            }
        }
        let seller = ask.char_id.unwrap_or(ask.at.owner);
        let seller_party = TradeParty::from_order(&ask);
        let seller_owner = seller_party.map_or(ask.at.owner, |other| other.financial.id);
        let buyer_account = accounts::ensure(db, buyer_owner.id, buyer_owner.wallet_key).await.map_err(db_err)?.id;
        let seller_account = match seller_party {
            Some(other) => Some(accounts::ensure(db, other.financial.id, other.financial.wallet_key).await.map_err(db_err)?.id),
            None => None,
        };
        let fill = record(db, incoming, &ask, &ask, q, buyer, seller, Some(buyer_account), seller_account, &ask.at, now).await?;
        if from_wallet {
            crate::dao::wallets::journal::append(db, buyer_account, gross.checked_neg().ok_or(crate::isk::AmountError)?, &journal_entry(buyer, REF_MARKET_TRANSACTION, buyer_owner.id, seller_owner, Some(ask.id), Some(fill), now, 0)).await.map_err(db_err)?;
        }
        if let Some(seller_party) = seller_party {
            let seller_account = seller_account.ok_or_else(|| CallError::Internal(anyhow::anyhow!("missing seller wallet account")))?;
            let tax = match seller_taxes.get(&seller) {
                Some(&tax) => tax,
                None => {
                    let tax = skills(db, sde, seller, now).await?.sales_tax;
                    seller_taxes.insert(seller, tax);
                    tax
                }
            };
            let net = gross.checked_sub(fees::apply_rate(gross, tax)?).ok_or(crate::isk::AmountError)?;
            journal::credit_sale(db, outbox, seller_party.financial, seller_account, gross, net, &journal_entry(seller, REF_MARKET_TRANSACTION, buyer_owner.id, seller_owner, Some(ask.id), Some(fill), now, 1)).await?;
        }
        assets::deliver(db, sde, outbox, party, ask.at.id, incoming.type_id, q, incoming.vault_user_id, incoming.id, fill).await?;
        outbox.event_from(SourceKey::new("MarketOrderFilled", fill), crate::lua::events::GameEvent::MarketOrderFilled { order_id: ask.id, buyer_id: buyer, seller_id: seller, type_id: incoming.type_id, quantity: q, price: ask.price });
        outbox.operation(MarketEffect::Bought { buyer, quantity: q, type_id: incoming.type_id, station: ask.at.id, value: gross.wire_isk(), order: ask.id });
        left -= q;
        spent = spent.checked_add(gross).ok_or(crate::isk::AmountError)?;
        consumed.push((ask, q));
    }
    consumed.sort_unstable_by_key(|(order, _)| order.id);
    for (mut order, q) in consumed {
        consume(db, outbox, &mut order, q, now).await?;
    }
    Ok((quantity - left, spent))
}

/// `buyMultiFromBase.py:IsBuyable`: an order type must be in the market and tradable.
fn admit_type(sde: &Sde, type_id: i64) -> Result<(), CallError> {
    let id = i32::try_from(type_id).map_err(|_| user_error(messages::MSG_MKT_ORDER_DID_NOT_MATCH, vec![]))?;
    if !sde.types.get(id).is_some_and(|ty| ty.market_group_id.is_some()) || !sde.is_tradable(id) {
        return Err(user_error(messages::MSG_MKT_ORDER_DID_NOT_MATCH, vec![]));
    }
    Ok(())
}

/// Place one buy under the shared economy lock and transaction.
pub(crate) async fn place_buy(identity: &Identity, db: &Db, world: &World, sde: &Arc<Sde>, request: BuyRequest, corp_wallet: Option<i32>) -> Result<Order, CallError> {
    validate_buy(request)?;
    admit_type(sde, request.type_id)?;
    let _trading = lock().await;
    let now = filetime_now();
    let identity = *identity;
    db.transaction(|ex, outbox: &mut Outbox<MarketEffect>| {
        let sde = Arc::clone(sde);
        Box::pin(async move {
            let party = authorization::authorize_trade(ex, &identity, corp_wallet).await?;
            let skills = skills(ex, &sde, party.issuer, now).await?;
            let mut seller_taxes = HashMap::from([(party.issuer, skills.sales_tax)]);
            place_buy_inner(&identity, ex, outbox, &sde, request, party, &skills, &mut seller_taxes, now, false).await
        })
    }, |value, outbox| async move { dispatch_market(value, outbox, world) }).await
}

/// The bounded batch owns one lock and transaction; a refused child rolls back all children.
pub(super) async fn place_buy_batch(identity: &Identity, db: &Db, world: &World, sde: &Arc<Sde>, requests: Vec<BuyRequest>, corp_wallet: Option<i32>) -> Result<Vec<Order>, CallError> {
    for &request in &requests { validate_buy(request)?; }
    for request in &requests { admit_type(sde, request.type_id)?; }
    let _trading = lock().await;
    let now = filetime_now();
    let identity = *identity;
    let requests: Arc<[BuyRequest]> = requests.into();
    db.transaction(|ex, outbox: &mut Outbox<MarketEffect>| {
        let sde = Arc::clone(sde);
        let requests = Arc::clone(&requests);
        Box::pin(async move {
            let party = authorization::authorize_trade(ex, &identity, corp_wallet).await?;
            let skills = skills(ex, &sde, party.issuer, now).await?;
            let mut seller_taxes = HashMap::from([(party.issuer, skills.sales_tax)]);
            let mut result = Vec::with_capacity(requests.len());
            for &request in requests.iter() {
                result.push(place_buy_inner(&identity, ex, outbox, &sde, request, party, &skills, &mut seller_taxes, now, true).await?);
            }
            Ok(result)
        })
    }, |value, outbox| async move { dispatch_market(value, outbox, world) }).await
}

/// Immediate trades (`duration == 0`) take the client's whole-cent limit as entered; an order that
/// can rest must also sit on a `marketutil/ticks.py` price tick.
fn placement_price(price: f64, duration: i64) -> Result<(), CallError> {
    let valid = if duration > 0 { super::fees::valid_tick(price) } else { super::fees::valid_cents(price) };
    if valid { Ok(()) } else { Err(notify_error(messages::INVALID_PRICE)) }
}

pub(super) fn validate_buy(request: BuyRequest) -> Result<(), CallError> {
    placement_price(request.price, request.duration)?;
    if request.station_id <= 0 || request.type_id <= 0 || i32::try_from(request.type_id).is_err()
        || request.quantity <= 0
        || request.min_volume <= 0 || request.min_volume > request.quantity
        || !super::fees::valid_order_range(request.range)
        || !super::fees::valid_order_duration(request.duration, filetime_now())
        || request.expected_brokers_fee.is_some_and(|v| !v.is_finite()) {
        return Err(user_error(messages::MSG_MKT_ORDER_DID_NOT_MATCH, vec![]));
    }
    Ok(())
}

async fn place_buy_inner(identity: &Identity, db: &(impl Executor + ?Sized), outbox: &mut Outbox<MarketEffect>, sde: &Arc<Sde>, request: BuyRequest, party: TradeParty, skills: &super::fees::Skills, seller_taxes: &mut HashMap<i64, fees::FeeRate>, now: i64, accept_empty: bool) -> Result<Order, CallError> {
    let at = station(sde, request.station_id)?;
    let quantity = request.quantity;
    let type_id = i32::try_from(request.type_id).map_err(|_| crate::isk::AmountError)?;
    let scope = orders::MarketScope::for_type(type_id);
    if scope == orders::MarketScope::Regional {
        if request.duration > 0 {
            fees::check_remote(identity, sde, &at, skills.procurement)?;
            if identity.station_id != Some(at.id) && request.range > skills.visibility {
                return Err(user_error(messages::MSG_MKT_ORDER_DID_NOT_MATCH, vec![]));
            }
        } else {
            fees::check_remote(identity, sde, &at, RANGE_REGION)?;
        }
    }
    let rate = station_broker_rate(db, sde, party.issuer, at.owner, skills.broker_fee).await?;
    fees::check_expected_fee(request.expected_brokers_fee, rate)?;
    if request.duration > 0 { check_order_slots(db, party.issuer, skills).await?; }
    let price = crate::isk::IskAmount::from_isk(request.price)?;
    let principal = price.checked_mul(quantity).ok_or(crate::isk::AmountError)?;
    let expires = request.duration.checked_mul(FILETIME_DAY).and_then(|v| now.checked_add(v)).ok_or(crate::isk::AmountError)?;
    let mut order = Order {
        id: orders::next_order_id(db).await.map_err(db_err)?, type_id, char_id: Some(party.issuer),
        corporation_id: (party.financial.id != party.issuer).then_some(party.financial.id),
        wallet_key: party.financial.wallet_key, source_location_id: None, source_flag_id: None, at,
        price, vol_entered: quantity, vol_remaining: quantity,
        min_volume: request.min_volume,
        bid: true, range: request.range, duration: request.duration, issued: Some(now),
        escrow: crate::isk::IskAmount::ZERO, scope, vault_user_id: bound_user(db, party, scope).await?,
        plex_escrow: 0, created_at: Some(now), expires_at: Some(expires),
    };
    if request.duration > 0 {
        let total = principal.checked_add(broker_fee(rate, principal)?).ok_or(crate::isk::AmountError)?;
        journal::debit_order_charge(db, outbox, party.financial, total, principal, &journal_entry(party.issuer, REF_MARKET_ESCROW, party.financial.id, at.owner, Some(order.id), None, now, 0)).await?;
        security_tax(db, outbox, sde, identity.system_id, party, &order, principal, now).await?;
    }
    let (bought, spent) = lift_asks(db, outbox, sde, party, &order, request.duration == 0, seller_taxes, now).await?;
    if request.duration == 0 && bought == 0 && !accept_empty {
        return Err(user_error(messages::MSG_MKT_ORDER_DID_NOT_MATCH, vec![]));
    }
    order.vol_remaining -= bought;
    if request.duration > 0 {
        order.escrow = price.checked_mul(order.vol_remaining).ok_or(crate::isk::AmountError)?;
        let refund = principal.checked_sub(spent).and_then(|v| v.checked_sub(order.escrow)).ok_or(crate::isk::AmountError)?;
        journal::credit(db, outbox, party.financial, refund, &journal_entry(party.issuer, REF_MARKET_ESCROW, party.financial.id, at.owner, Some(order.id), None, now, 0)).await?;
    }
    finish_placement(db, outbox, &mut order, now).await?;
    Ok(order)
}

/// One sell child is independently authorized and committed.
pub(crate) async fn place_sell(identity: &Identity, db: &Db, world: &World, sde: &Arc<Sde>, request: SellRequest, corp_wallet: Option<i32>) -> Result<bool, CallError> {
    validate_sell(request)?;
    admit_type(sde, request.type_id)?;
    let _trading = lock().await;
    let now = filetime_now();
    let identity = *identity;
    db.transaction(|db, outbox: &mut Outbox<MarketEffect>| {
        let sde = Arc::clone(sde);
        Box::pin(async move {
            let party = authorization::authorize_trade(db, &identity, corp_wallet).await?;
            let item = assets::source(db, &sde, party, request.item_id, request.station_id, request.type_id, request.quantity, request.office_id).await?;
            let quantity = request.quantity;
            let type_id = item.type_id;
            let scope = orders::MarketScope::for_type(type_id);
            let at = station(&sde, request.station_id)?;
            let skills = skills(db, &sde, party.issuer, now).await?;
            if scope == orders::MarketScope::Regional {
                fees::check_remote(&identity, &sde, &at, skills.marketing)?;
            }
            let rate = station_broker_rate(db, &sde, party.issuer, at.owner, skills.broker_fee).await?;
            fees::check_expected_fee(request.expected_brokers_fee, rate)?;
            if request.duration > 0 { check_order_slots(db, party.issuer, &skills).await?; }
            let price = crate::isk::IskAmount::from_isk(request.price)?;
            let notional = price.checked_mul(quantity).ok_or(crate::isk::AmountError)?;
            let expires = request.duration.checked_mul(FILETIME_DAY).and_then(|v| now.checked_add(v)).ok_or(crate::isk::AmountError)?;
            let mut order = Order {
                id: orders::next_order_id(db).await.map_err(db_err)?, type_id, char_id: Some(party.issuer),
                corporation_id: corp_wallet.map(|_| party.financial.id), wallet_key: party.financial.wallet_key,
                source_location_id: corp_wallet.map(|_| item.location_id), source_flag_id: corp_wallet.map(|_| item.flag_id), at,
                price, vol_entered: quantity, vol_remaining: quantity, min_volume: 1,
                bid: false, range: RANGE_STATION, duration: request.duration, issued: Some(now),
                escrow: crate::isk::IskAmount::ZERO, scope, vault_user_id: bound_user(db, party, scope).await?,
                plex_escrow: 0, created_at: Some(now), expires_at: Some(expires),
            };
            if request.duration > 0 {
                journal::debit(db, outbox, party.financial, broker_fee(rate, notional)?, &journal_entry(party.issuer, REF_BROKER_FEE, party.financial.id, at.owner, Some(order.id), None, now, 0)).await?;
                security_tax(db, outbox, &sde, Some(at.system), party, &order, notional, now).await?;
                take_items(db, outbox, item.clone(), quantity).await?;
            }
            let sold = hit_bids(db, outbox, &sde, party, &order, skills.sales_tax, now).await?;
            if request.duration == 0 {
                if sold == 0 { return Err(user_error(messages::MSG_MKT_ORDER_DID_NOT_MATCH, vec![])); }
                take_items(db, outbox, item, sold).await?;
            }
            order.vol_remaining -= sold;
            if request.duration > 0 && scope == orders::MarketScope::GlobalPlex {
                order.plex_escrow = order.vol_remaining;
            }
            finish_placement(db, outbox, &mut order, now).await?;
            Ok(true)
        })
    }, |value, outbox| async move { dispatch_market(value, outbox, world) }).await
}

pub(super) fn validate_sell(request: SellRequest) -> Result<(), CallError> {
    placement_price(request.price, request.duration)?;
    if request.item_id <= 0 || request.station_id <= 0 || request.type_id <= 0 || i32::try_from(request.type_id).is_err()
        || request.quantity <= 0 || !super::fees::valid_order_duration(request.duration, filetime_now())
        || request.office_id.is_some_and(|id| id <= 0) || request.expected_brokers_fee.is_some_and(|v| !v.is_finite()) {
        return Err(user_error(messages::MSG_MKT_ORDER_DID_NOT_MATCH, vec![]));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
