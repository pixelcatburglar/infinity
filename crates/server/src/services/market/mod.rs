//! marketProxy: regional order books (eve/client/script/ui/services/marketsvc.py).
//!
//! The book holds the NPC orders seeded at the starter stations (sql/market_seed.sql, and
//! region-wide buy orders priced at the configured NPC buy ratio) and players' orders. NPC
//! stock and demand are unlimited, so fills never consume NPC orders.
//!
//! A trade happens at the resting order's price and delivers to the buyer's hangar in the
//! seller's station. The seller pays sales tax and an order that rests pays the broker's
//! fee, as marketutil computes them (skilllimits, brokerFee), using raw NPC station owner
//! corporation and faction standings from game.standings. Faction-war upgrades are
//! unsupported (upgrade level zero); structure markets are unsupported. A sell
//! order holds its items and a buy order its ISK in escrow until it fills, is cancelled or
//! expires; closed orders move to game.market_order_history, and every fill is a row of
//! game.market_transactions (wallet transactions and price history). Ranges in jumps use
//! the startup-loaded SDE map. Fees, tax, duration, history and maintenance cadence are
//! configured market policies. The SQL startup seed is the static-data exception.
//! Order expiry scans run as durable periodic jobs registered through the services scheduler
//! and persisted in game.jobs, not detached maintenance sleeps. Internal scan errors warn
//! but the scan still completes.
//! Committed fills and newly resting orders queue Lua events.

use crate::dao::items::loading::ItemQuery;
use super::items::load_items;
use crate::dao::market::trading;
use crate::dao::market::orders::{self, Order, Station};
use crate::dao::market::listings::{self, Area};
use super::wallets::{balance, credit, debit, not_enough_money};
use super::identity::Identity;
use super::{crowset, db_err, keyval, notify_error, user_error, value_field, Call, CallError, CallResult};
use crate::client::{filetime_now, Client};
use crate::consts::{ACCOUNT_CASH, FILETIME_DAY, FLAG_HANGAR, SKILL_ACCOUNTING, SKILL_BROKER_RELATIONS};
use crate::db::Db;
use crate::db::executor::Executor;
use crate::db::outbox::{Effect, Outbox};
use crate::messages;
use crate::sde::Sde;
use crate::space::World;
use marshal::{DbRowDescriptor, DbType, Value};
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::sync::{Arc, OnceLock};
use tracing::{info, warn};
mod lists;
use lists::{best_asks, escrow, history_header, my_orders, order_history, orders, price_history, transactions};


const RANGE_STATION: i64 = -1;
const RANGE_SOLAR_SYSTEM: i64 = 0;
const RANGE_REGION: i64 = 32767;
/// marketutil.const.MAX_ORDER_PRICE
const MAX_ORDER_PRICE: f64 = 9_223_372_036_854.0;
/// What the client shows for a system it can't route to.
const UNREACHABLE: i64 = 1_000_000;

// marketutil.const, appConst.mktMinimumFee and MKT_TRANSACTION_TAX_DEFAULT
const BROKER_RELATIONS_PER_LEVEL: f64 = 0.003;
const ACCOUNTING_PER_LEVEL: f64 = 0.11;
/// `marketutil/const.py:15`: faction standing contribution to broker rate.
const MARKET_FACTION_STANDING_MULTIPLIER: f64 = 0.0003;
/// `marketutil/const.py:16`: NPC corporation standing contribution to broker rate.
const MARKET_NPC_CORP_STANDING_MULTIPLIER: f64 = 0.0002;

// marketHistory orderState
const STATE_CLOSED: i32 = 2; // filled or expired
const STATE_CANCELLED: i32 = 3;

const SKILL_TRADE: i32 = 3443;
const SKILL_RETAIL: i32 = 3444;
const SKILL_WHOLESALE: i32 = 16596;
const SKILL_MARGIN_TRADING: i32 = 16597;
const SKILL_TYCOON: i32 = 18580;

enum MarketEffect {
    Bought { buyer: i64, quantity: i32, type_id: i32, station: i64, value: f64, order: i64 },
    Sold { seller: i64, quantity: i32, type_id: i32, station: i64, value: f64, order: i64 },
    Placed { buyer: bool, owner: i64, order: i64, quantity: i32, type_id: i32, price: f64 },
    Repriced { owner: i64, order: i64, price: f64 },
    Cancelled { owner: i64, order: i64 },
    Expired { owner: Option<i64>, order: i64 },
}

