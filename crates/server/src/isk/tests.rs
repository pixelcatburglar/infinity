use super::*;

#[test]
fn exact_decimal_domain_and_format() {
    let max = "9999999999999999999999999999999999.99";
    let a = IskAmount::parse_decimal(max).unwrap();
    assert_eq!(a.decimal(), max);
    assert_eq!(a.checked_neg().unwrap().decimal(), format!("-{max}"));
    assert_eq!(IskAmount::parse_decimal("+0001.2300").unwrap().decimal(), "1.23");
    assert_eq!(IskAmount::parse_decimal("-0.00").unwrap(), IskAmount::ZERO);
    for s in ["", ".1", "1.", "1.001", "NaN", "inf", "1e2", "10000000000000000000000000000000000.00", "--1"] {
        assert!(IskAmount::parse_decimal(s).is_err(), "{s}");
    }
    assert!(a.checked_sub(IskAmount::from_cents(-1).unwrap()).is_none());
    assert!(a.checked_neg().unwrap().checked_sub(IskAmount::from_cents(1).unwrap()).is_none());
    assert!(IskAmount::from_whole_isk(i128::MAX).is_none());
}

#[test]
fn split_precision_preserves_signed_cents_and_wire_boundaries() {
    for (amount, expected) in [
        ("0.00", Some((0, 0))),
        ("0.01", Some((0, 10_000_000))),
        ("0.99", Some((0, 990_000_000))),
        ("1.01", Some((1, 10_000_000))),
        ("-0.01", Some((0, -10_000_000))),
        ("-0.99", Some((0, -990_000_000))),
        ("-1.01", Some((-1, -10_000_000))),
        ("9223372036854775807.99", Some((i64::MAX, 990_000_000))),
        ("9223372036854775808.00", None),
        ("-9223372036854775808.99", Some((i64::MIN, -990_000_000))),
        ("-9223372036854775809.00", None),
        ("9999999999999999999999999999999999.99", None),
        ("-9999999999999999999999999999999999.99", None),
    ] {
        assert_eq!(IskAmount::parse_decimal(amount).unwrap().split_precision(), expected, "{amount}");
    }
}

#[test]
fn floating_conversion_rounds_binary_values_once() {
    for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert!(IskAmount::from_isk(value).is_err());
    }
    let half = 0.005f64;
    let below = f64::from_bits(half.to_bits() - 1);
    let above = f64::from_bits(half.to_bits() + 1);
    for (value, cents) in [(below, 0), (half, 1), (above, 1), (-below, 0), (-half, -1), (-above, -1), (1.005, 100), (-1.005, -100), (0.004, 0), (-0.004, 0)] {
        assert_eq!(IskAmount::from_isk(value).unwrap(), IskAmount::from_cents(cents).unwrap(), "{value}");
    }
    let boundary = 1e34f64;
    let below = f64::from_bits(boundary.to_bits() - 1);
    let above = f64::from_bits(boundary.to_bits() + 1);
    let expected = 999_999_999_999_999_894_846_684_784_341_549_056i128;
    for value in [below, boundary] {
        assert_eq!(IskAmount::from_isk(value).unwrap(), IskAmount::from_cents(expected).unwrap());
        assert_eq!(IskAmount::from_isk(-value).unwrap(), IskAmount::from_cents(-expected).unwrap());
    }
    for value in [above, -above, f64::MAX, -f64::MAX] {
        assert!(IskAmount::from_isk(value).is_err(), "{value}");
    }
}

#[test]
fn serde_and_lua_keep_distinct_boundaries() {
    let amount = IskAmount::parse_decimal("9999999999999999999999999999999999.99").unwrap();
    let json = serde_json::to_string(&amount).unwrap();
    assert_eq!(json, "\"9999999999999999999999999999999999.99\"");
    assert_eq!(serde_json::from_str::<IskAmount>(&json).unwrap(), amount);
    let lua = mlua::Lua::new();
    let value = mlua::IntoLua::into_lua(amount, &lua).unwrap();
    assert!(matches!(value, mlua::Value::Number(n) if n == amount.wire_isk() && n > personal_client_cap().wire_isk()));
}

