use super::{fees, validate_quote, job_deadline, job_seconds, FILETIME_SECOND, MANUFACTURING, RESEARCH_TIME, RESEARCH_MATERIAL};
use marshal::Value;

#[test]
fn duration_multiplier_applies_before_rounding_and_duration_guard() {
    assert_eq!(job_seconds(10.5, 0.5, 1, 100.0).ok(), Some(5));
    assert_eq!(job_seconds(10.5, 1.0, 1, 100.0).ok(), Some(11));
    assert_eq!(job_seconds(10.5, 2.0, 1, 100.0).ok(), Some(21));
    assert!(job_seconds(100.0, 2.0, 2, 75.0).is_err());
    assert!(job_seconds(100.0, 0.5, 2, 75.0).is_ok());
}

#[test]
fn duration_rejects_nonfinite_and_unrepresentable_results() {
    assert!(job_seconds(f64::MAX, 2.0, 1, f64::MAX).is_err());
    assert!(job_seconds(i64::MAX as f64, 1.0, 1, f64::MAX).is_err());
    assert!(job_seconds(f64::NAN, 1.0, 1, f64::MAX).is_err());
}

#[test]
fn deadline_adds_duration_to_start() {
    assert_eq!(job_deadline(123, 2).ok(), Some(123 + 2 * FILETIME_SECOND));
}

#[test]
fn deadline_rejects_duration_multiplication_overflow() {
    assert!(job_deadline(0, i64::MAX / FILETIME_SECOND + 1).is_err());
}

#[test]
fn deadline_rejects_timestamp_addition_overflow() {
    assert!(job_deadline(i64::MAX, 1).is_err());
}

#[test]
fn fees_round_components_independently_and_index_unrounded_cost() {
    let f = fees(10.6, MANUFACTURING, 0.45, 0.1, 0.14, 0.14, 0.14);
    assert_eq!(f.indexed_cost, 1.0);
    assert_eq!(f.facility_tax, 2.0);
    assert_eq!(f.scc, 2.0);
    assert_eq!(f.security_tax, 2.0);
    assert_eq!(f.total(), 7.0);
    let fractional = fees(10.6, MANUFACTURING, 0.44, 0.14, 0.0, 0.0, 0.14);
    assert_eq!(fractional.indexed_cost, 1.0);
    assert_eq!(fractional.security_tax, 0.0);
}

#[test]
fn research_discount_applies_to_scc_only() {
    for activity in [RESEARCH_TIME, RESEARCH_MATERIAL] {
        let f = fees(100.0, activity, 0.5, 0.1, 0.1, 0.1, 0.1);
        assert_eq!((f.indexed_cost, f.facility_tax, f.scc, f.security_tax, f.total()),
            (10.0, 10.0, 5.0, 10.0, 35.0));
    }
}

#[test]
fn separate_optional_quotes_must_be_finite_numeric_and_match() {
    let f = fees(100.0, MANUFACTURING, 0.5, 0.1, 0.2, 0.1, 0.1);
    assert!(validate_quote(None, None, &f).is_ok());
    assert!(validate_quote(Some(&Value::Int(10)), Some(&Value::Float(20.0)), &f).is_ok());
    assert!(validate_quote(Some(&Value::Int(11)), None, &f).is_err());
    assert!(validate_quote(None, Some(&Value::Int(21)), &f).is_err());
    for bad in [Value::Float(f64::NAN), Value::Float(f64::INFINITY), Value::str("10"), Value::None, Value::Bool(true)] {
        assert!(validate_quote(Some(&bad), None, &f).is_err());
        assert!(validate_quote(None, Some(&bad), &f).is_err());
    }
}

#[test]
fn manufacturing_output_uses_recipe_quantity_and_checks_overflow() {
    for (per_run, runs, expected) in [(4, 3, 12), (i64::from(i32::MAX), 2, i64::from(i32::MAX) * 2), (i64::MAX, 1, i64::MAX)] {
        assert_eq!(super::manufacturing_quantity(per_run, runs).ok(), Some(expected));
    }
    assert!(super::manufacturing_quantity(0, 3).is_err());
    assert!(super::manufacturing_quantity(i64::MAX, 2).is_err());
}
