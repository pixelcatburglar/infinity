use super::*;

#[test]
fn crash_limits_are_validated_before_delivery() {
    assert_eq!(crashes(&Value::list([])).unwrap().len(), 0);
    assert!(crashes(&Value::list(vec![Value::None; MAX_CRASHES + 1])).is_none());
    assert!(crashes(&Value::list([Value::str("x".repeat(MAX_CRASH_BYTES + 1))])).is_none());
    assert!(crashes(&Value::Int(1)).is_none());
}

#[test]
fn crash_debug_preserves_arbitrary_shapes() {
    let values = vec![
        Value::None,
        Value::Int(-42),
        Value::str(b"escaped\n\xff"),
        Value::unicode("中文"),
        Value::tuple([Value::Bool(true), Value::list([Value::Int(7)])]),
        Value::Dict(vec![(Value::str("key"), Value::unicode("value"))]),
        Value::BigLong(vec![0xff; 16]),
    ];
    let expected: Vec<_> = values.iter().map(|value| format!("{value:?}")).collect();
    assert_eq!(crashes(&Value::list(values)), Some(expected));
}

#[test]
fn crash_debug_enforces_exact_byte_boundary() {
    let overhead = format!("{:?}", Value::unicode("")).len();
    let at_limit = Value::unicode("x".repeat(MAX_CRASH_BYTES - overhead));
    let expected = format!("{at_limit:?}");
    assert_eq!(expected.len(), MAX_CRASH_BYTES);
    assert_eq!(crashes(&Value::list([at_limit])), Some(vec![expected]));

    let over_limit = Value::unicode("x".repeat(MAX_CRASH_BYTES - overhead + 1));
    assert!(crashes(&Value::list([over_limit])).is_none());
}

#[test]
fn oversized_escaped_and_nested_crashes_reject_the_batch() {
    let escaped = Value::str(vec![0xff; MAX_CRASH_BYTES]);
    let nested = Value::tuple([Value::list([escaped])]);
    assert!(crashes(&Value::list([Value::Int(1), nested])).is_none());

    let mut text = CrashText(String::new());
    assert!(write!(&mut text, "{:?}", Value::str(vec![0xff; MAX_CRASH_BYTES])).is_err());
    assert!(text.0.len() <= MAX_CRASH_BYTES);
}