#[test]
fn checked_market_arithmetic_preserves_full_domain_and_rounds_ties_away() {
    let cent = IskAmount::from_cents(1).unwrap();
    let max = IskAmount::from_cents(MAX_CENTS).unwrap();
    assert_eq!(cent.checked_add(cent), IskAmount::from_cents(2));
    assert!(max.checked_add(cent).is_none());
    assert_eq!(cent.checked_mul(-3), IskAmount::from_cents(-3));
    assert!(max.checked_mul(2).is_none());
    assert_eq!(cent.checked_mul_ratio(1, 2), Some(cent));
    assert_eq!(cent.checked_neg().unwrap().checked_mul_ratio(1, 2), cent.checked_neg());
    assert_eq!(cent.checked_mul_ratio(1, 3), Some(IskAmount::ZERO));
    assert_eq!(max.checked_mul_ratio(u128::MAX, u128::MAX), Some(max));
    assert_eq!(max.checked_mul_ratio(u128::MAX - 1, u128::MAX), Some(max));
    assert_eq!(max.checked_mul_ratio(0, u128::MAX), Some(IskAmount::ZERO));
    assert!(max.checked_mul_ratio(1, 0).is_none());
    assert!(max.checked_mul_ratio(2, 1).is_none());
    assert_eq!(IskAmount::from_cents(3).unwrap().checked_mul_ratio(u128::MAX, u128::MAX), IskAmount::from_cents(3));
}

#[test]
fn weighted_sum_rounds_once_after_signed_combination() {
    let cent = IskAmount::from_cents(1).unwrap();
    let minus_cent = IskAmount::from_cents(-1).unwrap();
    let zero = IskAmount::ZERO;
    for (amount, weight, denominator, expected) in [
        (cent, 1, 2, 1), (minus_cent, 1, 2, -1),
        (cent, 1, 3, 0), (minus_cent, 1, 3, 0),
        (cent, 2, 3, 1), (minus_cent, 2, 3, -1),
    ] {
        assert_eq!(amount.checked_weighted_sum(weight, zero, 0, denominator), IskAmount::from_cents(expected));
    }
    assert_eq!(cent.checked_weighted_sum(1, cent, 1, 2), Some(cent));
    assert_eq!(cent.checked_weighted_sum(1, minus_cent, 1, 2), Some(zero));
    assert_eq!(cent.checked_weighted_sum(0, minus_cent, 0, 1), Some(zero));
    assert_eq!(cent.checked_weighted_sum(0, minus_cent, 1, 1), Some(minus_cent));
    assert!(cent.checked_weighted_sum(0, zero, 0, 0).is_none());
}

#[test]
fn weighted_sum_handles_wide_cancellation_and_final_overflow() {
    let max = IskAmount::from_cents(MAX_CENTS).unwrap();
    let neg_max = max.checked_neg().unwrap();
    let cent = IskAmount::from_cents(1).unwrap();
    assert_eq!(max.checked_weighted_sum(u128::MAX, neg_max, u128::MAX, 1), Some(IskAmount::ZERO));
    assert_eq!(max.checked_weighted_sum(u128::MAX, neg_max, u128::MAX - 1, 1), Some(max));
    assert_eq!(neg_max.checked_weighted_sum(u128::MAX - 1, max, u128::MAX, 1), Some(max));
    assert_eq!(max.checked_weighted_sum(u128::MAX, max, u128::MAX, u128::MAX), None);
    assert_eq!(max.checked_weighted_sum(2, cent, 1, 2), None);
    assert_eq!(neg_max.checked_weighted_sum(2, cent.checked_neg().unwrap(), 1, 2), None);
    assert_eq!(max.checked_weighted_sum(u128::MAX, neg_max, u128::MAX - 1, 2),
        max.checked_mul_ratio(1, 2));
    assert_eq!(cent.checked_mul_ratio(u128::MAX / 2, u128::MAX), Some(IskAmount::ZERO));
    assert_eq!(cent.checked_mul_ratio(u128::MAX / 2 + 1, u128::MAX), Some(cent));
}
