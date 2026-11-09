//! marketProxy: regional order books (eve/client/script/ui/services/marketsvc.py).
//!
//! The book holds the NPC orders seeded at the starter stations (sql/market_seed.sql, and
//! region-wide buy orders priced at the configured NPC buy ratio) and players' orders. NPC
//! stock and demand are unlimited, so fills never consume NPC orders.
//!
//! A trade happens at the resting order's price and delivers at the seller's station:
//! personal purchases reach the hangar, corporate purchases reach Deliveries. The seller
//! pays sales tax and an order that rests pays the broker's
//! fee, as marketutil computes them (skilllimits, brokerFee), using effective skills and
//! raw NPC station owner corporation and faction standings from game.standings. Faction-war upgrades are
//! unsupported (upgrade level zero); structure markets are unsupported. A sell
//! order holds its items and a buy order its ISK in escrow until it fills, is cancelled or
//! expires; closed orders move to game.market_order_history, and every fill is a row of
//! game.market_transactions (wallet transactions and price history). Ranges in jumps use
//! the startup-loaded SDE map. Fees, tax, duration, history and maintenance cadence are
//! configured market policies. The SQL startup seed is the static-data exception.
//! Order expiry scans run as durable periodic jobs registered through the services scheduler
//! and persisted in game.jobs, not detached maintenance sleeps. Scan errors reach the scheduler.
//! Personal and corporate cash movements and real fill account identities are recorded transactionally.
//! Issuer, financial owner and asset owner are distinct. Corporate orders preserve their
//! original wallet and source custody; same-account matches are skipped, while separate
//! corporate divisions can trade. Corporate fills continue after issuer departure.
//! Market price and escrow calculations remain floating point; wallet postings use checked
//! exact cents. Upwell markets and shared ship bay sources are unsupported.
//! Combined order charges mutate cash once and journal numeric escrow and broker components;
//! their journal captions identify the authorizing actor, while trade entries identify financial owners.
//! Escrow consumption itself does not move cash or produce a wallet journal entry.
//! Committed fills and newly resting orders queue Lua events.

use crate::dao::market::orders::{self, Order, Station};
use crate::dao::market::trading as trading_dao;
use crate::dao::market::listings::{self, Area};
use super::{crowset, db_err, keyval, notify_error, Call, CallError, CallResult};
use crate::client::{filetime_now, Client};
use infinity_core::consts::FILETIME_DAY;
use infinity_core::consts::{OWNER_SCC, REF_BROKER_FEE, REF_MARKET_ESCROW, REF_MARKET_TRANSACTION};
use crate::dao::wallets::{accounts, journal::Entry};
use crate::services::wallets::journal;
use crate::services::wallets::ownership::FinancialOwner;
use crate::db::Db;
use crate::db::executor::Executor;
use crate::db::outbox::{Effect, Outbox};
use infinity_core::messages;
use crate::space::World;
use marshal::{DbRowDescriptor, DbType, Value};
use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;
use tracing::info;

mod authorization;
mod assets;
mod cache;
mod notifications;
use notifications::tell_owner;

#[derive(Clone, Copy)]
pub(crate) struct TradeParty { pub(crate) issuer: i64, pub(crate) financial: FinancialOwner }

impl TradeParty {
    pub(super) fn from_order(order: &Order) -> Option<Self> {
        order.char_id.map(|issuer| Self { issuer, financial: FinancialOwner {
            id: order.corporation_id.unwrap_or(issuer), wallet_key: order.wallet_key,
        } })
    }
    pub(super) fn same_account(self, other: Self) -> bool {
        self.financial.id == other.financial.id && self.financial.wallet_key == other.financial.wallet_key
    }
}

#[derive(Clone, Copy)]
pub(crate) struct BuyRequest {
    pub(crate) station_id: i64, pub(crate) type_id: i64, pub(crate) price: f64, pub(crate) quantity: i64,
    pub(crate) range: i64, pub(crate) min_volume: i64, pub(crate) duration: i64,
    pub(crate) expected_brokers_fee: Option<f64>,
}
#[derive(Clone, Copy)]
pub(crate) struct SellRequest {
    pub(crate) item_id: i64, pub(crate) station_id: i64, pub(crate) type_id: i64, pub(crate) price: f64,
    pub(crate) quantity: i64, pub(crate) duration: i64, pub(crate) office_id: Option<i64>,
    pub(crate) expected_brokers_fee: Option<f64>,
}
#[derive(Clone, Copy)]
pub(crate) struct ModifyRequest {
    pub(crate) order_id: i64, pub(crate) new_price: f64, pub(crate) bid: bool,
    pub(crate) station_id: i64, pub(crate) system_id: i64, pub(crate) old_price: f64,
    pub(crate) range: i64, pub(crate) remaining: i64, pub(crate) issued: i64,
    pub(crate) requested_scope: orders::MarketScope,
}
mod fees;
pub(crate) mod lifecycle;
mod lists;
mod market_proxy;
pub(crate) mod trading;

