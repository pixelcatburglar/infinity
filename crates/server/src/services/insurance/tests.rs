use super::price_value;

#[test]
fn insurance_price_prefers_nonzero_retail_then_nullable_base() {
    assert_eq!(price_value(Some(7.0), Some(8.0), Some(9.0)), 7.0);
    assert_eq!(price_value(Some(0.0), Some(8.0), Some(9.0)), 8.0);
    assert_eq!(price_value(None, Some(0.0), Some(9.0)), 9.0);
    assert_eq!(price_value(None, None, Some(0.0)), 0.0);
    assert_eq!(price_value(None, None, None), 0.0);
    assert_eq!(price_value(Some(-1.0), Some(8.0), Some(9.0)), -1.0);
    assert!(price_value(Some(f64::NAN), Some(8.0), Some(9.0)).is_nan());
    assert!(price_value(Some(0.0), Some(f64::NAN), Some(9.0)).is_nan());
}
