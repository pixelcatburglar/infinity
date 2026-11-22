use super::*;

mod db_smoke;
mod fit_db;
pub(in crate::services::effects) mod module_admission;

#[test]
fn primary_sensor_keeps_first_tie_and_ignores_invalid_values() {
    assert_eq!(primary([10.0, 10.0, 2.0, 1.0]), Some((0, 10.0)));
    assert_eq!(primary([f64::NAN, 1.0, 3.0, 2.0]), Some((2, 3.0)));
    assert_eq!(primary([0.0, -1.0, f64::INFINITY, f64::NAN]), Some((2, f64::INFINITY)));
    assert_eq!(JAM_STRENGTH[primary([1.0, 2.0, 4.0, 3.0]).unwrap().0], 240);
}

#[test]
fn chance_scales_with_defense_and_falloff() {
    assert_eq!(chance(5.0, 10.0, 1.0), 0.5);
    assert_eq!(chance(5.0, 20.0, 1.0), 0.25);
    assert_eq!(chance(5.0, 10.0, range_multiplier(20.0, 10.0, 10.0)), 0.25);
    assert_eq!(chance(20.0, 10.0, 1.0), 1.0);
    for invalid in [0.0, -1.0, f64::NAN, f64::INFINITY] {
        assert_eq!(chance(invalid, 10.0, 1.0), 0.0);
        assert_eq!(chance(5.0, invalid, 1.0), 0.0);
        assert_eq!(chance(5.0, 10.0, invalid), 0.0);
    }
}

#[test]
fn duration_prefers_valid_type_value_and_otherwise_uses_cycle() {
    assert_eq!(choose_duration(Some(8000.0), 20000.0), 8000.0);
    for value in [None, Some(0.0), Some(-1.0), Some(f64::NAN)] {
        assert_eq!(choose_duration(value, 20000.0), 20000.0);
    }
}
