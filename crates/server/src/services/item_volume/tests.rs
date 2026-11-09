//! Behavioral checks for client item-volume rules using synthetic SDE fields.
//! The calculation mirrors `inventorycommon/util.py:30-59` and its override tables;
//! database loading and item creation are outside these tests.

use super::*;

#[test]
fn override_tables_are_strictly_sorted_by_id() {
    assert!(overrides::TYPE_OVERRIDES.windows(2).all(|pair| pair[0].0 < pair[1].0));
    assert!(overrides::GROUP_OVERRIDES.windows(2).all(|pair| pair[0].0 < pair[1].0));
}

#[test]
fn packaged_type_precedes_group_then_raw_volume() {
    // Porpoise has a type override and belongs to the industrial-command group.
    assert_eq!(calculate(42244, Some(941), 123.0, false, 1, None).unwrap(), 50_000.0);
    assert_eq!(calculate(587, Some(25), 28_600.0, false, 2, None).unwrap(), 5_000.0);
    assert_eq!(calculate(587, None, 123.0, false, 2, None).unwrap(), 246.0);
}

#[test]
fn assembled_and_quantity_rules() {
    assert_eq!(calculate(587, Some(25), 28_600.0, true, 2, None).unwrap(), 28_600.0);
    assert_eq!(calculate(587, Some(25), 28_600.0, false, 2, Some(-5)).unwrap(), 2_500.0);
    assert_eq!(calculate(587, Some(25), 28_600.0, false, 2, Some(0)).unwrap(), 0.0);
    assert_eq!(calculate(587, None, -1.0, false, 12, Some(0)).unwrap(), -1.0);
}

#[test]
fn plastic_wrap_uses_stored_quantity_and_rejects_nonpositive_volume() {
    assert_eq!(calculate(TYPE_PLASTIC_WRAP, None, 1.0, true, -1234, None).unwrap(), 12.34);
    assert_eq!(calculate(TYPE_PLASTIC_WRAP, None, 1.0, true, -1234, Some(2)).unwrap(), 24.68);
    assert!(calculate(TYPE_PLASTIC_WRAP, None, 1.0, true, 0, None).is_err());
    assert!(calculate(TYPE_PLASTIC_WRAP, None, 1.0, true, 1, None).is_err());
    assert!(calculate(TYPE_PLASTIC_WRAP, None, 1.0, true, i64::MIN, None).is_ok());
}

#[test]
fn logical_stack_size_ignores_singleton_encoding() {
    assert_eq!(stack_size(false, 42), 42);
    assert_eq!(stack_size(true, 1), 1);
    assert_eq!(stack_size(true, infinity_core::consts::BLUEPRINT_COPY_QUANTITY), 1);
    assert_eq!(stack_size(true, -500_000), 1);
}

#[test]
fn wrap_encoding_rounds_up_and_has_a_minimum() {
    assert_eq!(encode_wrap_quantity(5000.0).unwrap(), -500_000);
    assert_eq!(encode_wrap_quantity(1.001).unwrap(), -101);
    assert_eq!(encode_wrap_quantity(0.0).unwrap(), -1);
    assert_eq!(encode_wrap_quantity(f64::MIN_POSITIVE).unwrap(), -1);
    assert_eq!(encode_wrap_quantity(21_474_836.47).unwrap(), -i64::from(i32::MAX));
}

#[test]
fn wrap_encoding_rejects_invalid_and_out_of_range_volume() {
    for volume in [-1.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY, 21_474_836.48, f64::MAX] {
        assert!(matches!(encode_wrap_quantity(volume), Err(CallError::Internal(_))));
    }
}
