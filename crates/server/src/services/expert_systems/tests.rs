use super::*;

#[test]
fn envelope_rejects_bound_and_nonempty_or_invalid_kwargs() {
    let empty = Value::Dict(vec![]);
    let kwargs = Value::Dict(vec![(Value::unicode("unexpected"), Value::Int(1))]);
    let invalid = Value::Int(1);
    let none = Value::None;
    let call = Call { service: "expertSystemMgr", method: "GetMyExpertSystems", args: &[], kwargs: &empty, bound: false, bind_params: None };
    assert!(valid_envelope(&call));
    assert!(valid_envelope(&Call { kwargs: &none, ..call }));
    assert!(!valid_envelope(&Call { kwargs: &kwargs, ..call }));
    assert!(!valid_envelope(&Call { kwargs: &invalid, ..call }));
    assert!(!valid_envelope(&Call { bound: true, ..call }));
}
