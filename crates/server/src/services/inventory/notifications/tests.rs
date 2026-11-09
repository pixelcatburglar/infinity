use super::*;
use infinity_core::consts::{CLIENT_MAX_SHORT_AMOUNT, IX_FLAG, IX_OWNER_ID, IX_SINGLETON};

#[test]
fn item_change_dict_caps_only_int_quantity_columns() {
    let beyond = i64::MAX;
    let change = vec![
        (IX_STACK_SIZE, Value::Int(beyond)),
        (IX_QUANTITY, Value::Int(beyond)),
        (IX_LOCATION_ID, Value::Int(beyond)),
        (IX_OWNER_ID, Value::Int(beyond)),
        (IX_QUANTITY - 1, Value::Int(beyond)),
        (IX_QUANTITY, Value::Int(-2)),
        (IX_SINGLETON, Value::Bool(true)),
        (IX_STACK_SIZE, Value::None),
        (IX_FLAG, Value::Int(4)),
    ];
    let Value::Dict(pairs) = item_change_dict(change) else { panic!("expected dict") };
    assert_eq!(pairs, vec![
        (Value::Int(IX_STACK_SIZE), Value::Int(CLIENT_MAX_SHORT_AMOUNT)),
        (Value::Int(IX_QUANTITY), Value::Int(CLIENT_MAX_SHORT_AMOUNT)),
        (Value::Int(IX_LOCATION_ID), Value::Int(beyond)),
        (Value::Int(IX_OWNER_ID), Value::Int(beyond)),
        (Value::Int(IX_QUANTITY - 1), Value::Int(beyond)),
        (Value::Int(IX_QUANTITY), Value::Int(-2)),
        (Value::Int(IX_SINGLETON), Value::Bool(true)),
        (Value::Int(IX_STACK_SIZE), Value::None),
        (Value::Int(IX_FLAG), Value::Int(4)),
    ]);
}
