//! Order and transaction projections for `marketProxy`
//! (`eve/client/script/ui/services/marketsvc.py`), using market and wallet queries
//! and the startup-loaded SDE map. Public books use coherent version snapshots;
//! matching, mutations and notifications are separate.
use super::*;
/// Columns of an order row; the client sets `jumps` itself (RefreshJumps).
fn order_header() -> DbRowDescriptor {
    DbRowDescriptor::new([
        ("price", DbType::R8),
        ("volRemaining", DbType::I8),
        ("typeID", DbType::I4),
        ("range", DbType::I2),
        ("orderID", DbType::I8),
        ("volEntered", DbType::I8),
        ("minVolume", DbType::I8),
        ("bid", DbType::Bool),
        ("issueDate", DbType::FileTime),
        ("duration", DbType::I2),
        ("stationID", DbType::I8),
        ("regionID", DbType::I4),
        ("solarSystemID", DbType::I4),
        ("constellationID", DbType::I4),
        ("jumps", DbType::I2),
    ])
}
/// A coherent public book; generic calls include the regional session cache key.
pub(super) async fn orders(db: &Db, region: i64, type_id: i32, regional_session: bool) -> CallResult {
    let snapshot = crate::dao::market::versions::snapshot(db, orders::MarketScope::for_type(type_id), type_id, region, filetime_now(), FILETIME_DAY).await.map_err(db_err)?;
    Ok(super::cache::wrap(book_rows(&snapshot.rows), &snapshot, regional_session))
}

fn book_rows(rows: &[Order]) -> Value {
    // Seeded NPC dates are persisted for stable snapshots; legacy NULLs retain the baseline UTC-day fallback.
    let fallback = today();
    let header = order_header();
    let (mut asks, mut bids) = (vec![], vec![]);
    for r in rows {
        let line = [Value::Float(r.price.wire_isk()), Value::Int(r.vol_remaining), Value::Int(i64::from(r.type_id)), Value::Int(r.range), Value::Int(r.id), Value::Int(r.vol_entered), Value::Int(r.min_volume), Value::Bool(r.bid), Value::Int(r.issued.unwrap_or(fallback)), Value::Int(r.duration), Value::Int(r.at.id), Value::Int(r.at.region), Value::Int(r.at.system), Value::Int(r.at.constellation), Value::Int(0)];
        let row = header.row(&line);
        if r.bid { bids.push(row) } else { asks.push(row) }
    }
    Value::tuple([crowset(&header, asks), crowset(&header, bids)])
}

/// The cheapest global PLEX ask and its coherent cache token.
pub(super) async fn plex_best(db: &Db) -> CallResult {
    let type_id = infinity_core::consts::TYPE_PLEX;
    let snapshot = crate::dao::market::versions::best_ask_snapshot(db, orders::MarketScope::for_type(type_id), type_id, 0, filetime_now(), FILETIME_DAY).await.map_err(db_err)?;
    let best = snapshot.rows.first();
    let header = best_header();
    let inner = Value::Dict(best.into_iter().map(|r| (Value::Int(i64::from(type_id)), header.row(&[Value::Float(r.price.wire_isk()), Value::Int(r.vol_remaining), Value::Int(i64::from(type_id)), Value::Int(r.at.id)]))).collect());
    Ok(super::cache::wrap(inner, &snapshot, false))
}

fn best_header() -> DbRowDescriptor {
    DbRowDescriptor::new([("price", DbType::R8), ("volRemaining", DbType::I8), ("typeID", DbType::I4), ("stationID", DbType::I8)])
}

/// Cheapest ask per type within `column = id`: {typeID: best}.
pub(super) async fn best_asks(db: &Db, area: Area, id: i64, as_rows: bool) -> CallResult {
    let rows = listings::best_asks(db, filetime_now(), FILETIME_DAY, area, id, None).await.map_err(db_err)?;
    // Station/system answers are tuples (marketutil.ConvertTuplesToBestByOrders); the
    // region answer is used as-is, by attribute.
    let header = best_header();
    Ok(Value::Dict(
        rows.iter()
            .map(|r| {
                let line = [Value::Float(r.price.wire_isk()), Value::Int(r.vol_remaining), Value::Int(r.type_id), Value::Int(r.station_id)];
                (Value::Int(r.type_id), if as_rows { header.row(&line) } else { Value::tuple(line) })
            })
            .collect(),
    ))
}
// ---------------------------------------------------------------- order lists

/// Open orders issued by the character, including corporate orders.
pub(super) async fn my_orders(c: &Client) -> CallResult {
    let me = c.session.int("charid").unwrap_or(0);
    let rows = orders::my_orders(&c.db, me).await.map_err(db_err)?;
    Ok(order_rows(c, &rows))
}

