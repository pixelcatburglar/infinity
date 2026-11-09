use super::*;
use crate::isk::IskAmount;

fn rowset(value: &Value) -> (DbRowDescriptor, &[Value]) {
    let Value::Reduce { data, .. } = value else { panic!("expected CRowset") };
    let parts = data.items().unwrap();
    let args = parts[1].items().unwrap();
    (DbRowDescriptor::from_value(&args[0]).unwrap(), args[1].items().unwrap())
}

fn order(bid: bool) -> Order {
    Order {
        id: 4_000_000_001, type_id: infinity_core::consts::TYPE_PLEX, char_id: Some(12),
        at: Station { id: 600, system: 300, constellation: 200, region: 100, owner: 0 },
        corporation_id: None, wallet_key: 1000, source_location_id: None, source_flag_id: None,
        price: IskAmount::parse_decimal("123.45").unwrap(), vol_entered: 9, vol_remaining: 7,
        min_volume: 1, bid, range: -1, duration: 30, issued: Some(123456), escrow: IskAmount::ZERO,
        scope: orders::MarketScope::GlobalPlex, vault_user_id: None, plex_escrow: 0,
        created_at: None, expires_at: None,
    }
}

#[test]
fn book_rows_preserve_widths_locations_and_separate_sides() {
    let book = book_rows(&[order(false), order(true)]);
    let decoded = marshal::loads(&marshal::dumps(&book)).unwrap();
    for (side, expected_bid) in decoded.items().unwrap().iter().zip([false, true]) {
        let (header, rows) = rowset(side);
        assert_eq!(header.columns, order_header().columns);
        assert_eq!(rows.len(), 1);
        for (field, expected) in [("orderID", Value::Int(4_000_000_001)), ("range", Value::Int(-1)), ("issueDate", Value::Int(123456)), ("regionID", Value::Int(100)), ("solarSystemID", Value::Int(300)), ("stationID", Value::Int(600)), ("price", Value::Float(123.45)), ("bid", Value::Bool(expected_bid))] {
            assert_eq!(header.get(&rows[0], field), Some(expected));
        }
    }
    let empty = book_rows(&[]);
    for side in empty.items().unwrap() { let (header, rows) = rowset(side); assert_eq!(header.columns, order_header().columns); assert!(rows.is_empty()); }
    assert_eq!(order_header().columns[3].1, DbType::I2);
    assert_eq!(order_header().columns[4].1, DbType::I8);
    assert_eq!(order_header().columns[8].1, DbType::FileTime);
    assert_eq!(best_header().columns.len(), 4);
    let wire = marshal::dumps(&book);
    let decoded = marshal::loads(&wire).unwrap();
    let mut captured_rows = Vec::new();
    for (side_index, side) in decoded.items().unwrap().iter().enumerate() {
        let (header, rows) = rowset(side);
        for (row_index, row) in rows.iter().enumerate() {
            let Value::DbRow { data, objects, .. } = row else { panic!("expected DBRow") };
            assert!(objects.is_empty());
            let values: Vec<_> = header.columns.iter().map(|(name, _)| {
                let value = header.get(row, name).unwrap();
                match value {
                    Value::Int(v) => serde_json::json!({"name": name, "type": "int", "value": v.to_string()}),
                    Value::Bool(v) => serde_json::json!({"name": name, "type": "bool", "value": v}),
                    Value::Float(v) => serde_json::json!({"name": name, "type": "float", "value": v.to_string(), "bits": format!("{:016x}", v.to_bits())}),
                    Value::None => serde_json::json!({"name": name, "type": "none"}),
                    _ => panic!("unexpected order row value"),
                }
            }).collect();
            captured_rows.push(serde_json::json!({
                "path": [side_index, row_index],
                "columns": header.columns.iter().map(|(name, ty)| serde_json::json!([name, *ty as i32])).collect::<Vec<_>>(),
                "virtual_columns": header.virtual_columns.iter().map(|(name, callable)| serde_json::json!([name, callable])).collect::<Vec<_>>(),
                "values": values, "fixed_hex": data.iter().map(|byte| format!("{byte:02x}")).collect::<String>(), "objects": []
            }));
        }
    }
    let wire_hex: String = wire.iter().map(|byte| format!("{byte:02x}")).collect();
    println!("CLIENT_QUANTITY_INPUT_V1 {}", serde_json::json!({
        "version": 1, "id": "market-order-book", "scenario": "book_both_sides",
        "producer_test": "book_rows_preserve_widths_locations_and_separate_sides",
        "producer_symbols": ["book_rows", "order_header"], "kind": "order_book", "sequence": 0,
        "recipient": null, "session": { "source": "fixture_context" },
        "payload_scope": "book_rows", "wire_hex": wire_hex, "wire_size": wire.len(),
        "before": { "source": "fixture_context" }, "after": { "source": "server_output" },
        "rows": captured_rows
    }));
}

