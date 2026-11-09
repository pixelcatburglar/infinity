use super::*;
use marshal::Value;

fn amount(value: &str) -> IskAmount { IskAmount::parse_decimal(value).unwrap() }
fn rate(value: f64) -> FeeRate { FeeRate::from_input(value).unwrap() }

#[test]
fn tick_boundaries_and_significant_figures() {
    for price in [0.01, 0.99, 1.01, 10.01, 99.99, 100.1, 9999.0, 10000.0, 1e9, 1e12, 9_223_000_000_000.0] {
        assert!(valid_tick(price), "{price}");
    }
    for price in [0.0, 0.009, 1.001, 9.999, 100.01, 9999.9, super::super::MAX_ORDER_PRICE + 1.0, f64::NAN, f64::INFINITY, -1.0] {
        assert!(!valid_tick(price), "{price}");
    }
}

#[test]
fn immediate_cents_and_resting_ticks_differ() {
    for price in [1971.99, 225005.00, super::super::MAX_ORDER_PRICE] {
        assert!(valid_cents(price), "{price}");
        assert!(!valid_tick(price), "{price}");
    }
    for price in [1972.00, 225000.00, 0.01] {
        assert!(valid_cents(price), "{price}");
        assert!(valid_tick(price), "{price}");
    }
    for price in [1971.991, 0.0, 0.009, -1.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY, super::super::MAX_ORDER_PRICE + 1.0] {
        assert!(!valid_cents(price), "{price}");
        assert!(!valid_tick(price), "{price}");
    }
}

#[test]
fn effective_trade_fields_use_independent_skill_levels() {
    crate::tests::init_config();
    for (level, expected) in [(0, -1), (1, 0), (2, 5), (3, 10), (4, 20), (5, 32767)] { assert_eq!(skill_limit(level), expected); }
    let skills = skills_from_levels(|id| match id {
        SKILL_MARKETING => 2, SKILL_PROCUREMENT => 3,
        SKILL_VISIBILITY => 4, SKILL_DAYTRADING => 5, _ => 0,
    }).unwrap();
    assert_eq!((skills.marketing, skills.procurement, skills.visibility, skills.daytrading), (5, 10, 20, 32767));
    assert_eq!(skills.order_cap, 5);
    assert_eq!(skills.modification_discount, rate(0.5));
}

#[test]
fn broker_quote_metadata_and_formula_edges() {
    crate::tests::init_config();
    let r = adjusted_broker_rate(rate(0.015), 3.0, 5.0).unwrap();
    assert_eq!(broker_fee(r, amount("1000000.00")).unwrap(), amount("13100.00"));
    assert_eq!(broker_fee(adjusted_broker_rate(rate(0.001), 3.0, 5.0).unwrap(), amount("1000000.00")).unwrap(), amount("100.00"));
    assert!(check_expected_fee(None, r).is_ok());
    assert!(check_expected_fee(Some(r.as_f64() + 0.0005), r).is_ok());
    let CallError::User { msg, dict } = check_expected_fee(Some(0.02), r).unwrap_err() else { panic!("expected quote refusal") };
    assert_eq!(msg, messages::MSG_MKT_BROKERS_FEE_UNEXPECTED);
    assert_eq!(dict, vec![("actualBrokerFeePerc".into(), Value::Float(100.0 * r.as_f64())), ("expectedBrokerFeePercentage".into(), Value::Float(0.02))]);
    assert!(check_expected_fee(Some(f64::NAN), r).is_err());
}

#[test]
fn source_quantum_and_exact_rate_composition() {
    assert_eq!(rate(0.0000000000004), rate(0.0));
    assert_eq!(rate(0.0000000000005), rate(0.000000000001));
    assert_eq!(rate(-0.0000000000005), rate(-0.000000000001));
    assert!(FeeRate::from_input(f64::NAN).is_none());
    assert!(FeeRate::from_input(f64::INFINITY).is_none());
    assert_eq!(rate(0.08).mul(rate(0.45)).unwrap(), rate(0.036));
    assert_eq!(apply_rate(amount("9007199254740993.01"), rate(1.0)).unwrap(), amount("9007199254740993.01"));
    assert_eq!(apply_rate(amount("1.00"), rate(0.125)).unwrap(), amount("0.13"));
    assert_eq!(apply_rate(amount("10.00"), rate(0.125)).unwrap(), amount("1.25"));
    let composed = rate(0.08).mul(FeeRate::ratio(45, 100).unwrap()).unwrap();
    assert_eq!(apply_rate(amount("10000000000000000.00"), composed).unwrap(), amount("360000000000000.00"));
    assert!(apply_rate(amount("1.00"), rate(-0.01)).is_err());
    assert!(apply_rate(amount("-1.00"), rate(0.01)).is_err());
}

#[test]
fn broker_minimum_and_modification_combined_rounding() {
    crate::tests::init_config();
    assert_eq!(broker_fee(rate(0.01), amount("9999.99")).unwrap(), amount("100.00"));
    assert_eq!(broker_fee(rate(0.01), amount("10001.00")).unwrap(), amount("100.01"));
    assert_eq!(broker_fee(rate(-0.01), amount("100000.00")).unwrap(), amount("100.00"));
    assert_eq!(modification_fee(rate(0.1), amount("2000.00"), amount("3000.00"), rate(0.5)).unwrap(), amount("250.00"));
    assert_eq!(modification_fee(rate(0.1), amount("3000.00"), amount("2000.00"), rate(0.5)).unwrap(), amount("100.00"));
    assert_eq!(modification_fee(rate(0.5), amount("1000.01"), amount("1000.02"), rate(0.5)).unwrap(), amount("250.01"));
    assert!(modification_fee(rate(0.1), amount("1.00"), amount("2.00"), rate(1.01)).is_err());
}

