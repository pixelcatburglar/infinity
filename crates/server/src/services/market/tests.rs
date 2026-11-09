use super::*;
use super::fees::{adjusted_broker_rate, broker_fee, FeeRate};
use infinity_core::consts::ACCOUNT_CASH;

mod broker_standings;
mod accounting;
mod escrow;
mod rollback;
mod corporate_fixture;
mod corporate_trading;
mod corporate_lifecycle;
mod corporate_permissions;
mod corporate_wire;
mod batch_results;
mod immediate_prices;
mod stack_quantities;
async fn modify_request(c: &Client, id: i64, price: f64) -> ModifyRequest {
    let o = orders::my_order(&c.db, c.session.int("charid").unwrap(), id).await.unwrap().unwrap();
    ModifyRequest { order_id: id, new_price: price, bid: o.bid, station_id: o.at.id,
        system_id: o.at.system, old_price: o.price.wire_isk(), range: o.range,
        remaining: i64::from(o.vol_remaining), issued: o.issued.unwrap_or_else(today),
        requested_scope: o.scope }
}

fn buy_line(station: i64, ty: i32, price: f64, quantity: i64) -> Value {
    crate::services::keyval([
        ("stationID", Value::Int(station)), ("typeID", Value::Int(i64::from(ty))),
        ("price", Value::Float(price)), ("quantity", Value::Int(quantity)),
    ])
}

fn sell_line(station: i64, ty: i32, item: i64, price: f64, quantity: i64, office: Option<i64>) -> Value {
    crate::services::keyval([
        ("stationID", Value::Int(station)), ("typeID", Value::Int(i64::from(ty))),
        ("itemID", Value::Int(item)), ("price", Value::Float(price)),
        ("quantity", Value::Int(quantity)), ("officeID", office.map_or(Value::None, Value::Int)),
    ])
}