#[test]
fn npc_issue_date_uses_utc_day_fallback_and_preserves_persisted_date() {
    let before = today();
    let mut npc = order(false);
    npc.char_id = None;
    npc.issued = None;
    let book = book_rows(&[npc.clone()]);
    let after = today();
    let (header, rows) = rowset(&book.items().unwrap()[0]);
    let issued = header.get(&rows[0], "issueDate").unwrap();
    assert!(before > 0);
    assert!(issued == Value::Int(before) || issued == Value::Int(after));
    npc.issued = Some(before - FILETIME_DAY);
    let book = book_rows(&[npc]);
    let (header, rows) = rowset(&book.items().unwrap()[0]);
    assert_eq!(header.get(&rows[0], "issueDate"), Some(Value::Int(before - FILETIME_DAY)));
}

#[test]
fn exact_projection_and_unbounded_aggregate_are_distinct() {
    for price in ["1234567.89", "0.01", "-0.01"] {
        assert_eq!(IskAmount::parse_decimal(price).unwrap().wire_isk(), aggregate_wire(price).unwrap());
    }
    assert!(aggregate_wire("99999999999999999999999999999999999999999999999").unwrap().is_finite());
    assert!(aggregate_wire("NaN").is_err());
    assert!(aggregate_wire("1e9999").is_err());
    assert!(aggregate_wire("bad").is_err());
}

#[test]
fn history_keeps_empty_keys_real_days_and_checked_counts() {
    let id = infinity_core::consts::TYPE_PLEX;
    let make = |day, count| listings::PriceHistory { type_id: id, day, low: IskAmount::parse_decimal("2.00").unwrap(), high: IskAmount::parse_decimal("5.00").unwrap(), average: "3.25".into(), volume: 5_000_000_000, count };
    let empty = history_rows(&[id], 20, &[]).unwrap();
    let (old, new) = empty.get(&id).unwrap();
    assert!(rowset(old).1.is_empty() && rowset(new).1.is_empty());
    let map = history_rows(&[id], 20, &[make(10, 3), make(20, 4)]).unwrap();
    let (old, new) = map.get(&id).unwrap();
    for (side, day, count) in [(old, 10, 3), (new, 20, 4)] {
        let (header, rows) = rowset(side);
        assert_eq!(header.columns[4].1, DbType::I8);
        assert_eq!(header.columns[5].1, DbType::I4);
        assert_eq!(rows.len(), 1);
        assert_eq!(header.get(&rows[0], "historyDate"), Some(Value::Int(day)));
        assert_eq!(header.get(&rows[0], "avgPrice"), Some(Value::Float(3.25)));
        assert_eq!(header.get(&rows[0], "volume"), Some(Value::Int(5_000_000_000)));
        assert_eq!(header.get(&rows[0], "orders"), Some(Value::Int(count)));
    }
    assert!(history_rows(&[id], 20, &[make(20, i64::from(i32::MAX) + 1)]).is_err());
}
