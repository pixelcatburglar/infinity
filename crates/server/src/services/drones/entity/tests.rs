use super::*;

#[test]
fn command_shapes_and_deduplication() {
    let ids = Value::list([Value::Int(7), Value::Int(7), Value::Int(8)]);
    let (parsed, command) = parse("CmdAssist", &[Value::Int(42), ids.clone()]).ok().unwrap();
    assert_eq!(parsed, vec![7, 8]);
    assert_eq!(command, Command::Order(DroneOrder::Assist(42)));
    assert!(parse("CmdAssist", &[ids.clone(), Value::Int(42)]).is_err());
    assert!(parse("CmdReturnBay", &[Value::list([Value::Bool(true)])]).is_err());
    assert!(parse("CmdReturnBay", &[Value::Int(7)]).is_err());
    assert!(parse("CmdEngage", &[ids.clone(), Value::Bool(true)]).is_err());
    assert_eq!(parse("CmdSalvage", &[ids.clone(), Value::None]).ok().unwrap().1, Command::Order(DroneOrder::Salvage(None)));
    assert_eq!(parse("CmdSalvage", &[ids.clone(), Value::Int(9)]).ok().unwrap().1, Command::Order(DroneOrder::Salvage(Some(9))));
    for args in [&[ids.clone()][..], &[ids.clone(), Value::Int(0)][..], &[ids.clone(), Value::Int(-4)][..],
        &[ids.clone(), Value::Bool(true)][..], &[Value::Int(7), Value::Int(9)][..]] {
        assert!(parse("CmdSalvage", args).is_err(), "{args:?}");
    }
}

#[test]
fn per_id_user_error_wire_shape_and_internal_propagation() {
    let mut errors = Vec::new();
    record_error(&mut errors, 7, crate::services::user_error("Example", vec![("amount".into(), Value::Int(3))])).ok().unwrap();
    assert_eq!(errors, vec![(Value::Int(7), Value::tuple([Value::str("Example"), Value::Dict(vec![(Value::str("amount"), Value::Int(3))])]))]);
    assert!(record_error(&mut errors, 8, CallError::Internal(anyhow::anyhow!("failure"))).is_err());
    assert_eq!(errors.len(), 1);
}


#[test]
fn absent_fleet_members_are_not_equal() {
    assert!(!crate::services::fleets::same_fleet(i64::MAX, i64::MAX - 1));
}