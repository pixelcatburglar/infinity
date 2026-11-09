use super::model::{decode, Request};
use crate::services::Call;
use marshal::Value;

fn call<'a>(method: &'a str, args: &'a [Value], kwargs: &'a Value) -> Call<'a> {
    Call { service: "shipAdaptionMgr", method, args, kwargs, bound: false, bind_params: None }
}
fn chip(id: Value, quantity: Value) -> Value {
    Value::instance("utillib.KeyVal", [("type_id", id), ("quantity", quantity)])
}

#[test]
fn strict_integer_identity_and_metadata() {
    let none = Value::None;
    for quantity in [Value::Bool(true), Value::Float(1.0), Value::str("1"), Value::Int(0), Value::Int(i64::from(u32::MAX) + 1)] {
        assert!(decode(&call("InjectChips", &[Value::Int(1), quantity], &none), 1, 1, 1).is_none());
    }
    for quantity in [i32::MAX as u32 + 1, u32::MAX] {
        assert_eq!(decode(&call("InjectChips", &[Value::Int(1), Value::Int(i64::from(quantity))], &none), 1, 1, 1), Some(Request::Inject { item: 1, quantity }));
    }
    let args = [Value::Int(2), Value::Int(3)];
    let valid = call("InjectChips", &args, &none);
    assert_eq!(decode(&valid, 1, 1, 1), Some(Request::Inject { item: 2, quantity: 3 }));
    assert!(decode(&valid, 0, 1, 1).is_none());
    assert!(decode(&valid, 1, 0, 1).is_none());
    assert!(decode(&Call { bound: true, ..valid }, 1, 1, 1).is_none());
    let kwargs = Value::dict([("owner", Value::Int(2))]);
    assert!(decode(&call("InjectChips", &args, &kwargs), 1, 1, 1).is_none());
}

#[test]
fn exchange_requires_exact_unique_keyvals_and_bounds() {
    let none = Value::None;
    let valid = chip(Value::Int(3), Value::Int(4));
    let args = [Value::Int(2), Value::List(vec![valid.clone()])];
    assert_eq!(decode(&call("call_chip_exchange", &args, &none), 1, 1, 1), Some(Request::Exchange { issuer: 2, chips: vec![(3, 4)] }));
    assert!(decode(&call("call_chip_exchange", &args, &none), 1, 1, 0).is_none());
    let bad = [
        Value::dict([("type_id", Value::Int(3)), ("quantity", Value::Int(4))]),
        Value::instance("other.KeyVal", [("type_id", Value::Int(3)), ("quantity", Value::Int(4))]),
        Value::Instance(Box::new(Value::str("utillib.KeyVal")), Box::new(Value::Dict(vec![(Value::str("type_id"), Value::Int(3)), (Value::str("type_id"), Value::Int(4))]))),
        Value::Instance(Box::new(Value::unicode("utillib.KeyVal")), Box::new(Value::dict([("type_id", Value::Int(3)), ("quantity", Value::Int(4))]))),
        Value::Instance(Box::new(Value::str("utillib.KeyVal")), Box::new(Value::Dict(vec![(Value::unicode("type_id"), Value::Int(3)), (Value::str("quantity"), Value::Int(4))]))),
        chip(Value::Int(3), Value::Bool(true)),
    ];
    for row in bad {
        let args = [Value::Int(2), Value::List(vec![row])];
        assert!(decode(&call("call_chip_exchange", &args, &none), 1, 1, 1).is_none());
    }
    let args = [Value::Int(2), Value::List(vec![valid.clone(), valid])];
    assert!(decode(&call("call_chip_exchange", &args, &none), 1, 1, 2).is_none());
}

#[test]
fn opposite_exchange_orders_normalize_before_planning() {
    let none = Value::None;
    let low = chip(Value::Int(3), Value::Int(4));
    let high = chip(Value::Int(7), Value::Int(2));
    let forward = [Value::Int(2), Value::List(vec![low.clone(), high.clone()])];
    let reverse = [Value::Int(2), Value::List(vec![high, low])];
    let expected = Some(Request::Exchange { issuer: 2, chips: vec![(3, 4), (7, 2)] });
    assert_eq!(decode(&call("call_chip_exchange", &forward, &none), 1, 1, 2), expected);
    assert_eq!(decode(&call("call_chip_exchange", &reverse, &none), 1, 1, 2), expected);
}

#[test]
fn exchange_provenance_uses_the_locked_location() {
    use crate::lua::events::resources::{FactOrigin, LoyaltyReason};
    let location = crate::dao::characters::ship::Location {
        corporation: 10, alliance: Some(11), station: Some(12), system: Some(13), ship: Some(14),
    };
    let mutation = super::operations::exchange_mutation(&location, 15);
    assert_eq!(mutation.reason, LoyaltyReason::ChipExchange);
    assert_eq!(mutation.occurred_at, 15);
    assert_eq!(mutation.context.corporation_id, Some(10));
    assert_eq!(mutation.context.alliance_id, Some(11));
    assert_eq!((mutation.context.station_id, mutation.context.system_id, mutation.context.ship_id), (Some(12), Some(13), Some(14)));
    assert_eq!(mutation.context.origin, FactOrigin::Natural);
}