/// Open corporate orders in the caller's currently readable divisions.
pub(super) async fn corporation_orders(c: &Client) -> CallResult {
    let identity = crate::services::identity::Identity::of_client(c);
    let keys = super::authorization::readable_divisions(&c.db, &identity, None).await?;
    let rows = orders::corporation_orders(&c.db, identity.corp_id, &keys).await.map_err(db_err)?;
    Ok(order_rows(c, &rows))
}

fn order_rows(c: &Client, rows: &[Order]) -> Value {
    let here = c.session.int("solarsystemid2").unwrap_or(0);
    let fallback = today();
    let header = DbRowDescriptor::new([
        ("orderID", DbType::I8),
        ("typeID", DbType::I4),
        ("charID", DbType::I8),
        ("regionID", DbType::I4),
        ("stationID", DbType::I8),
        ("solarSystemID", DbType::I4),
        ("constellationID", DbType::I4),
        ("range", DbType::I2),
        ("bid", DbType::Bool),
        ("price", DbType::R8),
        ("volEntered", DbType::I8),
        ("volRemaining", DbType::I8),
        ("issueDate", DbType::FileTime),
        ("minVolume", DbType::I8),
        ("duration", DbType::I2),
        ("isCorp", DbType::Bool),
        ("escrow", DbType::R8),
        ("keyID", DbType::I4),
        ("jumps", DbType::I4),
    ]);
    let lines = rows
        .iter()
        .map(|o| {
            header.row(&[
                Value::Int(o.id),
                Value::Int(o.type_id as i64),
                o.char_id.map(Value::Int).unwrap_or(Value::None),
                Value::Int(o.at.region),
                Value::Int(o.at.id),
                Value::Int(o.at.system),
                Value::Int(o.at.constellation),
                Value::Int(o.range),
                Value::Bool(o.bid),
                Value::Float(o.price.wire_isk()),
                Value::Int(o.vol_entered),
                Value::Int(o.vol_remaining),
                Value::Int(o.issued.unwrap_or(fallback)),
                Value::Int(o.min_volume),
                Value::Int(o.duration),
                Value::Bool(o.corporation_id.is_some()),
                Value::Float(o.escrow.wire_isk()),
                Value::Int(i64::from(o.wallet_key)),
                Value::Int(jumps(&c.sde.map, here, o.at.system, UNREACHABLE).unwrap_or(UNREACHABLE)),
            ])
        })
        .collect();
    crowset(&header, lines)
}

/// `GetMarketOrderHistory`: the character's closed orders of the last 90 days.
pub(super) async fn order_history(c: &Client) -> CallResult {
    let me = c.session.int("charid").unwrap_or(0);
    let rows = listings::history(&c.db, me, filetime_now().saturating_sub(crate::config::game().market.order_history_days.saturating_mul(FILETIME_DAY))).await.map_err(db_err)?;
    let header = DbRowDescriptor::new([
        ("orderID", DbType::I8),
        ("typeID", DbType::I4),
        ("charID", DbType::I8),
        ("regionID", DbType::I4),
        ("stationID", DbType::I8),
        ("range", DbType::I2),
        ("bid", DbType::Bool),
        ("price", DbType::R8),
        ("volEntered", DbType::I8),
        ("volRemaining", DbType::I8),
        ("issueDate", DbType::FileTime),
        ("minVolume", DbType::I8),
        ("duration", DbType::I2),
        ("orderState", DbType::I4),
        ("lastStateChange", DbType::FileTime),
        ("isCorp", DbType::Bool),
        ("keyID", DbType::I4),
    ]);
    let lines = rows
        .iter()
        .map(|r| {
            header.row(&[
                Value::Int(r.order_id),
                Value::Int(r.type_id),
                Value::Int(r.char_id),
                Value::Int(r.region_id),
                Value::Int(r.station_id),
                Value::Int(r.range),
                Value::Bool(r.bid),
                Value::Float(r.price.wire_isk()),
                Value::Int(r.vol_entered),
                Value::Int(r.vol_remaining),
                Value::Int(r.issued),
                Value::Int(r.min_volume),
                Value::Int(r.duration),
                Value::Int(r.state),
                Value::Int(r.closed),
                Value::Bool(r.corporation_id.is_some()),
                Value::Int(i64::from(r.wallet_key)),
            ])
        })
        .collect();
    Ok(crowset(&header, lines))
}

/// `CharGetTransactions(fromDate)`: the character's fills, newest first (marketSvc.py).
pub(super) async fn transactions(c: &Client, from: Option<i64>) -> CallResult {
    let me = c.session.int("charid").unwrap_or(0);
    let from = from.unwrap_or(filetime_now().saturating_sub(crate::config::game().market.transaction_history_days.saturating_mul(FILETIME_DAY)));
    let rows = listings::transactions(&c.db, me, from).await.map_err(db_err)?;
    Ok(transaction_rows(&rows))
}