fn dispatch_market<T>(value: T, outbox: Outbox<MarketEffect>, world: &World) -> Result<T, CallError> {
    for effect in outbox.into_entries() {
        match effect {
            Effect::Notification(notification) => notification.dispatch(world, None)?,
            Effect::Event(event) => crate::lua::on_event(event),
            Effect::Operation(MarketEffect::Bought { buyer, quantity, type_id, station, value, order }) =>
                info!("market: {buyer} bought {quantity} x {type_id} at {station} for {value} ISK (order {order})"),
            Effect::Operation(MarketEffect::Sold { seller, quantity, type_id, station, value, order }) =>
                info!("market: {seller} sold {quantity} x {type_id} at {station} for {value} ISK (order {order})"),
            Effect::Operation(MarketEffect::Placed { buyer, owner, order, quantity, type_id, price }) => {
                let side = if buyer { "buy" } else { "sell" };
                info!("market: {owner} placed {side} order {order} for {quantity} x {type_id} at {price} ISK");
            }
            Effect::Operation(MarketEffect::Repriced { owner, order, price }) => info!("market: {owner} repriced order {order} to {price} ISK"),
            Effect::Operation(MarketEffect::Cancelled { owner, order }) => info!("market: {owner} cancelled order {order}"),
            Effect::Operation(MarketEffect::Expired { owner, order }) => info!("market: order {order} of {owner:?} expired"),
        }
    }
    Ok(value)
}

pub(super) fn round2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

fn valid_price(price: f64) -> Result<f64, CallError> {
    if price.is_finite() && (0.01..=MAX_ORDER_PRICE).contains(&price) {
        Ok(round2(price))
    } else {
        Err(notify_error(messages::INVALID_PRICE))
    }
}

fn valid_duration(duration: i64, now: i64) -> bool {
    (0..=crate::config::game().market.max_order_days).contains(&duration)
        && duration <= i32::MAX as i64
        && duration.checked_mul(FILETIME_DAY).and_then(|days| now.checked_add(days)).is_some()
}



fn today() -> i64 {
    filetime_now() / FILETIME_DAY * FILETIME_DAY
}


// ---------------------------------------------------------------- trading

/// Trades run one at a time, so each sees the book as the last one left it.
pub(crate) async fn lock() -> tokio::sync::MutexGuard<'static, ()> {
    static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(Default::default).lock().await
}


pub(super) fn station(sde: &crate::sde::Sde, id: i64) -> Result<Station, CallError> {
    let r = sde.map.station(id).ok_or_else(|| notify_error(messages::NO_MARKET_AT_LOCATION))?;
    Ok(market_station(r))
}

fn market_station(r: &crate::sde::map::Station) -> Station {
    Station { id: r.station_id, system: r.solar_system_id.expect("station solar system is null"), constellation: r.constellation_id.expect("station constellation is null"), region: r.region_id.expect("station region is null"), owner: r.owner_id.unwrap_or(0) }
}

/// Stargate jumps from `from` to `to`, looking no further than `max`.
pub(super) fn jumps(g: &crate::sde::map::Map, from: i64, to: i64, max: i64) -> Option<i64> {
    if from == to {
        return Some(0);
    }
    let mut seen = HashSet::from([from]);
    let mut frontier = vec![from];
    for d in 1..=max {
        let mut next = vec![];
        for s in frontier {
            for &n in g.jumps(s) {
                if n == to {
                    return Some(d);
                }
                if seen.insert(n) {
                    next.push(n);
                }
            }
        }
        if next.is_empty() {
            break;
        }
        frontier = next;
    }
    None
}

/// May an order of `range` placed at `at` trade with something at `other`?
fn in_range(g: &crate::sde::map::Map, range: i64, at: &Station, other: &Station) -> bool {
    match range {
        RANGE_STATION => at.id == other.id,
        RANGE_SOLAR_SYSTEM => at.system == other.system,
        r if r >= RANGE_REGION => at.region == other.region,
        r => at.region == other.region && jumps(g, at.system, other.system, r).is_some(),
    }
}

/// What a character's trade skills make of fees, taxes and order slots (marketutil.skilllimits).
struct Skills {
    order_cap: i64,
    sales_tax: f64,
    broker_fee: f64,
    modification_discount: f64,
}

async fn skills(db: &(impl Executor + ?Sized), char_id: i64) -> Result<Skills, CallError> {
    let wanted = vec![SKILL_TRADE, SKILL_RETAIL, SKILL_BROKER_RELATIONS, SKILL_WHOLESALE, SKILL_MARGIN_TRADING, SKILL_ACCOUNTING, SKILL_TYCOON];
    let rows = crate::dao::skills::levels::levels_of(db, char_id, &wanted).await.map_err(db_err)?;
    let levels: HashMap<i32, i32> = rows.iter().map(|r| (r.type_id, r.level)).collect();
    let l = |skill: i32| levels.get(&skill).copied().unwrap_or(0) as f64;
    Ok(Skills {
        order_cap: (5.0 + 4.0 * l(SKILL_TRADE) + 8.0 * l(SKILL_RETAIL) + 16.0 * l(SKILL_WHOLESALE) + 32.0 * l(SKILL_TYCOON)) as i64,
        sales_tax: crate::config::game().market.sales_tax * (1.0 - ACCOUNTING_PER_LEVEL * l(SKILL_ACCOUNTING)),
        broker_fee: crate::config::game().market.broker_fee - BROKER_RELATIONS_PER_LEVEL * l(SKILL_BROKER_RELATIONS),
        modification_discount: 0.5 + 0.06 * l(SKILL_MARGIN_TRADING),
    })
}
/// `marketutil/brokerFee.py:63-70`; faction-war upgrade level zero is identity.
fn adjusted_broker_rate(commission: f64, faction: f64, corp: f64) -> f64 {
    commission - faction * MARKET_FACTION_STANDING_MULTIPLIER - corp * MARKET_NPC_CORP_STANDING_MULTIPLIER
}