async fn buy_rpc(c: &mut Client, station: i64, rows: Vec<Value>, corporate: Value) -> CallResult {
    let args = [Value::Int(station), Value::List(rows), corporate];
    let kwargs = Value::None;
    let call = Call { service: "marketProxy", method: "BuyMultipleItems", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
    super::market_proxy::dispatch(c, &call).await.unwrap()
}

async fn sell_rpc(c: &mut Client, rows: Vec<Value>, corporate: bool, duration: i64) -> CallResult {
    let args = [Value::List(rows), Value::Bool(corporate), Value::Int(duration), Value::None];
    let kwargs = Value::None;
    let call = Call { service: "marketProxy", method: "PlaceMultiSellOrder", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
    super::market_proxy::dispatch(c, &call).await.unwrap()
}

fn cached_result(value: &Value, regional: bool) -> Value {
    let Value::Instance(class, state) = value else { panic!("expected cached book") };
    assert_eq!(**class, Value::str("carbon.common.script.net.objectCaching.CachedMethodCallResult"));
    let parts = state.items().unwrap();
    assert_eq!(parts.len(), 3);
    assert_eq!(parts[0].get("versionCheck"), Some(&Value::str("always")));
    let region_id = Value::str("regionid");
    assert_eq!(parts[0].get("sessionInfo"), if regional { Some(&region_id) } else { None });
    let Value::Bytes(bytes) = &parts[1] else { panic!("expected marshalled book") };
    marshal::loads(bytes).unwrap()
}

async fn cleanup_market_fixture(db: &Db, f: crate::services::tests::fixture::Fixture) -> Result<(), crate::db::Error> {
    db.execute("DELETE FROM game.wallet_journal WHERE account_id IN (SELECT account_id FROM game.wallet_accounts WHERE owner_id=$1) OR market_transaction_id IN (SELECT transaction_id FROM game.market_transactions WHERE buyer_id=$1 OR seller_id=$1)", &[&f.character]).await?;
    db.execute("DELETE FROM game.market_transactions WHERE buyer_id=$1 OR seller_id=$1", &[&f.character]).await?;
    db.execute("DELETE FROM game.wallet_accounts WHERE owner_id=$1", &[&f.character]).await?;
    db.execute("DELETE FROM game.market_orders WHERE char_id=$1", &[&f.character]).await?;
    db.execute("DELETE FROM game.market_order_history WHERE char_id=$1", &[&f.character]).await?;
    crate::services::tests::fixture::cleanup(db, f).await
}

#[test]
fn station_preserves_location_and_defaults_null_owner() {
    let mut row = crate::sde::map::Station {
        station_id: 10, type_id: None, owner_id: None, solar_system_id: Some(20),
        constellation_id: Some(30), region_id: Some(40), operation_id: None,
        x: None, y: None, z: None, security: None,
    };
    let at = market_station(&row);
    assert_eq!((at.id, at.system, at.constellation, at.region, at.owner), (10, 20, 30, 40, 0));
    row.owner_id = Some(50);
    assert_eq!(market_station(&row).owner, 50);
}

#[test]
fn broker_fee_applies_minimum_and_rounds() {
    crate::tests::init_config();
    let rate = FeeRate::from_input(0.03).unwrap();
    let amount = crate::isk::IskAmount::parse_decimal;
    assert_eq!(broker_fee(rate, amount("10").unwrap()).unwrap(), amount("100").unwrap());
    assert_eq!(broker_fee(rate, amount("4001").unwrap()).unwrap(), amount("120.03").unwrap());
}

#[test]
fn broker_standings_adjust_rate_without_clamping() {
    crate::tests::init_config();
    let amount = crate::isk::IskAmount::parse_decimal;
    let base = FeeRate::from_input(0.015).unwrap();
    let million = amount("1000000").unwrap();
    assert_eq!(broker_fee(adjusted_broker_rate(base, 3.0, 5.0).unwrap(), million).unwrap(), amount("13100").unwrap());
    assert_eq!(broker_fee(adjusted_broker_rate(base, 0.0, 0.0).unwrap(), million).unwrap(), amount("15000").unwrap());
    assert_eq!(broker_fee(adjusted_broker_rate(base, -3.0, -5.0).unwrap(), million).unwrap(), amount("16900").unwrap());
    let negative = adjusted_broker_rate(FeeRate::from_input(0.001).unwrap(), 3.0, 5.0).unwrap();
    assert!(negative.as_f64() < 0.0);
    assert_eq!(broker_fee(negative, million).unwrap(), amount("100").unwrap());
    assert_eq!(broker_fee(adjusted_broker_rate(base, 3.0, 5.0).unwrap(), amount("10001").unwrap()).unwrap(), amount("131.01").unwrap());
}

#[test]
fn duration_obeys_policy_and_timestamp_bounds() {
    crate::tests::init_config();
    assert!(valid_duration(90, 0));
    assert!(!valid_duration(91, 0));
    assert!(!valid_duration(-1, 0));
    assert!(!valid_duration(1, i64::MAX));
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn plex_orders_match_regional_order_book() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let (mut client, _peer) = crate::services::tests::client(db, 1).await;
    let region = client.sde.map.stations().find_map(|station| station.region_id).expect("SDE region");
    let kwargs = Value::None;
    let args = [Value::Int(i64::from(infinity_core::consts::TYPE_PLEX))];
    let mut first_book = None;
    for region in [region, -1] {
        client.session.apply(vec![("regionid", Value::Int(region))]);
        let plex_call = Call { service: "marketProxy", method: "GetPlexOrders", args: &[], kwargs: &kwargs, bound: false, bind_params: None };
        let orders_call = Call { service: "marketProxy", method: "GetOrders", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
        let plex = dispatch(&mut client, &plex_call).await.unwrap().unwrap();
        let ordinary = dispatch(&mut client, &orders_call).await.unwrap().unwrap();
        let plex = cached_result(&plex, false);
        assert_eq!(plex, cached_result(&ordinary, true));
        if let Some(first) = &first_book { assert_eq!(&plex, first); }
        else { first_book = Some(plex.clone()); }
        let Value::Tuple(sides) = &plex else { panic!("expected order book tuple") };
        assert_eq!(sides.len(), 2);
        for side in sides {
            let Value::Reduce { data, list, dict } = side else { panic!("expected CRowset") };
            assert!(list.is_empty() && dict.is_empty());
            let parts = data.items().unwrap();
            assert_eq!(parts[0], Value::global("carbon.common.script.sys.crowset.CRowset"));
            let rowset_args = parts[1].items().unwrap();
            assert_eq!(rowset_args.len(), 2);
            let descriptor = DbRowDescriptor::from_value(&rowset_args[0]).unwrap();
            assert_eq!(descriptor.columns.len(), 15);
            assert!(rowset_args[1].items().is_some());
        }
    }
}
