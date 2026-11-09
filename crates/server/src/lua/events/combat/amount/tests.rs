use super::*;

#[test]
fn exact_subnormal_fraction_and_carry() {
    let mut sum = ExactBinarySum::default();
    assert_eq!(sum.to_amount(), BinaryAmount { coefficient: "0".into(), exponent: 0 });
    assert!(sum.try_add(f64::from_bits(1)));
    assert!(sum.try_add(f64::from_bits(1)));
    assert_eq!(sum.to_amount(), BinaryAmount { coefficient: "1".into(), exponent: -1073 });
    let mut sum = ExactBinarySum::default();
    for _ in 0..10 { assert!(sum.try_add(0.5)); }
    assert_eq!(sum.occurrences(), 10);
    assert_eq!(sum.to_amount(), BinaryAmount { coefficient: "5".into(), exponent: 0 });
    let mut sum = ExactBinarySum::default();
    assert!(sum.try_add(f64::from_bits(1)));
    assert!(sum.try_add(f64::from_bits(1_u64 << 52)));
    assert_eq!(sum.occurrences(), 2);
}
#[test]
fn invalid_additions_and_count_overflow_are_atomic() {
    let mut sum = ExactBinarySum::default();
    for value in [0.0, -1.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let before = sum.clone(); assert!(!sum.try_add(value)); assert_eq!(sum, before);
    }
    sum.occurrences = u64::MAX - 1;
    assert!(sum.try_add(1.0));
    let before = sum.clone(); assert!(!sum.try_add(1.0)); assert_eq!(sum, before);
}
#[test]
fn maximum_float_and_large_coefficient() {
    let mut sum = ExactBinarySum::default();
    assert!(sum.try_add(f64::MAX));
    assert_eq!(sum.to_amount(), BinaryAmount { coefficient: "9007199254740991".into(), exponent: 971 });
    assert!(sum.try_add(f64::from_bits(1)));
    let amount = sum.to_amount();
    assert_eq!(amount.exponent, -1074);
    assert!(amount.coefficient.len() > 20);
    assert!(amount.validate().is_ok());
}

#[test]
fn full_integer_conversion_above_u64() {
    let mut sum = ExactBinarySum::default();
    assert!(sum.try_add(18446744073709551616.0));
    assert!(sum.try_add(1.0));
    assert_eq!(sum.to_amount(), BinaryAmount { coefficient: "18446744073709551617".into(), exponent: 0 });
}
#[test]
fn crossing_limb_boundary_is_exact() {
    let mut sum = ExactBinarySum::default();
    sum.limbs[0] = u64::MAX;
    assert!(sum.try_add(f64::from_bits(1)));
    assert_eq!(sum.to_amount(), BinaryAmount { coefficient: "1".into(), exponent: -1010 });
}

#[test]
fn coefficient_bit_and_exponent_bounds() {
    let mut sum = ExactBinarySum::default();
    sum.limbs[(MAX_SUM_BITS - 1) / WORD_BITS] = 1 << ((MAX_SUM_BITS - 1) % WORD_BITS);
    sum.limbs[0] = 1;
    let legal = sum.to_amount();
    assert_eq!(legal.exponent, QUANTUM_EXPONENT);
    assert!(legal.validate().is_ok());
    sum.limbs[MAX_SUM_BITS / WORD_BITS] |= 1 << (MAX_SUM_BITS % WORD_BITS);
    assert!(sum.to_amount().validate().is_err());
    assert!(BinaryAmount { coefficient: "1".into(), exponent: MAX_EXPONENT }.validate().is_ok());
    assert!(BinaryAmount { coefficient: "1".into(), exponent: MAX_EXPONENT + 1 }.validate().is_err());
    assert!(BinaryAmount { coefficient: "1".into(), exponent: QUANTUM_EXPONENT - 1 }.validate().is_err());
    let mut high = ExactBinarySum::default();
    high.limbs[(MAX_SUM_BITS - 1) / WORD_BITS] = 1 << ((MAX_SUM_BITS - 1) % WORD_BITS);
    assert_eq!(high.to_amount(), BinaryAmount { coefficient: "1".into(), exponent: MAX_EXPONENT });
}

fn amount(coefficient: &str, exponent: i16) -> BinaryAmount {
    BinaryAmount { coefficient: coefficient.into(), exponent }
}