async fn station_broker_rate(ex: &(impl Executor + ?Sized), sde: &crate::sde::Sde, char_id: i64, owner: i64, commission: f64) -> Result<f64, CallError> {
    let Some(corp) = sde.npc_corporations.get(owner) else { return Ok(commission) };
    let corp_standing = super::standings::standing(ex, char_id, owner).await?;
    let faction_standing = match corp.faction_id {
        Some(faction) => super::standings::standing(ex, char_id, faction).await?,
        None => 0.0,
    };
    Ok(adjusted_broker_rate(commission, faction_standing, corp_standing))
}


fn broker_fee(rate: f64, amount: f64) -> f64 {
    round2((rate * amount).max(crate::config::game().market.broker_fee_min_isk))
}

async fn check_order_slots(db: &(impl Executor + ?Sized), char_id: i64, skills: &Skills) -> Result<(), CallError> {
    let open = trading::order_count(db, char_id).await.map_err(db_err)?;
    if open >= skills.order_cap {
        return Err(notify_error(&messages::market_order_limit_message(open)));
    }
    Ok(())
}


async fn require_balance(db: &(impl Executor + ?Sized), char_id: i64, amount: f64) -> Result<(), CallError> {
    if !crate::dao::wallets::characters::covers(db, char_id, amount).await.map_err(db_err)? {
        return Err(not_enough_money(balance(db, char_id).await?, amount));
    }
    Ok(())
}


use super::items::{deliver, take_items};

async fn record(db: &(impl Executor + ?Sized), type_id: i32, quantity: i32, price: f64, buyer: i64, seller: i64, at: &Station, now: i64) -> Result<(), CallError> {
    trading::record(db, type_id, quantity, price, buyer, seller, at.id, at.region, now).await.map_err(db_err)?;
    Ok(())
}


async fn my_order(db: &(impl Executor + ?Sized), char_id: i64, order_id: i64, now: i64) -> Result<Order, CallError> {
    orders::my_order(db, char_id, order_id, now).await.map_err(db_err)?
        .ok_or_else(|| notify_error(messages::MARKET_ORDER_CLOSED))
}

async fn insert_order(db: &(impl Executor + ?Sized), o: &mut Order) -> Result<(), CallError> {
    o.id = orders::insert(db, o).await.map_err(db_err)?;
    Ok(())
}

/// Move a player's order to the history.
async fn close(db: &(impl Executor + ?Sized), o: &Order, state: i32, now: i64) -> Result<(), CallError> {
    trading::close(db, o.id, o.vol_remaining, state, now).await.map_err(db_err)?;
    Ok(())
}

/// Give a closed order's escrow back: a sell order's items, a buy order's ISK.
async fn refund<X: Send>(db: &(impl Executor + ?Sized), sde: &crate::sde::Sde, outbox: &mut Outbox<X>, o: &Order) -> Result<(), CallError> {
    let Some(owner) = o.char_id else { return Ok(()) };
    if o.bid {
        credit(db, outbox, owner, round2(o.escrow)).await
    } else if o.vol_remaining > 0 {
        deliver(db, sde, outbox, owner, o.at.id, o.type_id, o.vol_remaining).await
    } else {
        Ok(())
    }
}

/// The client's order KeyVal (OnOwnOrdersChanged, BuyMultipleItems).
fn order_keyval(o: &Order) -> Value {
    keyval([
        ("orderID", Value::Int(o.id)),
        ("typeID", Value::Int(o.type_id as i64)),
        ("charID", o.char_id.map(Value::Int).unwrap_or(Value::None)),
        ("stationID", Value::Int(o.at.id)),
        ("solarSystemID", Value::Int(o.at.system)),
        ("regionID", Value::Int(o.at.region)),
        ("price", Value::Float(o.price)),
        ("range", Value::Int(o.range)),
        ("volEntered", Value::Int(o.vol_entered as i64)),
        ("volRemaining", Value::Int(o.vol_remaining as i64)),
        ("minVolume", Value::Int(o.min_volume as i64)),
        ("bid", Value::Bool(o.bid)),
        ("duration", Value::Int(o.duration)),
        ("issueDate", Value::Int(o.issued)),
    ])
}

