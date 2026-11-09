use super::*;

fn pair(id: i64, count: i64) -> Value { Value::tuple([Value::Int(id), Value::Int(count)]) }

#[test]
fn launch_decoder_validates_the_whole_batch() {
    assert!(matches!(parse(&[Value::list([pair(10, 1), pair(11, 2)]), Value::None, Value::Int(1)]), Ok((rows, None)) if rows == vec![(10, 1), (11, 2)]));
    for rows in [vec![pair(10, 1), pair(10, 2)], vec![pair(10, 1), pair(11, i64::from(i32::MAX) + 1)], vec![pair(10, 1), pair(0, 1)]] {
        assert!(parse(&[Value::list(rows), Value::None, Value::Bool(false)]).is_err());
    }
    assert!(parse(&[Value::list([pair(10, 1)]), Value::None, Value::Int(2)]).is_err());
}

#[test]
fn error_encoding_preserves_user_errors_and_propagates_internal_errors() {
    let encoded = error_value(crate::services::user_error("Example", vec![("key".into(), Value::Int(4))])).unwrap();
    assert_eq!(encoded, Value::list([Value::tuple([Value::str("Example"), Value::Dict(vec![(Value::str("key"), Value::Int(4))])])]));
    assert!(matches!(error_value(CallError::Internal(anyhow::anyhow!("failure"))), Err(CallError::Internal(_))));
}
