use super::*;
use crate::dao::lp_offers::RequiredItem;

fn offer() -> Offer {
    Offer { corporation_id: 7, offer_id: 8, type_id: 9, quantity: 3, lp_cost: 10, isk_cost: 20, ak_cost: 2,
        required_items: vec![RequiredItem { type_id: 11, quantity: 2 }, RequiredItem { type_id: 12, quantity: 1 }, RequiredItem { type_id: 11, quantity: 3 }] }
}
fn item(id: i64, kind: i32, quantity: i64) -> Item {
    Item { item_id: id, type_id: kind, owner_id: 1, location_id: 2, flag_id: 4, quantity, singleton: false, group_id: 0, category_id: 0, custom_info: None }
}

#[test]
fn wire_preserves_requirements_and_costs() {
    let Value::Instance(_, fields) = wire(offer()) else { panic!("expected KeyVal") };
    assert_eq!(fields.get("offerID"), Some(&Value::Int(8)));
    assert_eq!(fields.get("qty"), Some(&Value::Int(3)));
    assert_eq!(fields.get("akCost"), Some(&Value::Int(2)));
    assert_eq!(fields.get("reqItems"), Some(&Value::list([
        Value::tuple([Value::Int(11), Value::Int(2)]), Value::tuple([Value::Int(12), Value::Int(1)]), Value::tuple([Value::Int(11), Value::Int(3)])
    ])));
}

#[test]
fn scaling_combines_duplicate_costs_and_rejects_invalid_values() {
    let o = offer();
    let cost = scale(&o, 2).unwrap();
    assert_eq!((cost.lp, cost.isk, cost.output), (20, 40, 6));
    assert_eq!(cost.items.get(&11), Some(&10));
    assert_eq!(cost.items.get(&12), Some(&2));
    assert!(scale(&o, 0).is_none());
    assert!(scale(&o, MAX_OFFER_COUNT + 1).is_none());
    let mut bad = o.clone(); bad.quantity = i32::MAX;
    assert_eq!(scale(&bad, 2).unwrap().output, i64::from(i32::MAX) * 2);
    bad = o.clone(); bad.lp_cost = i64::MAX;
    assert!(scale(&bad, 2).is_none());
    bad = o; bad.isk_cost = 9_007_199_254_740_993;
    assert_eq!(scale(&bad, 1).unwrap().isk, 9_007_199_254_740_993);
    bad.isk_cost = i64::MAX;
    assert!(scale(&bad, 2).is_none());
}

#[test]
fn stack_plan_consumes_in_input_order_and_refuses_shortfall() {
    let stacks = vec![item(1, 11, 4), item(2, 12, 2), item(3, 11, 9)];
    let requirements = BTreeMap::from([(11, 10), (12, 2)]);
    let changes = plan(&stacks, &requirements).unwrap();
    assert_eq!(changes[&1].quantity, None);
    assert_eq!(changes[&2].quantity, None);
    assert_eq!(changes[&3].quantity, Some(3));
    assert!(plan(&stacks, &BTreeMap::from([(11, 14)])).is_none());
    let large = i64::from(i32::MAX) + 1;
    assert_eq!(plan(&[item(4, 11, large)], &BTreeMap::from([(11, 1)])).unwrap()[&4].quantity, Some(large - 1));
}

#[test]
fn corporation_division_keys_are_bounded_before_authority_checks() {
    let first = infinity_core::consts::ACCOUNT_CASH;
    assert!(super::super::super::corpwallet::valid_key(first));
    assert!(super::super::super::corpwallet::valid_key(first + 6));
    assert!(!super::super::super::corpwallet::valid_key(first - 1));
    assert!(!super::super::super::corpwallet::valid_key(i32::MAX));
    assert!(!super::super::super::corpwallet::valid_key(i32::MIN));
}

mod database;