fn tell_owner<X>(outbox: &mut Outbox<X>, o: &Order, reason: &str) {
    if let Some(owner) = o.char_id {
        outbox.notify_pilot(owner, "OnOwnOrdersChanged", vec![Value::list([order_keyval(o)]), Value::str(reason), Value::Bool(false)]);
    }
}

/// `quantity` of a resting order traded: a player's order shrinks, and closes once filled.
async fn consume<X: Send>(db: &(impl Executor + ?Sized), outbox: &mut Outbox<X>, o: &mut Order, quantity: i32, now: i64) -> Result<(), CallError> {
    let Some(owner) = o.char_id else { return Ok(()) };
    o.vol_remaining -= quantity;
    if o.bid {
        o.escrow = round2(o.escrow - o.price * quantity as f64).max(0.0);
    }
    if o.vol_remaining <= 0 {
        if o.bid {
            credit(db, outbox, owner, o.escrow).await?;
        }
        close(db, o, STATE_CLOSED, now).await?;
    } else {
        trading::consume(db, o.id, o.vol_remaining, o.escrow).await.map_err(db_err)?;
    }
    tell_owner(outbox, o, "Modify");
    Ok(())
}

/// Sell up to `quantity` x `type_id` at `at` to the best bids paying at least `floor`.
/// Returns how many sold.
async fn hit_bids(db: &(impl Executor + ?Sized), outbox: &mut Outbox<MarketEffect>, sde: &crate::sde::Sde, seller: i64, at: &Station, type_id: i32, floor: f64, quantity: i32, now: i64) -> Result<i32, CallError> {
    let rows = orders::bids(db, now / FILETIME_DAY * FILETIME_DAY, type_id, at.region, floor).await.map_err(db_err)?;
    let tax = skills(db, seller).await?.sales_tax;
    let mut left = quantity;
    for mut bid in rows {
        if left == 0 {
            break;
        }
        if !in_range(&sde.map, bid.range, &bid.at, at) {
            continue;
        }
        bid.at.owner = sde.map.station(bid.at.id).and_then(|s| s.owner_id).unwrap_or(0);
        let available = if bid.char_id.is_some() { bid.vol_remaining } else { left };
        let q = left.min(available);
        if q < bid.min_volume && available >= bid.min_volume {
            continue;
        }
        let value = round2(bid.price * q as f64);
        credit(db, outbox, seller, round2(value * (1.0 - tax))).await?;
        if let Some(buyer) = bid.char_id {
            deliver(db, sde, outbox, buyer, at.id, type_id, q).await?;
        }
        record(db, type_id, q, bid.price, bid.char_id.unwrap_or(bid.at.owner), seller, at, now).await?;
        consume(db, outbox, &mut bid, q, now).await?;
        outbox.event(crate::lua::events::GameEvent::MarketOrderFilled { order_id: bid.id, buyer_id: bid.char_id.unwrap_or(bid.at.owner), seller_id: seller, type_id, quantity: q, price: bid.price });
        outbox.operation(MarketEffect::Sold { seller, quantity: q, type_id, station: at.id, value, order: bid.id });
        left -= q;
    }
    Ok(quantity - left)
}

/// Buy up to `quantity` x `type_id` from the cheapest asks within `range` of `at` at no more
/// than `cap`, paying from the wallet unless an order's escrow covers it.
/// Returns (bought, spent).
#[allow(clippy::too_many_arguments)]
async fn lift_asks(db: &(impl Executor + ?Sized), outbox: &mut Outbox<MarketEffect>, sde: &crate::sde::Sde, buyer: i64, at: &Station, range: i64, type_id: i32, cap: f64, quantity: i32, from_wallet: bool, now: i64) -> Result<(i32, f64), CallError> {
    let rows = orders::asks(db, now / FILETIME_DAY * FILETIME_DAY, type_id, at.region, cap).await.map_err(db_err)?;
    let (mut left, mut spent) = (quantity, 0.0);
    for mut ask in rows {
        if left == 0 {
            break;
        }
        if !in_range(&sde.map, range, at, &ask.at) {
            continue;
        }
        ask.at.owner = sde.map.station(ask.at.id).and_then(|s| s.owner_id).unwrap_or(0);
        let available = if ask.char_id.is_some() { ask.vol_remaining } else { left };
        let q = left.min(available);
        let value = round2(ask.price * q as f64);
        if from_wallet {
            match debit(db, outbox, buyer, value).await {
                Ok(()) => {}
                Err(CallError::User { .. }) if left < quantity => break,
                Err(e) => return Err(e),
            }
        }
        if let Some(seller) = ask.char_id {
            let tax = skills(db, seller).await?.sales_tax;
            credit(db, outbox, seller, round2(value * (1.0 - tax))).await?;
        }
        deliver(db, sde, outbox, buyer, ask.at.id, type_id, q).await?;
        record(db, type_id, q, ask.price, buyer, ask.char_id.unwrap_or(ask.at.owner), &ask.at, now).await?;
        consume(db, outbox, &mut ask, q, now).await?;
        outbox.event(crate::lua::events::GameEvent::MarketOrderFilled { order_id: ask.id, buyer_id: buyer, seller_id: ask.char_id.unwrap_or(ask.at.owner), type_id, quantity: q, price: ask.price });
        outbox.operation(MarketEffect::Bought { buyer, quantity: q, type_id, station: ask.at.id, value, order: ask.id });
        left -= q;
        spent += value;
    }
    Ok((quantity - left, spent))
}

