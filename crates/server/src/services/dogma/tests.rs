use super::*;
use super::info::{active_effect, cap_client_quantity, overlay_overrides, ship_info, ship_state_core, wire_quantity, ONLINE_EFFECT_DURATION, ONLINE_EFFECT_REPEAT};
use crate::services::{combat, fit, item_descriptor};
use crate::dao::items::loading::Item;
use infinity_core::consts::{dogma::{ATTR_CAPACITOR_CAPACITY, ATTR_CHARGE}, BLUEPRINT_COPY_QUANTITY, CLIENT_MAX_SHORT_AMOUNT, SINGLETON_QUANTITY, TYPE_CHARACTER, TYPE_PLASTIC_WRAP};
use marshal::Value;
use std::collections::BTreeMap;

#[path = "../combat/groups_health/tests/fixture.rs"]
mod fixture;
mod capacitor_priming_db;
mod boarding;

#[test]
fn item_attribute_union_overlays_and_adds() {
    let mut attrs = BTreeMap::from([(1, 10.0), (2, 20.0)]);
    overlay_overrides(&mut attrs, [(2, 30.0), (3, 40.0)]);
    assert_eq!(attrs, BTreeMap::from([(1, 10.0), (2, 30.0), (3, 40.0)]));
}

#[test]
fn overload_active_effect_has_godma_environment_shape() {
    let (key, value) = active_effect(10, 20, 30, 3001);
    assert_eq!(key, Value::Int(3001));
    let Value::Tuple(fields) = value else { panic!("expected effect tuple") };
    assert_eq!(fields.len(), 10);
    assert_eq!(&fields[..3], &[Value::Int(10), Value::Int(20), Value::Int(30)]);
    assert_eq!(fields[6], Value::Int(3001));
    assert_eq!(fields[8], Value::Int(ONLINE_EFFECT_DURATION));
    assert_eq!(fields[9], Value::Int(ONLINE_EFFECT_REPEAT));
}

#[test]
fn item_row_preserves_type_aware_wire_quantity() {
    let mut item = Item {
        item_id: 1, type_id: TYPE_PLASTIC_WRAP, owner_id: 2, location_id: 3,
        flag_id: 4, quantity: -500_000, singleton: true, group_id: 5,
        category_id: 6, custom_info: None,
    };
    let descriptor = item_descriptor();
    assert_eq!(descriptor.get(&item.row(), "quantity"), Some(Value::Int(-500_000)));
    item.type_id = TYPE_CHARACTER;
    assert_eq!(descriptor.get(&item.row(), "quantity"), Some(Value::Int(SINGLETON_QUANTITY)));
    item.quantity = BLUEPRINT_COPY_QUANTITY;
    assert_eq!(descriptor.get(&item.row(), "quantity"), Some(Value::Int(BLUEPRINT_COPY_QUANTITY)));
    item.singleton = false;
    item.quantity = 42;
    assert_eq!(descriptor.get(&item.row(), "quantity"), Some(Value::Int(42)));
}

#[test]
fn wire_quantity_caps_only_ordinary_stacks_above_the_client_limit() {
    let limit = CLIENT_MAX_SHORT_AMOUNT;
    for (quantity, expected) in [
        (0, 0),
        (1, 1),
        (42, 42),
        (limit - 1, limit - 1),
        (limit, limit),
        (limit + 1, limit),
        (i64::MAX, limit),
        (-1, -1),
        (-2, -2),
        (i64::MIN, i64::MIN),
    ] {
        assert_eq!(cap_client_quantity(quantity), expected, "{quantity}");
        assert_eq!(wire_quantity(TYPE_CHARACTER, false, quantity), expected, "{quantity}");
    }
    for (type_id, quantity, expected) in [
        (TYPE_PLASTIC_WRAP, -500_000, -500_000),
        (TYPE_PLASTIC_WRAP, -(limit + 1), -(limit + 1)),
        (TYPE_PLASTIC_WRAP, i64::MIN, i64::MIN),
        (TYPE_CHARACTER, 1, SINGLETON_QUANTITY),
        (TYPE_CHARACTER, i64::MAX, SINGLETON_QUANTITY),
        (TYPE_CHARACTER, BLUEPRINT_COPY_QUANTITY, BLUEPRINT_COPY_QUANTITY),
        (TYPE_PLASTIC_WRAP, BLUEPRINT_COPY_QUANTITY, BLUEPRINT_COPY_QUANTITY),
    ] {
        assert_eq!(wire_quantity(type_id, true, quantity), expected, "{type_id} {quantity}");
    }
}
