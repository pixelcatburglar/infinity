//! `marketProxy` dispatch for `eve/client/script/ui/services/marketsvc.py` and the
//! multi-buy and multi-sell clients. Uses market orders, transactions and loaded SDE data.
//! Corporate reads require current wallet access; writes reauthorize the selected division.
//! Multi-buy is one bounded transaction; multi-sell commits children independently.
//! Structure markets are unsupported.
use super::{Call, CallError, CallResult, Client, Value, RANGE_STATION, BuyRequest, SellRequest, ModifyRequest};
use super::lifecycle::{cancel_order, modify_order};
use super::lists::{best_asks, corporation_orders, corporation_transactions, escrow, my_orders, order_history, orders, plex_best, price_history, transactions};
use super::trading::{place_buy, place_buy_batch, place_sell, validate_buy, validate_sell};
use infinity_core::consts::TYPE_PLEX;
use crate::dao::market::listings::Area;
use crate::dao::market::orders::MarketScope;
use crate::services::{identity::Identity, value_field};
use std::collections::HashSet;

/// Client buySellMultiBase default order cap when no server override is supplied.
const MAX_MULTI_BUY_ITEMS: usize = 100;

fn invalid() -> CallError { crate::services::user_error(infinity_core::messages::MSG_MKT_ORDER_DID_NOT_MATCH, vec![]) }
fn integer(v: Option<&Value>) -> Result<i64, CallError> {
    match v { Some(Value::Int(n)) => Ok(*n), _ => Err(invalid()) }
}
fn number(v: Option<&Value>) -> Result<f64, CallError> {
    match v { Some(Value::Float(n)) if n.is_finite() => Ok(*n), Some(Value::Int(n)) => Ok(*n as f64), _ => Err(invalid()) }
}
fn boolean(v: Option<&Value>) -> Result<bool, CallError> {
    match v { Some(Value::Bool(b)) => Ok(*b), _ => Err(invalid()) }
}
fn optional_number(v: Option<&Value>) -> Result<Option<f64>, CallError> {
    match v { None | Some(Value::None) => Ok(None), _ => number(v).map(Some) }
}
fn optional_integer(v: Option<&Value>) -> Result<Option<i64>, CallError> {
    match v { None | Some(Value::None) => Ok(None), _ => integer(v).map(Some) }
}
fn type_id(v: Option<&Value>) -> Result<i32, CallError> {
    i32::try_from(integer(v)?).ok().filter(|&id| id > 0).ok_or_else(invalid)
}
fn sell_request(item: &Value, duration: i64, fee: Option<f64>) -> Result<SellRequest, CallError> {
    Ok(SellRequest { item_id: integer(value_field(item, "itemID"))?,
        station_id: integer(value_field(item, "stationID"))?, type_id: integer(value_field(item, "typeID"))?,
        price: number(value_field(item, "price"))?, quantity: integer(value_field(item, "quantity"))?,
        duration, office_id: optional_integer(value_field(item, "officeID"))?, expected_brokers_fee: fee })
}
fn plex_sell_request(a: &[Value]) -> Result<(SellRequest, bool), CallError> {
    if a.len() != 4 { return Err(invalid()); }
    let corporate = boolean(a.get(1))?;
    let request = sell_request(&a[0], integer(a.get(2))?, optional_number(a.get(3))?)?;
    if request.type_id != i64::from(TYPE_PLEX) { return Err(invalid()); }
    Ok((request, corporate))
}
fn modify_request(a: &[Value], scope: MarketScope) -> Result<ModifyRequest, CallError> {
    if a.len() != 9 { return Err(invalid()); }
    Ok(ModifyRequest { order_id: integer(a.get(0))?, new_price: number(a.get(1))?,
        bid: boolean(a.get(2))?, station_id: integer(a.get(3))?, system_id: integer(a.get(4))?,
        old_price: number(a.get(5))?, range: integer(a.get(6))?, remaining: integer(a.get(7))?,
        issued: integer(a.get(8))?, requested_scope: scope })
}
fn wallet(c: &Client, corporate: bool) -> Result<Option<i32>, CallError> {
    if !corporate { return Ok(None); }
    c.session.int("corpAccountKey").and_then(|v| i32::try_from(v).ok())
        .filter(|&v| crate::services::corpwallet::valid_key(v)).map(Some)
        .ok_or_else(|| crate::services::notify_error(infinity_core::messages::DEPARTMENT_WALLET_WITHDRAW_UNAUTHORIZED))
}