/// `PlaceBuyOrder` / `BuyMultipleItems`: buy what the book offers now, and with a duration
/// leave the rest as a buy order. Returns the order as the client's KeyVal.
#[allow(clippy::too_many_arguments)]
async fn place_buy(c: &Client, station_id: i64, type_id: i64, price: f64, quantity: i64, range: i64, min_volume: i64, duration: i64) -> CallResult {
    let (db, world) = (&c.db, &*c.world);
    let me = c.session.int("charid").unwrap_or(0);
    let price = valid_price(price)?;
    let now = filetime_now();
    if quantity <= 0 || quantity > i32::MAX as i64 || !valid_duration(duration, now) {
        return Err(user_error(messages::MSG_MKT_ORDER_DID_NOT_MATCH, vec![]));
    }
    let (type_id, quantity) = (type_id as i32, quantity as i32);
    let min_volume = (min_volume.clamp(1, quantity as i64)) as i32;
    let _trading = lock().await;
    let at = station(&c.sde, station_id)?;
    let sde = Arc::clone(&c.sde);
    db.transaction(|db, outbox: &mut Outbox<MarketEffect>| {
    let sde = Arc::clone(&sde);
    Box::pin(async move {
    let skills = skills(db, me).await?;
    let broker_rate = if duration > 0 {
        check_order_slots(db, me, &skills).await?;
        let rate = station_broker_rate(db, &sde, me, at.owner, skills.broker_fee).await?;
        let escrow = round2(price * quantity as f64);
        require_balance(db, me, escrow + broker_fee(rate, escrow)).await?;
        rate
    } else {
        skills.broker_fee
    };
    let (bought, _) = lift_asks(db, outbox, &sde, me, &at, range, type_id, price, quantity, true, now).await?;
    let mut order = Order {
        id: 0,
        type_id,
        char_id: Some(me),
        at,
        price,
        vol_entered: quantity,
        vol_remaining: quantity - bought,
        min_volume,
        bid: true,
        range,
        duration,
        issued: now,
        escrow: 0.0,
    };
    if duration == 0 || order.vol_remaining == 0 {
        if bought == 0 {
            return Err(user_error(messages::MSG_MKT_ORDER_DID_NOT_MATCH, vec![]));
        }
        order.duration = 0;
        order.vol_remaining = 0;
        outbox.notify_pilot(me, "OnOwnOrdersChanged", vec![Value::list([order_keyval(&order)]), Value::str("Expiry"), Value::Bool(false)]);
        return Ok(order_keyval(&order));
    }
    order.escrow = round2(price * order.vol_remaining as f64);
    debit(db, outbox, me, order.escrow + broker_fee(broker_rate, order.escrow)).await?;
    insert_order(db, &mut order).await?;
    outbox.event(crate::lua::events::GameEvent::MarketOrderPlaced { char_id: me, order_id: order.id, type_id, quantity: order.vol_remaining, price, is_buy: true });
    outbox.operation(MarketEffect::Placed { buyer: true, owner: me, order: order.id, quantity: order.vol_remaining, type_id, price });
    tell_owner(outbox, &order, "Add");
    Ok(order_keyval(&order))
    }) }, |value, outbox| async move { dispatch_market(value, outbox, world) }).await
}

