use super::*;

#[test]
fn scan_reports_cargo_and_fitted_hardware_separately() {
    assert!(visible_flag(true, FLAG_CARGO));
    assert!(!visible_flag(false, FLAG_CARGO));
    assert!(visible_flag(false, infinity_core::consts::FLAG_FITTING_FIRST));
    assert!(!visible_flag(true, infinity_core::consts::FLAG_FITTING_FIRST));
}
