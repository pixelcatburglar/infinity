use super::*;

fn sell_item(ty: i64) -> Value {
    crate::services::keyval([
        ("stationID", Value::Int(11)), ("typeID", Value::Int(ty)),
        ("itemID", Value::Int(12)), ("price", Value::Float(100.0)),
        ("quantity", Value::Int(3)), ("officeID", Value::Int(13)),
        ("delta", Value::Float(-4.0)), ("rawBrokerFeePercentage", Value::Float(0.9)),
    ])
}

#[test]
fn dedicated_sell_preserves_source_and_fee_and_rejects_other_types() {
    let args = [sell_item(i64::from(TYPE_PLEX)), Value::Bool(true), Value::Int(7), Value::Float(0.03)];
    let (r, corporate) = plex_sell_request(&args).unwrap();
    assert!(corporate);
    assert_eq!((r.station_id, r.type_id, r.item_id, r.quantity, r.duration, r.office_id), (11, i64::from(TYPE_PLEX), 12, 3, 7, Some(13)));
    assert_eq!((r.price, r.expected_brokers_fee), (100.0, Some(0.03)));
    assert!(plex_sell_request(&[sell_item(1), Value::Bool(false), Value::Int(7), Value::None]).is_err());
    assert!(plex_sell_request(&args[..3]).is_err());
    assert!(plex_sell_request(&[sell_item(i64::from(TYPE_PLEX)), Value::Int(1), Value::Int(7), Value::None]).is_err());
}

#[test]
fn modify_keeps_all_fields_and_requested_scope() {
    let args = [Value::Int(1), Value::Float(2.0), Value::Bool(true), Value::Int(3), Value::Int(4), Value::Float(5.0), Value::Int(6), Value::Int(7), Value::Int(8)];
    for scope in [MarketScope::Regional, MarketScope::GlobalPlex] {
        let r = modify_request(&args, scope).unwrap();
        assert_eq!(r.requested_scope, scope);
        assert_eq!((r.order_id, r.new_price, r.bid, r.station_id, r.system_id, r.old_price, r.range, r.remaining, r.issued), (1, 2.0, true, 3, 4, 5.0, 6, 7, 8));
    }
    assert!(modify_request(&args[..8], MarketScope::Regional).is_err());
    let mut bad = args.clone();
    bad[2] = Value::Int(1);
    assert!(modify_request(&bad, MarketScope::GlobalPlex).is_err());
}

#[test]
fn checked_type_ids_and_wire_numbers_reject_invalid_values() {
    assert_eq!(type_id(Some(&Value::Int(42))).unwrap(), 42);
    for v in [Value::Int(0), Value::Int(-1), Value::Int(i64::MAX), Value::Float(1.0), Value::None] {
        assert!(type_id(Some(&v)).is_err());
    }
    assert!(number(Some(&Value::Float(f64::NAN))).is_err());
    assert!(optional_integer(Some(&Value::Float(1.0))).is_err());
}