/// One item of `PlaceMultiSellOrder`: sell to the bids now, and with a duration list the
/// rest as a sell order. Returns whether anything sold or was listed.
#[allow(clippy::too_many_arguments)]
async fn place_sell(identity: &Identity, db: &Db, world: &World, sde: &Arc<Sde>, item_id: i64, station_id: i64, type_id: i64, price: f64, quantity: i64, duration: i64) -> Result<bool, CallError> {
    let me = identity.char_id;
    let price = valid_price(price)?;
    let now = filetime_now();
    if quantity <= 0 || !valid_duration(duration, now) {
        return Err(user_error(messages::MSG_MKT_ORDER_DID_NOT_MATCH, vec![]));
    }
    let _trading = lock().await;
    let sde = Arc::clone(sde);
    db.transaction(|db, outbox: &mut Outbox<MarketEffect>| {
    let sde = Arc::clone(&sde);
    Box::pin(async move {
    let item = load_items(db, &sde, ItemQuery::Id(item_id)).await.map_err(db_err)?.pop().ok_or_else(|| notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION))?;
    if item.owner_id != me || item.location_id != station_id || item.flag_id != FLAG_HANGAR || item.type_id as i64 != type_id {
        return Err(notify_error(messages::MARKET_SELL_ITEM_MUST_BE_IN_STATION_HANGAR));
    }
    if crate::dao::industry::has_active_blueprint_job(db, &[item.item_id]).await.map_err(db_err)? {
        return Err(notify_error(messages::industry::BLUEPRINT_ACTIVE));
    }
    if item.singleton {
        return Err(user_error(messages::MSG_REPACKAGE_BEFORE_SELLING, vec![("item".into(), Value::Int(type_id))]));
    }
    let (type_id, quantity) = (item.type_id, quantity.min(item.quantity as i64) as i32);
    let at = station(&sde, station_id)?;
    let skills = skills(db, me).await?;
    if duration == 0 {
        let sold = hit_bids(db, outbox, &sde, me, &at, type_id, price, quantity, now).await?;
        if sold == 0 {
            return Err(user_error(messages::MSG_MKT_ORDER_DID_NOT_MATCH, vec![]));
        }
        take_items(db, outbox, item, sold).await?;
        let done = Order {
            id: 0,
            type_id,
            char_id: Some(me),
            at,
            price,
            vol_entered: sold,
            vol_remaining: 0,
            min_volume: 1,
            bid: false,
            range: RANGE_STATION,
            duration: 0,
            issued: now,
            escrow: 0.0,
        };
        outbox.notify_pilot(me, "OnOwnOrdersChanged", vec![Value::list([order_keyval(&done)]), Value::str("Expiry"), Value::Bool(false)]);
        return Ok(true);
    }
    check_order_slots(db, me, &skills).await?;
    let broker_rate = station_broker_rate(db, &sde, me, at.owner, skills.broker_fee).await?;
    require_balance(db, me, broker_fee(broker_rate, price * quantity as f64)).await?;
    // The items go into escrow first; what the bids don't take stays listed.
    take_items(db, outbox, item, quantity).await?;
    let sold = hit_bids(db, outbox, &sde, me, &at, type_id, price, quantity, now).await?;
    let mut order = Order {
        id: 0,
        type_id,
        char_id: Some(me),
        at,
        price,
        vol_entered: quantity,
        vol_remaining: quantity - sold,
        min_volume: 1,
        bid: false,
        range: RANGE_STATION,
        duration,
        issued: now,
        escrow: 0.0,
    };
    if order.vol_remaining == 0 {
        return Ok(true);
    }
    debit(db, outbox, me, broker_fee(broker_rate, price * order.vol_remaining as f64)).await?;
    insert_order(db, &mut order).await?;
    outbox.event(crate::lua::events::GameEvent::MarketOrderPlaced { char_id: me, order_id: order.id, type_id, quantity: order.vol_remaining, price, is_buy: false });
    outbox.operation(MarketEffect::Placed { buyer: false, owner: me, order: order.id, quantity: order.vol_remaining, type_id, price });
    tell_owner(outbox, &order, "Add");
    Ok(true)
    }) }, |value, outbox| async move { dispatch_market(value, outbox, world) }).await
}