#[test]
fn fractional_batches_preserve_binary64_values() {
    let zero = amount("0", 0);
    let tenth = BinaryAmount::from_f64(0.1).unwrap();
    let fifth = BinaryAmount::from_f64(0.2).unwrap();
    let (progress, remainder) = accumulate_capped(&tenth, &fifth, 2).unwrap();
    assert_eq!(progress, 0);
    assert_eq!(remainder, amount("10808639105689191", -55));
    let mut remainder = zero;
    let mut progress = 0;
    for _ in 0..10 {
        let (accepted, next) = accumulate_capped(&remainder, &tenth, 2 - progress).unwrap();
        progress += accepted;
        remainder = next;
    }
    assert_eq!(progress, 1);
    assert_eq!(remainder, amount("1", -54));
}

#[test]
fn subnormal_and_near_integer_crossings_are_exact() {
    let tiny = amount("1", QUANTUM_EXPONENT);
    let (n, r) = accumulate_capped(&tiny, &tiny, 2).unwrap();
    assert_eq!((n, r), (0, amount("1", -1073)));
    let below_one = BinaryAmount::from_f64(f64::from_bits(1.0_f64.to_bits() - 1)).unwrap();
    let (n, r) = accumulate_capped(&below_one, &amount("1", -53), 2).unwrap();
    assert_eq!((n, r), (1, amount("0", 0)));
    let (n, r) = accumulate_capped(&below_one, &tiny, 2).unwrap();
    assert_eq!(n, 0);
    let mut expected = ExactBinarySum::default();
    assert!(expected.try_add(f64::from_bits(1.0_f64.to_bits() - 1)));
    assert!(expected.try_add(f64::from_bits(1)));
    assert_eq!(r, expected.to_amount());
}

#[test]
fn capped_progress_handles_full_u64_and_huge_values() {
    let zero = amount("0", 0);
    let max = amount("18446744073709551615", 0);
    assert_eq!(accumulate_capped(&zero, &max, u64::MAX).unwrap(), (u64::MAX, zero.clone()));
    let almost = amount("18446744073709551613", 0);
    assert_eq!(accumulate_capped(&amount("1", -1), &almost, u64::MAX).unwrap(), (u64::MAX - 2, amount("1", -1)));
    assert_eq!(accumulate_capped(&amount("1", -1), &amount("1", MAX_EXPONENT), u64::MAX).unwrap(), (u64::MAX, zero.clone()));
    assert_eq!(accumulate_capped(&zero, &amount("1", 64), u64::MAX).unwrap(), (u64::MAX, zero.clone()));
    assert_eq!(accumulate_capped(&zero, &zero, 0).unwrap(), (0, zero));
}

#[test]
fn rejects_invalid_operands_even_at_zero_cap() {
    let zero = amount("0", 0);
    let invalid = [
        amount("", 0), amount("00", 0), amount("01", 0), amount("2", 0),
        amount("0", 1), amount("-1", 0), amount("1", QUANTUM_EXPONENT - 1),
        amount("1", MAX_EXPONENT + 1), amount("3", MAX_EXPONENT),
        amount(&"9".repeat(MAX_DECIMAL_DIGITS + 1), 0),
        amount(&"9".repeat(MAX_DECIMAL_DIGITS), 0),
    ];
    for bad in invalid {
        assert!(bad.validate().is_err());
        assert!(accumulate_capped(&zero, &bad, 0).is_err());
        assert!(accumulate_capped(&bad, &zero, 0).is_err());
    }
    for bad in [amount("1", 0), amount("3", -1), amount("1", MAX_EXPONENT)] {
        assert!(bad.validate().is_ok());
        assert!(accumulate_capped(&bad, &zero, 0).is_err());
    }
    assert!(accumulate_capped(&amount("1", -1), &zero, 1).is_ok());
}

#[test]
fn remainder_validator_shares_accumulator_fraction_rules() {
    for valid in [amount("0", 0), amount("1", QUANTUM_EXPONENT), amount("3", -2)] {
        assert!(validate_remainder(&valid).is_ok());
    }
    for invalid in [amount("1", 0), amount("3", -1), amount("0", -1), amount("2", -2), amount("01", -2), amount(&"9".repeat(MAX_DECIMAL_DIGITS), QUANTUM_EXPONENT)] {
        assert!(validate_remainder(&invalid).is_err());
    }
}
