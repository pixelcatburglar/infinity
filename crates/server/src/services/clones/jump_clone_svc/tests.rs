use super::*;

#[test]
fn jump_rejects_invalid_wire_arguments() {
    let args = [Value::Int(42), Value::Int(i64::from(i32::MAX) + 1), Value::Float(900_000.0), Value::Bool(false)];
    assert_eq!(jump_arguments(&args), Some((42, i64::from(i32::MAX) + 1, 900_000.0, false)));
    for bad in [Value::Float(f64::NAN), Value::Float(f64::INFINITY), Value::Float(-1.0), Value::Bool(true)] {
        let mut input = args.clone(); input[2] = bad; assert!(jump_arguments(&input).is_none());
    }
    let mut input = args.clone(); input[0] = Value::Bool(true); assert!(jump_arguments(&input).is_none());
    input = args.clone(); input[3] = Value::Int(1); assert!(jump_arguments(&input).is_none());
    assert!(jump_arguments(&args[..3]).is_none());
}

#[test]
fn clone_name_is_unicode_and_bounded_by_characters() {
    let valid = [Value::Int(1), Value::unicode("克".repeat(MAX_CLONE_NAME_LENGTH))];
    assert!(clone_name_arguments(&valid).is_some());
    let long = [Value::Int(1), Value::unicode("克".repeat(MAX_CLONE_NAME_LENGTH + 1))];
    assert!(clone_name_arguments(&long).is_none());
    assert!(clone_name_arguments(&[Value::Int(1), Value::str("name")]).is_none());
}

#[test]
fn clone_rows_preserve_large_clone_identifiers() {
    let id = i64::from(i32::MAX) + 123;
    let value = clone_rows(&[clone_dao::CloneRow {
        clone_id: id, owner_id: 42, location_id: 60000001, name: String::from("克隆"),
    }]);
    let Value::Reduce { data, .. } = value else { panic!("expected CRowset") };
    let parts = data.items().unwrap();
    let args = parts[1].items().unwrap();
    let descriptor = DbRowDescriptor::from_value(&args[0]).unwrap();
    let rows = args[1].items().unwrap();
    assert_eq!(descriptor.get(&rows[0], "jumpCloneID"), Some(Value::Int(id)));
}

#[test]
fn ship_clone_state_requires_no_arguments() {
    assert_eq!(ship_clone_state(&[]).unwrap(), clone_rows(&[]));
    assert!(ship_clone_state(&[Value::Int(1)]).is_err());
}