async fn write_call(c: &Client, call: &Call<'_>) -> CallResult {
    let a = call.args;
    let identity = Identity::of_client(c);
    let expected = match call.method {
        "PlaceBuyOrder" | "ModifyCharOrder" | "ModifyPlexCharOrder" => 9,
        "BuyMultipleItems" => 3,
        "PlaceMultiSellOrder" | "PlacePlexSellOrder" => 4,
        "CancelCharOrder" => 2,
        _ => return Err(invalid()),
    };
    if a.len() != expected { return Err(invalid()); }
    match call.method {
        "PlaceBuyOrder" => {
            let request = BuyRequest {
                station_id: integer(a.get(0))?, type_id: integer(a.get(1))?, price: number(a.get(2))?,
                quantity: integer(a.get(3))?, range: integer(a.get(4))?, min_volume: integer(a.get(5))?,
                duration: integer(a.get(6))?, expected_brokers_fee: optional_number(a.get(8))?,
            };
            let division = wallet(c, boolean(a.get(7))?)?;
            place_buy(&identity, &c.db, &c.world, &c.sde, request, division).await?;
            Ok(Value::None)
        }
        "BuyMultipleItems" => {
            let _station = integer(a.get(0))?;
            let items = a.get(1).and_then(Value::items).ok_or_else(invalid)?;
            if items.is_empty() || items.len() > MAX_MULTI_BUY_ITEMS {
                return Err(crate::services::user_error(infinity_core::messages::MSG_TOO_MANY_ITEMS_FOR_ORDER, vec![]));
            }
            let division = wallet(c, boolean(a.get(2))?)?;
            let mut seen = HashSet::new();
            let mut requests = Vec::with_capacity(items.len());
            for item in items {
                let request = BuyRequest { station_id: integer(value_field(item, "stationID"))?,
                    type_id: integer(value_field(item, "typeID"))?, price: number(value_field(item, "price"))?,
                    quantity: integer(value_field(item, "quantity"))?, range: RANGE_STATION, min_volume: 1,
                    duration: 0, expected_brokers_fee: None };
                validate_buy(request)?;
                if !seen.insert(request.type_id) { return Err(invalid()); }
                requests.push(request);
            }
            let orders = place_buy_batch(&identity, &c.db, &c.world, &c.sde, requests, division).await?;
            Ok(Value::List(orders.iter().map(super::order_keyval).collect()))
        }
        "PlaceMultiSellOrder" => {
            let items = a.first().and_then(Value::items).ok_or_else(invalid)?;
            if items.is_empty() { return Err(invalid()); }
            let division = wallet(c, boolean(a.get(1))?)?;
            let duration = integer(a.get(2))?;
            let fee = optional_number(a.get(3))?;
            let mut seen = HashSet::new();
            let mut requests = Vec::with_capacity(items.len());
            for item in items {
                let request = sell_request(item, duration, fee)?;
                validate_sell(request)?;
                if !seen.insert(request.item_id) { return Err(invalid()); }
                requests.push(request);
            }
            let mut any = false;
            let mut failures = Vec::new();
            for request in &requests {
                match place_sell(&identity, &c.db, &c.world, &c.sde, *request, division).await {
                    Ok(made) => any |= made,
                    Err(e @ CallError::User { .. }) if requests.len() == 1 => return Err(e),
                    Err(CallError::User { .. }) => failures.push((request.item_id, request.type_id, request.quantity)),
                    Err(e) => return Err(e),
                }
            }
            if !failures.is_empty() { return Err(crate::services::notify_error(&infinity_core::messages::market::sell_failures(&failures))); }
            Ok(Value::Bool(any))
        }
        // eve/client/script/ui/shared/market/sellMulti.py: SellItems
        "PlacePlexSellOrder" => {
            let (request, corporate) = plex_sell_request(a)?;
            let division = wallet(c, corporate)?;
            validate_sell(request)?;
            Ok(Value::Bool(place_sell(&identity, &c.db, &c.world, &c.sde, request, division).await?))
        }
        "ModifyCharOrder" | "ModifyPlexCharOrder" => {
            let scope = if call.method == "ModifyPlexCharOrder" { MarketScope::GlobalPlex } else { MarketScope::Regional };
            let request = modify_request(a, scope)?;
            modify_order(&identity, &c.db, &c.world, &c.sde, request).await?;
            Ok(Value::None)
        }
        "CancelCharOrder" => {
            cancel_order(&identity, &c.db, &c.world, &c.sde, integer(a.get(0))?, integer(a.get(1))?).await?;
            Ok(Value::None)
        }
        _ => Err(invalid()),
    }
}

pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    if call.service != "marketProxy" {
        return None;
    }
    if matches!(call.method, "PlaceBuyOrder" | "BuyMultipleItems" | "PlaceMultiSellOrder" | "PlacePlexSellOrder" | "ModifyCharOrder" | "ModifyPlexCharOrder" | "CancelCharOrder") {
        return Some(write_call(c, call).await);
    }
    let valid_arity = match call.method {
        "GetOrders" | "GetHistoryForManyTypeIDs" | "GetOldPriceHistory" | "GetNewPriceHistory" => call.args.len() == 1,
        "CharGetTransactions" => call.args.len() == 1,
        "CorpGetTransactions" => call.args.len() == 2,
        "GetPlexOrders" | "GetStationAsks" | "GetSystemAsks" | "GetRegionBest" | "StartupCheck" |
        "GetCharEscrow" | "GetPlexBest" | "GetPlexHistory" | "GetPlexOldPriceHistory" |
        "GetPlexNewPriceHistory" | "GetCharOrders" | "GetCorporationOrders" | "GetMarketOrderHistory" => call.args.is_empty(),
        _ => return None,
    };
    if !valid_arity { return Some(Err(invalid())); }
    Some(async { match call.method {
        "GetOrders" => orders(&c.db, c.session.int("regionid").unwrap_or(0), type_id(call.args.first())?, true).await,
        "GetPlexOrders" => orders(&c.db, c.session.int("regionid").unwrap_or(0), TYPE_PLEX, false).await,
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
        "StartupCheck" => Ok(Value::None),
        "GetCharEscrow" => escrow(c).await,
        "GetPlexBest" => plex_best(&c.db).await,
        // eve/client/script/ui/services/marketsvc.py: GetHistoryForManyTypeIDs
        "GetPlexHistory" | "GetHistoryForManyTypeIDs" => {
            let type_ids = if call.method == "GetPlexHistory" {
                vec![TYPE_PLEX]
            } else {
                call.args[0].items().ok_or_else(invalid)?.iter().map(|v| type_id(Some(v))).collect::<Result<Vec<_>, _>>()?
            };
            price_history(c, &type_ids).await.map(|h| Value::Dict(h.into_iter()
                .map(|(t, (old, new))| (Value::Int(i64::from(t)), Value::tuple([old, new]))).collect()))
        }
        "GetOldPriceHistory" | "GetNewPriceHistory" | "GetPlexOldPriceHistory" | "GetPlexNewPriceHistory" => {
            let id = if matches!(call.method, "GetPlexOldPriceHistory" | "GetPlexNewPriceHistory") {
                TYPE_PLEX
            } else { type_id(call.args.first())? };
            let (old, new) = price_history(c, &[id]).await?.remove(&id).ok_or_else(invalid)?;
            Ok(if matches!(call.method, "GetOldPriceHistory" | "GetPlexOldPriceHistory") { old } else { new })
        }
        "CharGetTransactions" => async {
            transactions(c, optional_integer(call.args.first())?).await
        }.await,
        "CorpGetTransactions" => async {
            let from = optional_integer(call.args.first())?;
            let key = optional_integer(call.args.get(1)).map_err(|_| super::authorization::read_denied())?.map(|v| {
                i32::try_from(v).ok().filter(|&key| crate::services::corpwallet::valid_key(key))
                    .ok_or_else(super::authorization::read_denied)
            }).transpose()?;
            corporation_transactions(c, from, key).await
        }.await,
        "GetCharOrders" => my_orders(c).await,
        "GetCorporationOrders" => corporation_orders(c).await,
        "GetMarketOrderHistory" => order_history(c).await,
        _ => unreachable!(),
    }}.await)
}

#[cfg(test)]
mod tests;