/// Corporate fills on each authorized wallet account side.
pub(super) async fn corporation_transactions(c: &Client, from: Option<i64>, key: Option<i32>) -> CallResult {
    let identity = crate::services::identity::Identity::of_client(c);
    let keys = super::authorization::readable_divisions(&c.db, &identity, key).await?;
    let from = from.unwrap_or(filetime_now().saturating_sub(crate::config::game().market.transaction_history_days.saturating_mul(FILETIME_DAY)));
    let rows = listings::corporation_transactions(&c.db, identity.corp_id, &keys, from).await.map_err(db_err)?;
    Ok(transaction_rows(&rows))
}

fn transaction_rows(rows: &[listings::Transaction]) -> Value {
    let header = DbRowDescriptor::new([
        ("transactionID", DbType::I8),
        ("transactionDate", DbType::FileTime),
        ("typeID", DbType::I4),
        ("quantity", DbType::I8),
        ("price", DbType::R8),
        ("stationID", DbType::I8),
        ("buyerID", DbType::I8),
        ("sellerID", DbType::I8),
        ("accountID", DbType::I8),
        ("buyerAccountID", DbType::I8),
        ("sellerAccountID", DbType::I8),
    ]);
    let lines = rows
        .iter()
        .map(|r| {
            header.row(&[
                Value::Int(r.transaction_id),
                Value::Int(r.date),
                Value::Int(r.type_id),
                Value::Int(r.quantity),
                Value::Float(r.price.wire_isk()),
                Value::Int(r.station_id),
                Value::Int(r.buyer_id),
                Value::Int(r.seller_id),
                r.account_id.map(Value::Int).unwrap_or(Value::None),
                r.buyer_account_id.map(Value::Int).unwrap_or(Value::None),
                r.seller_account_id.map(Value::Int).unwrap_or(Value::None),
            ])
        })
        .collect();
    crowset(&header, lines)
}

/// `GetCharEscrow`: ISK held by buy orders, and the value of items held by sell orders.
pub(super) async fn escrow(c: &Client) -> CallResult {
    let me = c.session.int("charid").unwrap_or(0);
    let r = listings::escrow(&c.db, me).await.map_err(db_err)?;
    Ok(keyval([("iskEscrow", Value::Float(aggregate_wire(&r.isk)?)), ("itemsEscrow", Value::Float(aggregate_wire(&r.items)?))]))
}

fn aggregate_wire(value: &str) -> Result<f64, CallError> {
    let number: f64 = value.parse().map_err(|e| CallError::Internal(anyhow::anyhow!("invalid market aggregate: {e}")))?;
    if !number.is_finite() { return Err(CallError::Internal(anyhow::anyhow!("non-finite market aggregate"))); }
    Ok(number)
}

pub(super) fn history_header() -> DbRowDescriptor {
    DbRowDescriptor::new([
        ("historyDate", DbType::FileTime),
        ("lowPrice", DbType::R8),
        ("highPrice", DbType::R8),
        ("avgPrice", DbType::R8),
        ("volume", DbType::I8),
        ("orders", DbType::I4),
    ])
}

/// Daily trading in the session's region for each type, from the year's transactions:
/// typeID -> (days before today, today) as `dbutil.CRowset`s (marketsvc.GetHistoryRowList).
pub(super) async fn price_history(c: &Client, type_ids: &[i32]) -> Result<HashMap<i32, (Value, Value)>, CallError> {
    let region = c.session.int("regionid").unwrap_or(0);
    let today = today();
    let rows = listings::price_history(&c.db, region, type_ids, FILETIME_DAY, today.saturating_sub(crate::config::game().market.price_history_days.saturating_mul(FILETIME_DAY))).await.map_err(db_err)?;
    history_rows(type_ids, today, &rows)
}

fn history_rows(type_ids: &[i32], today: i64, rows: &[listings::PriceHistory]) -> Result<HashMap<i32, (Value, Value)>, CallError> {
    let header = history_header();
    let mut days: HashMap<i32, (Vec<Value>, Vec<Value>)> = type_ids.iter().map(|t| (*t, (vec![], vec![]))).collect();
    for r in rows {
        let (type_id, day) = (r.type_id, r.day);
        let count = i32::try_from(r.count).map_err(|_| CallError::Internal(anyhow::anyhow!("market history order count exceeds protocol width")))?;
        let row = header.row(&[Value::Int(day), Value::Float(r.low.wire_isk()), Value::Float(r.high.wire_isk()), Value::Float(aggregate_wire(&r.average)?), Value::Int(r.volume.min(i128::from(i64::MAX)) as i64), Value::Int(i64::from(count))]);
        let (old, new) = days.entry(type_id).or_default();
        (if day < today { old } else { new }).push(row);
    }
    Ok(days.into_iter().map(|(t, (old, new))| (t, (crowset(&header, old), crowset(&header, new)))).collect())
}

#[cfg(test)]
mod tests;
