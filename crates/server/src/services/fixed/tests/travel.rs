use super::*;

#[test]
fn travel_fixed_shapes_and_fallthrough() {
    let kwargs = Value::Dict(vec![]);
    let call = Call { service: "solarsystemInterferenceMgr", method: "GetLocalInterferenceState", args: &[], kwargs: &kwargs, bound: false, bind_params: None };
    let expected = Value::reduce("solarsysteminterference.solarsystemInterferenceState.SolarsystemInterferenceState", vec![Value::Float(0.0), Value::Int(0), Value::Float(1.0), Value::Int(1), Value::Float(1.0), Value::Float(0.0)], None);
    let value = root(&call).unwrap().unwrap();
    assert_eq!(value, expected);
    assert_eq!(marshal::loads(&marshal::dumps(&value)).unwrap(), expected);
    assert!(root(&Call { method: "Unknown", ..call }).is_none());
    assert!(root(&Call { service: "other", ..call }).is_none());
}
