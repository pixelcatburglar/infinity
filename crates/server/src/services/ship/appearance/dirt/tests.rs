//! Unit checks for the dirt ratio curve: timestamp resolution, finite
//! saturation and the consumer's zero-age clamp on future timestamps.
use super::*;

const NOW: i64 = 133_456_789_012_345_679; // FILETIME with sub-millisecond ticks

#[test]
fn clear_and_curve_points_match_client_formula() {
    assert_eq!(from_ratio(0.0, NOW).unwrap().timestamp, NOW); // a fractional-ms now stays exact
    let half = from_ratio(RATIO_BREAKPOINT, NOW).unwrap();
    assert!(half.attained_level.abs() < 1e-9 && (half.attained_ratio - RATIO_BREAKPOINT).abs() < 1e-9);
    // evegraphics utils.py: the previewer's exact 0.55 debug sentinel is an ordinary point here.
    let preview = from_ratio(RATIO_BREAKPOINT + 0.55 / HIGH_LEVEL_SLOPE, NOW).unwrap();
    assert!((preview.attained_level - 0.55).abs() < 1e-9);
    let full = from_ratio(1.0, NOW).unwrap();
    assert_eq!((full.timestamp, full.saturated), (0, true));
    assert!(full.attained_ratio < 1.0 && full.attained_level < MAX_LEVEL);
}

#[test]
fn boundaries_clamp_and_refuse() {
    for bad in [-0.25, 1.25, f64::NAN, f64::INFINITY] { assert!(from_ratio(bad, NOW).is_err()); }
    let future = describe(NOW + 1, NOW).unwrap(); // the consumer clamps a negative age to zero
    assert_eq!((future.attained_level, future.attained_ratio, future.saturated), (LOW_LEVEL_OFFSET, 0.0, false));
    assert_eq!(from_ratio(0.0, i64::MAX).unwrap().timestamp, i64::MAX);
    assert_eq!(from_ratio(1.0, i64::MAX).unwrap().timestamp, 0);
}