/// `ModifyCharOrder`: reprice an open order, paying the modification fee (and topping up or
/// releasing a buy order's escrow). A price that crosses the book trades at once.
async fn modify(c: &Client, order_id: i64, new_price: f64) -> CallResult {
    let (db, world) = (&c.db, &*c.world);
    let me = c.session.int("charid").unwrap_or(0);
    let new_price = valid_price(new_price)?;
    let _trading = lock().await;
    let sde = Arc::clone(&c.sde);
    let now = filetime_now();
    db.transaction(|db, outbox: &mut Outbox<MarketEffect>| {
    let sde = Arc::clone(&sde);
    Box::pin(async move {
    let mut o = my_order(db, me, order_id, now / FILETIME_DAY * FILETIME_DAY).await?;
    let skills = skills(db, me).await?;
    let owner = station(&sde, o.at.id)?.owner;
    let broker_rate = station_broker_rate(db, &sde, me, owner, skills.broker_fee).await?;
    let volume = o.vol_remaining as f64;
    let (old_amount, new_amount) = (o.price * volume, new_price * volume);
    let fee = round2(
        (new_amount * broker_rate * (1.0 - skills.modification_discount) + (new_amount - old_amount).max(0.0) * broker_rate).max(crate::config::game().market.broker_fee_min_isk),
    );
    let escrow = if o.bid { round2(new_amount) } else { 0.0 };
    let top_up = escrow - o.escrow;
    debit(db, outbox, me, fee + top_up.max(0.0)).await?;
    credit(db, outbox, me, round2(-top_up)).await?;
    o.price = new_price;
    o.escrow = escrow;
    o.issued = now;
    trading::reprice(db, o.id, o.price, o.escrow, o.issued).await.map_err(db_err)?;
    outbox.operation(MarketEffect::Repriced { owner: me, order: o.id, price: new_price });
    let traded = if o.bid {
        let (bought, spent) = lift_asks(db, outbox, &sde, me, &o.at, o.range, o.type_id, o.price, o.vol_remaining, false, now).await?;
        // The escrow paid this order's price; cheaper asks leave change.
        credit(db, outbox, me, round2(o.price * bought as f64 - spent)).await?;
        bought
    } else {
        hit_bids(db, outbox, &sde, me, &o.at, o.type_id, o.price, o.vol_remaining, now).await?
    };
    if traded > 0 {
        consume(db, outbox, &mut o, traded, now).await?;
    } else {
        tell_owner(outbox, &o, "Modify");
    }
    Ok(Value::None)
    }) }, |value, outbox| async move { dispatch_market(value, outbox, world) }).await
}

/// `CancelCharOrder`: close an open order and hand back its escrow (the broker's fee stays paid).
async fn cancel(c: &Client, order_id: i64) -> CallResult {
    let (db, world) = (&c.db, &*c.world);
    let sde = &c.sde;
    let me = c.session.int("charid").unwrap_or(0);
    let _trading = lock().await;
    let now = filetime_now();
    db.transaction(|db, outbox: &mut Outbox<MarketEffect>| { let sde = sde.clone(); Box::pin(async move {
        let sde = &*sde;
        let o = my_order(db, me, order_id, now / FILETIME_DAY * FILETIME_DAY).await?;
        close(db, &o, STATE_CANCELLED, now).await?;
        refund(db, sde, outbox, &o).await?;
        outbox.operation(MarketEffect::Cancelled { owner: me, order: o.id });
        tell_owner(outbox, &o, "Cancel");
        Ok(Value::None)
    })}, |value, outbox| async move { dispatch_market(value, outbox, world) }).await
}

/// Execute one scheduler-driven expiry scan.
pub(crate) fn expire_orders(db: Db, world: Arc<World>, _: crate::scheduler::Job) -> std::pin::Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send>> {
    Box::pin(async move {
        if let Err(CallError::Internal(e)) = expire(&db, world.sde(), &world).await { warn!("expiring market orders failed: {e:#}"); }
        Ok(())
    })
}

async fn expire(db: &Db, sde: &Arc<crate::sde::Sde>, world: &World) -> Result<(), CallError> {
    let _trading = lock().await;
    let now = filetime_now();
    db.transaction(|db, outbox: &mut Outbox<MarketEffect>| { let sde = sde.clone(); Box::pin(async move {
        let sde = &*sde;
        let rows = orders::expired(db, now / FILETIME_DAY * FILETIME_DAY, FILETIME_DAY, now).await.map_err(db_err)?;
        for o in rows {
            close(db, &o, STATE_CLOSED, now).await?;
            refund(db, sde, outbox, &o).await?;
            outbox.operation(MarketEffect::Expired { owner: o.char_id, order: o.id });
            tell_owner(outbox, &o, "Expiry");
        }
        Ok(())
    })}, |value, outbox| async move { dispatch_market(value, outbox, world) }).await?;
    Ok(())
}