/// walletEntryFormat._FmtWalletEntryManual selects owner1 as the escrow authorizer.
const ESCROW_AUTHORIZER_REFERENCE: i64 = -1;

const RANGE_STATION: i64 = -1;
const RANGE_SOLAR_SYSTEM: i64 = 0;
const RANGE_REGION: i64 = 32767;
/// marketutil.const.MAX_ORDER_PRICE
const MAX_ORDER_PRICE: f64 = 9_223_372_036_854.0;
/// What the client shows for a system it can't route to.
const UNREACHABLE: i64 = 1_000_000;

// marketHistory orderState
const STATE_CLOSED: i32 = 2; // filled or expired
const STATE_CANCELLED: i32 = 3;

enum MarketEffect {
    Bought { buyer: i64, quantity: i64, type_id: i32, station: i64, value: f64, order: i64 },
    Sold { seller: i64, quantity: i64, type_id: i32, station: i64, value: f64, order: i64 },
    Placed { buyer: bool, owner: i64, order: i64, quantity: i64, type_id: i32, price: f64 },
    Repriced { owner: i64, order: i64, price: f64 },
    Cancelled { owner: i64, order: i64 },
    Expired { owner: Option<i64>, order: i64 },
}
crate::progress::leaf_effect!(MarketEffect);

fn dispatch_market<T>(value: T, outbox: Outbox<MarketEffect>, world: &World) -> Result<T, CallError> {
    for effect in outbox.into_entries() {
        match effect {
            Effect::Notification(notification) => notification.dispatch(world, None)?,
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
fn journal_entry(actor: i64, kind: i32, owner1: i64, owner2: i64, order: Option<i64>, fill: Option<i64>, now: i64, side: i16) -> Entry<'static> {
    Entry { actor_id: actor, entry_type_id: kind, owner_id1: if kind == REF_MARKET_ESCROW || kind == REF_BROKER_FEE { actor } else { owner1 }, owner_id2: owner2,
        reference_id: if kind == REF_MARKET_ESCROW { ESCROW_AUTHORIZER_REFERENCE } else { fill.or(order).unwrap_or(0) }, created_at: now, description: "",
        market_transaction_id: fill, order_id: order, side }
}


pub(super) fn round2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

/// `marketutil.ticks`: prices use two ISK decimals and four significant figures.
fn valid_price(price: f64) -> Result<f64, CallError> {
    if fees::valid_tick(price) { return Ok(price); }
    Err(notify_error(messages::INVALID_PRICE))
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
async fn my_order(db: &(impl Executor + ?Sized), char_id: i64, order_id: i64) -> Result<Order, CallError> {
    orders::my_order(db, char_id, order_id).await.map_err(db_err)?
        .ok_or_else(|| notify_error(messages::MARKET_ORDER_CLOSED))
}

async fn insert_order(db: &(impl Executor + ?Sized), o: &mut Order) -> Result<(), CallError> {
    o.id = orders::insert(db, o).await.map_err(db_err)?;
    Ok(())
}

/// Move a player's order to the history.
async fn close(db: &(impl Executor + ?Sized), o: &Order, state: i32, reason: orders::CloseReason, now: i64) -> Result<(), CallError> {
    if trading_dao::close(db, o, o.vol_remaining, crate::isk::IskAmount::ZERO, 0, state, reason, now).await.map_err(db_err)? != 1 {
        return Err(notify_error(messages::MARKET_ORDER_CLOSED));
    }
    Ok(())
}
/// The client's order KeyVal (OnOwnOrdersChanged, BuyMultipleItems).
fn order_keyval(o: &Order) -> Value {
    keyval([
        ("orderID", Value::Int(o.id)),
        ("typeID", Value::Int(o.type_id as i64)),
        ("charID", o.char_id.map(Value::Int).unwrap_or(Value::None)),
        ("keyID", Value::Int(i64::from(o.wallet_key))),
        ("isCorp", Value::Bool(o.corporation_id.is_some())),
        ("escrow", Value::Float(o.escrow.wire_isk())),
        ("stationID", Value::Int(o.at.id)),
        ("solarSystemID", Value::Int(o.at.system)),
        ("regionID", Value::Int(o.at.region)),
        ("price", Value::Float(o.price.wire_isk())),
        ("range", Value::Int(o.range)),
        ("volEntered", Value::Int(o.vol_entered)),
        ("volRemaining", Value::Int(o.vol_remaining)),
        ("minVolume", Value::Int(o.min_volume)),
        ("bid", Value::Bool(o.bid)),
        ("duration", Value::Int(o.duration)),
        ("issueDate", Value::Int(o.issued.unwrap_or_else(today))),
    ])
}

pub async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    market_proxy::dispatch(c, call).await
}

#[cfg(test)]
mod tests;
