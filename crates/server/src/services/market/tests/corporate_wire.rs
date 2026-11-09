//! Corporate order wire fields produced by the market encoder.
use super::*;

#[test]
fn corporate_order_keyval_round_trips_issuer_division_and_remainder() {
    let order = Order { id: 123, type_id: 456, char_id: Some(789), corporation_id: Some(987), wallet_key: ACCOUNT_CASH + 1, source_location_id: None, source_flag_id: None, at: Station { id: 10, system: 20, constellation: 30, region: 40, owner: 50 }, price: crate::isk::IskAmount::parse_decimal("100").unwrap(), vol_entered: 5, vol_remaining: 2, min_volume: 1, bid: true, range: RANGE_STATION, duration: 1, issued: Some(100), escrow: crate::isk::IskAmount::parse_decimal("200").unwrap(), scope: orders::MarketScope::Regional, vault_user_id: None, plex_escrow: 0, created_at: Some(100), expires_at: Some(100 + FILETIME_DAY) };
    let wire = marshal::loads(&marshal::dumps(&order_keyval(&order))).unwrap();
    for (field, expected) in [("orderID", Value::Int(order.id)), ("typeID", Value::Int(i64::from(order.type_id))), ("charID", Value::Int(order.char_id.unwrap())), ("keyID", Value::Int(i64::from(order.wallet_key))), ("isCorp", Value::Bool(true)), ("volRemaining", Value::Int(2))] {
        assert_eq!(crate::services::value_field(&wire, field), Some(&expected));
    }
}
