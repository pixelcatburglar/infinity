use crate::dao::items::loading::Item;
use infinity_core::consts::CLIENT_MAX_SHORT_AMOUNT;
use marshal::{DbRowDescriptor, DbType, Value};

#[test]
fn item_rows_roundtrip_wide_and_ordinary_categories() {
    for (sequence, (category, singleton, quantity, wire_quantity, custom_info)) in [
        (2100, false, 7, 7, Some("fixture")),
        (2100, false, 1_i64 << 31, 1_i64 << 31, Some("fixture")),
        (6, false, 1_i64 << 32, 1_i64 << 32, None),
        (2100, false, (1_i64 << 53) + 1, CLIENT_MAX_SHORT_AMOUNT, Some("fixture")),
        (6, false, i64::MAX, CLIENT_MAX_SHORT_AMOUNT, None),
        (6, true, 1, -1, None),
    ].into_iter().enumerate() {
        let item = Item {
            item_id: 1_000_000_000_123, type_id: 42, owner_id: 90_000_001,
            location_id: 1_000_000_000_456, flag_id: 5, quantity, singleton,
            group_id: 123, category_id: category, custom_info: custom_info.map(str::to_owned),
        };
        let values = [
            Value::Int(item.item_id), Value::Int(i64::from(item.type_id)), Value::Int(item.owner_id),
            Value::Int(item.location_id), Value::Int(i64::from(item.flag_id)), Value::Int(wire_quantity),
            Value::Int(item.group_id), Value::Int(category),
            custom_info.map(Value::str).unwrap_or(Value::None),
        ];
        let encoded = marshal::dumps(&item.row());
        let decoded = marshal::loads(&encoded).unwrap();
        let Value::DbRow { descriptor, data, objects } = &decoded else { panic!("expected DBRow") };
        let descriptor = DbRowDescriptor::from_value(descriptor).unwrap();
        assert_eq!(descriptor.columns, vec![
            ("itemID".into(), DbType::I8), ("typeID".into(), DbType::I4),
            ("ownerID".into(), DbType::I4), ("locationID".into(), DbType::I8),
            ("flagID".into(), DbType::I2), ("quantity".into(), DbType::I8),
            ("groupID".into(), DbType::I2), ("categoryID".into(), DbType::I4),
            ("customInfo".into(), DbType::Str),
        ]);
        assert_eq!(descriptor.virtual_columns, vec![
            ("stacksize".into(), "eve.common.script.sys.eveCfg.StackSize".into()),
            ("singleton".into(), "eve.common.script.sys.eveCfg.Singleton".into()),
        ]);
        for ((name, _), expected) in descriptor.columns.iter().zip(&values) {
            assert_eq!(descriptor.get(&decoded, name), Some(expected.clone()), "{name}");
        }
        assert_eq!(data.len(), 42);
        assert_eq!(&data[16..24], &wire_quantity.to_le_bytes());
        let mut old = descriptor.clone();
        old.columns[7].1 = DbType::I1;
        let old_row = old.row(&values);
        let Value::DbRow { data: old_data, .. } = &old_row else { panic!("expected DBRow") };
        assert_eq!(old_data.len(), 39);
        println!("category {category}: old/new encoded lengths {}/{}; fixed lengths 39/42", marshal::dumps(&old_row).len(), encoded.len());
        let hex = |bytes: &[u8]| bytes.iter().map(|byte| format!("{byte:02x}")).collect::<String>();
        let typed = |value: Value| match value {
            Value::Int(value) => serde_json::json!({"type": "int", "value": value.to_string()}),
            Value::Bytes(value) => serde_json::json!({"type": "str", "hex": hex(&value)}),
            Value::None => serde_json::json!({"type": "none"}),
            other => panic!("unexpected row scalar: {other:?}"),
        };
        let columns: Vec<_> = descriptor.columns.iter().map(|(name, kind)| {
            serde_json::json!({"name": name, "db_type_code": *kind as i32})
        }).collect();
        let virtual_columns: Vec<_> = descriptor.virtual_columns.iter().map(|(name, callable)| {
            serde_json::json!({"name": name, "callable": callable})
        }).collect();
        let decoded_values: Vec<_> = descriptor.columns.iter().map(|(name, _)| {
            typed(descriptor.get(&decoded, name).expect("row column"))
        }).collect();
        let objects: Vec<_> = objects.iter().cloned().map(typed).collect();
        println!("CLIENT_QUANTITY_INPUT_V1 {}", serde_json::json!({
            "version": 1, "id": format!("item-row-{sequence}"), "scenario": "item_row",
            "producer_test": "services::tests::item_descriptor::item_rows_roundtrip_wide_and_ordinary_categories",
            "producer_symbols": ["Item::row", "item_descriptor", "marshal::dumps", "marshal::loads"],
            "kind": "item_row", "sequence": sequence, "recipient": null, "session": null,
            "event": null, "payload_scope": "item_row", "wire_hex": hex(&encoded),
            "wire_size": encoded.len(),
            "before": {"source": "fixture_context", "item_id": item.item_id.to_string(),
                "quantity": item.quantity.to_string(), "singleton": item.singleton},
            "after": {"source": "server_output"},
            "rows": [{"path": [], "columns": columns, "virtual_columns": virtual_columns,
                "values": decoded_values, "fixed_hex": hex(data), "objects": objects}]
        }));
    }
}
