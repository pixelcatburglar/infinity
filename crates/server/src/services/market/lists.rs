//! Market order and transaction list formatting for marketProxy
//! (`eve/client/script/ui/services/marketsvc.py`), backed by market DAO queries and
//! the startup-loaded SDE map. Matching, order mutation and notification dispatch
//! are handled separately.
use super::*;
/// Columns of an order row; the client sets `jumps` itself (RefreshJumps).
fn order_header() -> DbRowDescriptor {
    DbRowDescriptor::new([
        ("price", DbType::R8),
        ("volRemaining", DbType::I4),
        ("typeID", DbType::I4),
        ("range", DbType::I2),
        ("orderID", DbType::I8),
        ("volEntered", DbType::I4),
        ("minVolume", DbType::I4),
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
/// `GetOrders(typeID)` -> (asks, bids) in the session's region.
pub(super) async fn orders(db: &Db, region: i64, type_id: i64) -> CallResult {
    let rows = listings::orders(db, today(), type_id as i32, region).await.map_err(db_err)?;
    let header = order_header();
    let (mut asks, mut bids) = (vec![], vec![]);
    for r in &rows {
        let line = [Value::Float(r.price), Value::Int(r.vol_remaining), Value::Int(r.type_id), Value::Int(r.range), Value::Int(r.order_id), Value::Int(r.vol_entered), Value::Int(r.min_volume), Value::Bool(r.bid), Value::Int(r.issued), Value::Int(r.duration), Value::Int(r.station_id), Value::Int(r.region_id), Value::Int(r.solar_system_id), Value::Int(r.constellation_id), Value::Int(0)];
        let row = header.row(&line);
        if r.bid {
            bids.push(row)
        } else {
            asks.push(row)
        }
    }
    Ok(Value::tuple([crowset(&header, asks), crowset(&header, bids)]))
}

/// Cheapest ask per type within `column = id`: {typeID: best}.
pub(super) async fn best_asks(db: &Db, area: Area, id: i64, as_rows: bool) -> CallResult {
    let rows = listings::best_asks(db, area, id).await.map_err(db_err)?;
    // Station/system answers are tuples (marketutil.ConvertTuplesToBestByOrders); the
    // region answer is used as-is, by attribute.
    let header = DbRowDescriptor::new([("price", DbType::R8), ("volRemaining", DbType::I4), ("typeID", DbType::I4), ("stationID", DbType::I8)]);
    Ok(Value::Dict(
        rows.iter()
            .map(|r| {
                let line = [Value::Float(r.price), Value::Int(r.vol_remaining), Value::Int(r.type_id), Value::Int(r.station_id)];
                (Value::Int(r.type_id), if as_rows { header.row(&line) } else { Value::tuple(line) })
            })
            .collect(),
    ))
}
// ---------------------------------------------------------------- order lists

/// `GetCharOrders`: the character's open orders.
pub(super) async fn my_orders(c: &Client) -> CallResult {
    let me = c.session.int("charid").unwrap_or(0);
    let here = c.session.int("solarsystemid2").unwrap_or(0);
    let rows = orders::my_orders(&c.db, me, today()).await.map_err(db_err)?;
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
        ("volEntered", DbType::I4),
        ("volRemaining", DbType::I4),
        ("issueDate", DbType::FileTime),
        ("minVolume", DbType::I4),
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
                Value::Int(me),
                Value::Int(o.at.region),
                Value::Int(o.at.id),
                Value::Int(o.at.system),
                Value::Int(o.at.constellation),
                Value::Int(o.range),
                Value::Bool(o.bid),
                Value::Float(o.price),
                Value::Int(o.vol_entered as i64),
                Value::Int(o.vol_remaining as i64),
                Value::Int(o.issued),
                Value::Int(o.min_volume as i64),
                Value::Int(o.duration),
                Value::Bool(false),
                Value::Float(o.escrow),
                Value::Int(ACCOUNT_CASH as i64),
                Value::Int(jumps(&c.sde.map, here, o.at.system, UNREACHABLE).unwrap_or(UNREACHABLE)),
            ])
        })
        .collect();
    Ok(crowset(&header, lines))
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
        ("volEntered", DbType::I4),
        ("volRemaining", DbType::I4),
        ("issueDate", DbType::FileTime),
        ("minVolume", DbType::I4),
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
                Value::Int(me),
                Value::Int(r.region_id),
                Value::Int(r.station_id),
                Value::Int(r.range),
                Value::Bool(r.bid),
                Value::Float(r.price),
                Value::Int(r.vol_entered),
                Value::Int(r.vol_remaining),
                Value::Int(r.issued),
                Value::Int(r.min_volume),
                Value::Int(r.duration),
                Value::Int(r.state),
                Value::Int(r.closed),
                Value::Bool(false),
                Value::Int(ACCOUNT_CASH as i64),
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
    let header = DbRowDescriptor::new([
        ("transactionID", DbType::I8),
        ("transactionDate", DbType::FileTime),
        ("typeID", DbType::I4),
        ("quantity", DbType::I4),
        ("price", DbType::R8),
        ("stationID", DbType::I8),
        ("buyerID", DbType::I8),
        ("sellerID", DbType::I8),
        ("accountID", DbType::I4),
        ("buyerAccountID", DbType::I4),
        ("sellerAccountID", DbType::I4),
    ]);
    let lines = rows
        .iter()
        .map(|r| {
            header.row(&[
                Value::Int(r.transaction_id),
                Value::Int(r.date),
                Value::Int(r.type_id),
                Value::Int(r.quantity),
                Value::Float(r.price),
                Value::Int(r.station_id),
                Value::Int(r.buyer_id),
                Value::Int(r.seller_id),
                Value::Int(ACCOUNT_CASH as i64),
                Value::Int(ACCOUNT_CASH as i64),
                Value::Int(ACCOUNT_CASH as i64),
            ])
        })
        .collect();
    Ok(crowset(&header, lines))
}

/// `GetCharEscrow`: ISK held by buy orders, and the value of items held by sell orders.
pub(super) async fn escrow(c: &Client) -> CallResult {
    let me = c.session.int("charid").unwrap_or(0);
    let r = listings::escrow(&c.db, me).await.map_err(db_err)?;
    Ok(keyval([("iskEscrow", Value::Float(r.isk)), ("itemsEscrow", Value::Float(r.items))]))
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
    let header = history_header();
    let mut days: HashMap<i32, (Vec<Value>, Vec<Value>)> = type_ids.iter().map(|t| (*t, (vec![], vec![]))).collect();
    for r in &rows {
        let (type_id, day) = (r.type_id, r.day);
        let row = header.row(&[Value::Int(day), Value::Float(r.low), Value::Float(r.high), Value::Float(r.average), Value::Int(r.volume), Value::Int(r.count)]);
        let (old, new) = days.entry(type_id).or_default();
        (if day < today { old } else { new }).push(row);
    }
    Ok(days.into_iter().map(|(t, (old, new))| (t, (crowset(&header, old), crowset(&header, new)))).collect())
}
