use super::*;
use crate::consts::FILETIME_HOUR;

#[test]
fn python_two_cooldown_rounding_and_ready_boundaries() {
    for (hours, rounded) in [(2.5, 3), (3.5, 4)] {
        let ready = 42 + rounded * FILETIME_HOUR;
        assert!(!cooldown_ready(ready - 1, Some(42), hours).unwrap());
        assert!(cooldown_ready(ready, Some(42), hours).unwrap());
        assert!(cooldown_ready(ready + 1, Some(42), hours).unwrap());
    }
    assert_eq!((-2.5_f64).round(), -3.0);
    assert!(cooldown_ready(0, Some(0), -0.5).is_err());
    assert!(cooldown_ready(0, None, 3.5).unwrap());
    assert!(!cooldown_ready(i64::MAX, Some(i64::MAX), 1.0).unwrap());
    assert!(cooldown_ready(i64::MAX, Some(i64::MIN), 1.0).unwrap());
    assert!(cooldown_ready(0, Some(0), f64::MAX).is_err());
}

#[test]
fn validation_labels_include_only_the_catalogued_funds_token() {
    assert_eq!(Issue::Docked.label(), Value::str(messages::clones::VALIDATION_DOCKED));
    assert_eq!(Issue::Duplicate.label(), Value::str(messages::clones::VALIDATION_DUPLICATE));
    assert_eq!(Issue::Funds(900_000.0, 0.0).label(), Value::tuple([Value::str(messages::clones::VALIDATION_FUNDS), Value::dict([("cloneCost", Value::Float(900_000.0))])]));
}

#[test]
fn standing_rounding_matches_python_two_binary_float_cases() {
    for (value, expected) in [(2.675, 2.67), (2.985, 2.98), (2.995, 3.0), (3.005, 3.0), (-0.055, -0.06), (-0.045, -0.04), (2.625, 2.63), (-2.625, -2.63)] {
        assert_eq!(rounded_standing(value), expected, "{value}");
    }
    assert_eq!(rounded_standing(0.0).to_bits(), 0.0_f64.to_bits());
    assert_eq!(rounded_standing(-0.0).to_bits(), (-0.0_f64).to_bits());
    assert_eq!(rounded_standing(-f64::MIN_POSITIVE).to_bits(), (-0.0_f64).to_bits());
    assert!(rounded_standing(f64::NAN).is_nan());
    assert_eq!(rounded_standing(f64::MAX), f64::MAX);
}
