use super::*;
use marshal::Value;
use crate::services::sovereignty::queries::Query;

#[test]
fn bound_scope_requires_exact_shape_and_empty_arguments() {
    let kwargs = Value::Dict(vec![]);
    let valid = Value::tuple([Value::Int(42), Value::Int(1)]);
    let call = Call { service: "allianceRegistry", method: "GetCapitalSystemInfo", args: &[], kwargs: &kwargs, bound: true, bind_params: Some(&valid) };
    assert!(matches!(sovereignty_query(&call, Some(3), Some(7)), Ok(Query::CapitalSystemInfo { alliance_id: 42 })));
    for bad in [Value::Int(42), Value::list([Value::Int(42), Value::Int(1)]), Value::tuple([Value::Int(42), Value::Bool(true)]), Value::tuple([Value::Bool(true), Value::Int(1)]), Value::tuple([Value::Int(42), Value::Int(2)])] {
        assert!(sovereignty_query(&Call { bind_params: Some(&bad), ..call }, Some(3), Some(7)).is_err());
    }
    assert!(sovereignty_query(&Call { bound: false, ..call }, Some(3), Some(7)).is_err());
    assert!(sovereignty_query(&Call { service: "other", ..call }, Some(3), Some(7)).is_err());
    assert!(sovereignty_query(&Call { args: &[Value::Int(42)], ..call }, Some(3), Some(7)).is_err());
    let kw = Value::dict([("allianceID", Value::Int(42))]);
    assert!(sovereignty_query(&Call { kwargs: &kw, ..call }, Some(3), Some(7)).is_err());
    assert!(sovereignty_query(&call, None, Some(7)).is_err());
}

#[test]
fn local_scope_is_captured_and_claims_are_exact() {
    let kwargs = Value::Dict(vec![]);
    let call = Call { service: "sovMgr", method: "GetSovStructuresInfoForLocalSolarSystem", args: &[], kwargs: &kwargs, bound: false, bind_params: None };
    assert!(matches!(sovereignty_query(&call, Some(3), Some(7)), Ok(Query::Structures { system_id: 7 })));
    assert!(sovereignty_query(&Call { args: &[Value::Int(8)], ..call }, Some(3), Some(7)).is_err());
    assert!(sovereignty_query(&call, Some(3), None).is_err());
    assert!(sovereignty_claimed(&call));
    assert!(!sovereignty_claimed(&Call { method: "GetAlliance", service: "allianceRegistry", ..call }));
}