pub async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    if call.service != "marketProxy" {
        return None;
    }
    let int = |i: usize| call.args.get(i).and_then(|v| v.as_int());
    let float = |i: usize| call.args.get(i).and_then(|v| v.as_float()).unwrap_or(0.0);
    Some(match call.method {
        "GetOrders" => orders(&c.db, c.session.int("regionid").unwrap_or(0), int(0).unwrap_or(0)).await,
        "GetStationAsks" => {
            let station = c.session.int("stationid").unwrap_or(0);
            best_asks(&c.db, Area::Station, station, false).await
        }
        "GetSystemAsks" => {
            let system = c.session.int("solarsystemid2").unwrap_or(0);
            best_asks(&c.db, Area::System, system, false).await
        }
        "GetRegionBest" => {
            let region = c.session.int("regionid").unwrap_or(0);
            best_asks(&c.db, Area::Region, region, true).await
        }
        // PlaceBuyOrder(stationID, typeID, price, quantity, orderRange, minVolume, duration, useCorp, expectedBrokersFee)
        "PlaceBuyOrder" => {
            let (station, type_id, quantity) = (int(0).unwrap_or(0), int(1).unwrap_or(0), int(3).unwrap_or(0));
            let (range, min_volume, duration) = (int(4).unwrap_or(RANGE_STATION), int(5).unwrap_or(1), int(6).unwrap_or(0));
            place_buy(c, station, type_id, float(2), quantity, range, min_volume, duration).await.map(|_| Value::None)
        }
        // BuyMultipleItems(stationID, [KeyVal(stationID, typeID, price, quantity, ...)], useCorp) -> orders
        "BuyMultipleItems" => {
            let station = int(0).unwrap_or(0);
            let items = call.args.get(1).and_then(|v| v.items()).map(|v| v.to_vec()).unwrap_or_default();
            let mut done = vec![];
            for item in &items {
                let get = |k: &str| value_field(item, k);
                let type_id = get("typeID").and_then(|v| v.as_int()).unwrap_or(0);
                let price = get("price").and_then(|v| v.as_float()).unwrap_or(0.0);
                let quantity = get("quantity").and_then(|v| v.as_int()).unwrap_or(0);
                let at = get("stationID").and_then(|v| v.as_int()).unwrap_or(station);
                match place_buy(c, at, type_id, price, quantity, RANGE_STATION, 1, 0).await {
                    Ok(order) => done.push(order),
                    Err(e @ CallError::User { .. }) if items.len() == 1 => return Some(Err(e)),
                    Err(CallError::User { .. }) => {}
                    Err(e) => return Some(Err(e)),
                }
            }
            Ok(Value::List(done))
        }
        // PlaceMultiSellOrder([KeyVal(stationID, typeID, itemID, price, quantity, ...)], useCorp, duration, expectedBrokersFee)
        // -> whether any order was made (sellMulti closes its window on True)
        "PlaceMultiSellOrder" => {
            let items = call.args.first().and_then(|v| v.items()).map(|v| v.to_vec()).unwrap_or_default();
            let duration = int(2).unwrap_or(0);
            let mut any = false;
            for item in &items {
                let get = |k: &str| value_field(item, k).and_then(|v| v.as_int()).unwrap_or(0);
                let price = value_field(item, "price").and_then(|v| v.as_float()).unwrap_or(0.0);
                match place_sell(&Identity::of_client(c), &c.db, &c.world, &c.sde, get("itemID"), get("stationID"), get("typeID"), price, get("quantity"), duration).await {
                    Ok(made) => any |= made,
                    Err(e @ CallError::User { .. }) if items.len() == 1 => return Some(Err(e)),
                    Err(CallError::User { msg, .. }) => warn!(user = c.login.user_id, "market: selling {} failed: {msg}", get("itemID")),
                    Err(e) => return Some(Err(e)),
                }
            }
            Ok(Value::Bool(any))
        }
        // ModifyCharOrder(orderID, newPrice, bid, stationID, solarSystemID, price, range, volRemaining, issueDate)
        "ModifyCharOrder" => modify(c, int(0).unwrap_or(0), float(1)).await,
        // CancelCharOrder(orderID, regionID)
        "CancelCharOrder" => cancel(c, int(0).unwrap_or(0)).await,
        "StartupCheck" => Ok(Value::None),
        "GetCharEscrow" => escrow(c).await,
        "GetPlexBest" => Ok(Value::Dict(vec![])),
        "GetPlexHistory" => Ok(Value::Dict(vec![])),
        // GetHistoryForManyTypeIDs({typeID}) -> {typeID: (old, new)}
        "GetHistoryForManyTypeIDs" => {
            let type_ids: Vec<i32> = call.args.first().and_then(|v| v.items()).unwrap_or(&[]).iter().filter_map(|v| v.as_int()).map(|t| t as i32).collect();
            price_history(c, &type_ids)
                .await
                .map(|h| Value::Dict(h.into_iter().map(|(t, (old, new))| (Value::Int(t as i64), Value::tuple([old, new]))).collect()))
        }
        "GetOldPriceHistory" | "GetNewPriceHistory" => {
            let type_id = int(0).unwrap_or(0) as i32;
            price_history(c, &[type_id]).await.map(|mut h| {
                let (old, new) = h.remove(&type_id).unwrap_or_else(|| (crowset(&history_header(), vec![]), crowset(&history_header(), vec![])));
                if call.method == "GetOldPriceHistory" {
                    old
                } else {
                    new
                }
            })
        }
        "GetPlexOldPriceHistory" | "GetPlexNewPriceHistory" => Ok(crowset(&history_header(), vec![])),
        "CharGetTransactions" => transactions(c, int(0)).await,
        "CorpGetTransactions" => Ok(Value::list([])),
        "GetCharOrders" => my_orders(c).await,
        "GetCorporationOrders" => Ok(Value::list([])),
        "GetMarketOrderHistory" => order_history(c).await,
        _ => return None,
    })
}

#[cfg(test)]
mod tests;

