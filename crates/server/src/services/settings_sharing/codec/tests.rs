use super::*;

#[test]
fn deterministic_utf8_escaping_hash_and_limits() {
    let value = Value::dict([("z", Value::list([Value::Int(2), Value::Int(1)])), ("a", Value::unicode("中\"\n"))]);
    let (encoded, hash) = encode(&value, 1000, 3).unwrap();
    assert_eq!(encoded, "[[\"a\",\"中\\\"\\n\"],[\"z\",[2,1]]]");
    assert_eq!(hash, "23adee4c0dbc3d557825dc8d076493aff1e994677c3e316fc824a6d8f50ba89c");
    assert!(encode(&value, encoded.len(), 3).is_ok());
    assert!(encode(&value, encoded.len() - 1, 3).is_err());
    assert!(encode(&value, 1000, 2).is_err());
}

#[test]
fn unsupported_and_duplicate_values_are_rejected() {
    for value in [Value::Float(f64::NAN), Value::Float(f64::INFINITY), Value::Float(f64::NEG_INFINITY), Value::BigLong(vec![0]), Value::Bytes(vec![255]), Value::global("x"), Value::Dict(vec![(Value::Int(1), Value::None)]), Value::Dict(vec![(Value::str("a"), Value::None), (Value::unicode("a"), Value::None)])] {
        assert!(encode(&Value::dict([("x", value)]), 1000, 64).is_err());
    }
}

#[test]
fn nested_values_preserve_types_and_order() {
    let value = Value::dict([("x", Value::tuple([Value::None, Value::Bool(true), Value::Int(-2), Value::Float(1.5), Value::dict([("b", Value::Int(1)), ("a", Value::Int(2))])]))]);
    assert_eq!(encode(&value, 1000, 64).unwrap().0, "[[\"x\",[null,true,-2,1.5,{\"b\":1,\"a\":2}]]]");
}

#[test]
fn logical_depth_and_nested_pair_order_boundaries() {
    let pairs = Value::dict([("pairs", Value::list([Value::tuple([Value::Int(9), Value::Int(2)]), Value::tuple([Value::Int(1), Value::Int(7)])])), ("empty", Value::list([Value::list([]), Value::Dict(vec![])]))]);
    assert_eq!(encode(&pairs, 1000, 4).unwrap().0, "[[\"empty\",[[],{}]],[\"pairs\",[[9,2],[1,7]]]]");
    let mut nested = Value::None;
    for _ in 0..62 { nested = Value::list([nested]); }
    let at_limit = Value::dict([("x", nested)]);
    assert!(encode(&at_limit, 1000, 64).is_ok());
    assert!(encode(&at_limit, 1000, 63).is_err());
    let beyond = Value::dict([("x", Value::list([at_limit]))]);
    assert!(encode(&beyond, 1000, 64).is_err());
}

#[test]
fn dictionary_metadata_is_bounded_by_payload_limit() {
    let entries = (0..10000).map(|i| (Value::str(i.to_string()), Value::None)).collect();
    assert!(encode(&Value::Dict(entries), 16, 64).is_err());
}

#[test]
fn nested_dictionary_metadata_shares_one_budget() {
    let value = Value::dict([("a", Value::dict([("a", Value::None), ("b", Value::None)])), ("b", Value::None)]);
    let entries_left = Cell::new(3);
    let adapter = Adapter { value: &value, depth: 1, max_depth: 64, entries_left: &entries_left, top: true };
    assert!(serde_json::to_vec(&adapter).is_err());
    assert_eq!(entries_left.get(), 1);
}

#[test]
fn floats_are_decimal_yaml_numbers_and_roundtrip() {
    for number in [1e20, 1e-7, f64::from_bits(1), f64::MAX, -0.0] {
        let (encoded, _) = encode(&Value::dict([("x", Value::Float(number))]), 2000, 2).unwrap();
        let text = &encoded[6..encoded.len() - 2];
        assert!(text.contains('.') && !text.contains('e') && !text.contains('E'));
        assert_eq!(text.parse::<f64>().unwrap().to_bits(), number.to_bits());
    }
}