#[test]
fn skill_and_standing_endpoints() {
    crate::tests::init_config();
    let zero = skills_from_levels(|_| 0).unwrap();
    let five = skills_from_levels(|_| 5).unwrap();
    assert_eq!(zero.modification_discount, rate(0.5));
    assert_eq!(five.modification_discount, rate(0.8));
    assert!(five.broker_fee.as_f64() < zero.broker_fee.as_f64());
    assert!(five.sales_tax.as_f64() < zero.sales_tax.as_f64());
    assert!(adjusted_broker_rate(rate(0.02), 10.0, 10.0).unwrap().as_f64() < adjusted_broker_rate(rate(0.02), -10.0, -10.0).unwrap().as_f64());
    assert!(adjusted_broker_rate(rate(0.02), f64::NAN, 0.0).is_err());
}

#[test]
fn order_domains_reject_unoffered_values() {
    crate::tests::init_config();
    for range in [-1, 0, 1, 2, 3, 4, 5, 10, 20, 30, 40, 32767] { assert!(valid_order_range(range)); }
    for range in [-2, 6, 41, 32768] { assert!(!valid_order_range(range)); }
    for duration in [0, 1, 3, 7, 14, 30, 90] { assert!(valid_order_duration(duration, 0)); }
    assert!(!valid_order_duration(2, 0));
    assert!(!valid_order_duration(1, i64::MAX));
}

#[test]
fn market_fee_reference_matrix() {
    crate::tests::init_config();
    let tax_rates = [0.08, 0.0712, 0.0624, 0.0536, 0.0448, 0.036];
    let broker_rates = [0.03, 0.027, 0.024, 0.021, 0.018, 0.015];
    for level in 0..=5 {
        let skills = skills_from_levels(|id| {
            if id == SKILL_ACCOUNTING || id == SKILL_BROKER_RELATIONS { level } else { 0 }
        }).unwrap();
        for (id, actual, expected) in [
            (format!("accounting-{level}"), skills.sales_tax, tax_rates[level as usize]),
            (format!("broker-{level}"), skills.broker_fee, broker_rates[level as usize]),
        ] {
            println!("market fee reference: {{\"id\":\"{id}\",\"rate\":{}}}", actual.as_f64());
            assert_eq!(actual, rate(expected), "{id}");
        }
    }

    for (id, principal, level, faction, corp, expected) in [
        ("minimum", "1.00", 0, 0.0, 0.0, "100.00"),
        ("minimum-edge", "3333.33", 0, 0.0, 0.0, "100.00"),
        ("above-minimum", "3333.67", 0, 0.0, 0.0, "100.01"),
        ("neutral", "1000000.00", 0, 0.0, 0.0, "30000.00"),
        ("standings", "1000000.00", 5, 3.0, 5.0, "13100.00"),
        ("negative-standings", "1000000.00", 0, -3.0, -5.0, "31900.00"),
        ("large-standing", "10000000000000.00", 5, 3.0, 5.0, "131000000000.00"),
        ("extreme-notional", "19806241676281000000000.00", 5, 3.0, 5.0, "259461765959281100000.00"),
    ] {
        let skills = skills_from_levels(|id| if id == SKILL_BROKER_RELATIONS { level } else { 0 }).unwrap();
        let adjusted = adjusted_broker_rate(skills.broker_fee, faction, corp).unwrap();
        let actual = broker_fee(adjusted, amount(principal)).unwrap();
        println!("market fee reference: {{\"id\":\"{id}\",\"amount_decimal\":\"{}\"}}", actual.decimal());
        assert_eq!(actual, amount(expected), "{id}");
    }

    for (id, old, new, expected) in [
        ("mod-up", "1000.01", "1000.02", "250.01"),
        ("mod-down", "2000.02", "1000.01", "250.00"),
        ("mod-tie", "2000.00", "2000.02", "500.02"),
        ("mod-below", "2000.00", "2000.01", "500.01"),
        ("mod-above", "2000.00", "2000.03", "500.02"),
        ("mod-large", "9000000000000.00", "9223000000000.00", "2417250000000.00"),
    ] {
        let actual = modification_fee(rate(0.5), amount(old), amount(new), rate(0.5)).unwrap();
        println!("market fee reference: {{\"id\":\"{id}\",\"amount_decimal\":\"{}\"}}", actual.decimal());
        assert_eq!(actual, amount(expected), "{id}");
    }

    for (index, (price, expected)) in [
        (0.0, false), (-1.0, false), (0.009, false), (0.01, true),
        (99.99, true), (100.0, true), (9999.0, true), (10000.0, true),
        (10001.0, false), (9223372036854.0, false), (9223000000000.0, true),
        (10000000000000.0, false), (922300000000000.0, false), (922300000000001.0, false),
    ].into_iter().enumerate() {
        let actual = valid_tick(price);
        println!("market fee reference: {{\"id\":\"tick-{index}\",\"order_admitted\":{actual}}}");
        assert_eq!(actual, expected, "tick-{index}");
    }
}
