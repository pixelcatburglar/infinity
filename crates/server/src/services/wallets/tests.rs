use super::*;

#[test]
fn wider_snapshots_validate_and_clamp_symmetrically() {
    let cap = personal_client_cap();
    let max = "99999999999999999999999999999999999.99";
    let min = "-99999999999999999999999999999999999.99";
    for value in [max, "+00010000000000000000000000000000000000.0000"] {
        assert_eq!(journal_balance_client_isk(value, cap).unwrap(), cap.wire_isk());
    }
    assert_eq!(journal_balance_client_isk(min, cap).unwrap(), -cap.wire_isk());
    for (value, expected) in [("-123.45", -123.45), ("+0001.2300", 1.23), ("-0.000", 0.0), ("1", 1.0), ("1.2", 1.2)] {
        assert_eq!(journal_balance_client_isk(value, cap).unwrap(), expected);
        assert_eq!(IskAmount::parse_decimal(value).unwrap().client_isk(cap), expected);
    }
    for value in ["", ".1", "NaN", "Infinity", "-inf", "1.001", "1.00.0", "1e2", "--1", "100000000000000000000000000000000000.00", "-100000000000000000000000000000000000.00", "1."] {
        assert!(journal_balance_client_isk(value, cap).is_err(), "{value}");
        assert!(IskAmount::parse_decimal(value).is_err(), "{value}");
    }
}

#[test]
fn negative_and_zero_are_distinct() {
    assert!(validate(IskAmount::from_cents(-1).unwrap()).is_err());
    assert!(validate(IskAmount::ZERO).is_ok());
}
