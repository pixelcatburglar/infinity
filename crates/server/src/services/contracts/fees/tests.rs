use super::*;
use super::super::{TYPE_AUCTION, TYPE_COURIER, TYPE_ITEM_EXCHANGE};

fn assert_fees(fees: Fees, sales_tax: f64, rate: f64, broker: f64, deposit: f64) {
    assert!((fees.sales_tax - sales_tax).abs() < 0.000_001, "sales tax: {}", fees.sales_tax);
    assert!((fees.sales_tax_rate - rate).abs() < 0.000_000_001, "sales tax rate: {}", fees.sales_tax_rate);
    assert!((fees.brokers_fee - broker).abs() < 0.000_001, "broker fee: {}", fees.brokers_fee);
    assert!((fees.deposit - deposit).abs() < 0.000_001, "deposit: {}", fees.deposit);
}

#[test]
fn minimum_exchange_fees() {
    assert_fees(calculate(TYPE_ITEM_EXCHANGE, false, 1.0, 0.0, 1440, 0, 0), 10_000.0, 0.01, 10_000.0, 100_000.0);
}

#[test]
fn auction_three_days_with_maximum_skills() {
    assert_fees(calculate(TYPE_AUCTION, false, 10_000_000.0, 0.0, 4320, 5, 5), 50_000.0, 0.005, 33_000.0, 100_000.0);
}

#[test]
fn courier_uses_reward_and_reduced_minimum() {
    assert_fees(calculate(TYPE_COURIER, false, 0.0, 1_000_000.0, 4320, 5, 5), 10_000.0, 0.0005, 11_000.0, 10_000.0);
}

#[test]
fn private_contract_ignores_public_fees() {
    assert_fees(calculate(TYPE_COURIER, true, 1_000_000_000.0, 0.0, 4320, 5, 5), 0.0, 0.0, 10_000.0, 0.0);
}

#[test]
fn maximum_skill_caps_apply_before_duration_multiplier() {
    assert_fees(calculate(TYPE_ITEM_EXCHANGE, false, 1_000_000_000_000.0, 0.0, 1440, 5, 5), 500_000_000.0, 0.005, 75_000_000.0, 100_000_000.0);
}

#[test]
fn broker_truncates_before_applying_additional_days() {
    assert_fees(calculate(TYPE_AUCTION, false, 2_500_325.5, 0.0, 4320, 0, 0), 25_003.0, 0.01, 11_001.1, 100_000.0);
}

#[test]
fn duration_uses_truncated_whole_days_including_subday() {
    let subday = calculate(TYPE_AUCTION, false, 10_000_000.0, 0.0, 720, 0, 0);
    let fractional = calculate(TYPE_AUCTION, false, 10_000_000.0, 0.0, 3600, 0, 0);
    assert_eq!(subday.brokers_fee, 38_000.0);
    assert_eq!(fractional.brokers_fee, 42_000.0);
}

#[test]
fn larger_of_price_and_reward_sets_public_base() {
    let price = calculate(TYPE_ITEM_EXCHANGE, false, 20_000_000.0, 3_000_000.0, 1440, 0, 0);
    let reward = calculate(TYPE_ITEM_EXCHANGE, false, 3_000_000.0, 20_000_000.0, 1440, 0, 0);
    assert_fees(price, 200_000.0, 0.01, 80_000.0, 200_000.0);
    assert_fees(reward, 200_000.0, 0.01, 80_000.0, 200_000.0);
}

#[test]
fn unskilled_caps_and_intermediate_skills() {
    assert_fees(calculate(TYPE_ITEM_EXCHANGE, false, 1_000_000_000_000.0, 0.0, 1440, 0, 0), 1_000_000_000.0, 0.01, 100_000_000.0, 100_000_000.0);
    assert_fees(calculate(TYPE_ITEM_EXCHANGE, false, 20_000_000.0, 0.0, 1440, 2, 3), 160_000.0, 0.008, 68_000.0, 200_000.0);
}

#[test]
fn settlement_uses_creation_time_tax_terms() {
    let public = calculate(TYPE_AUCTION, false, 1.0, 0.0, 1440, 5, 0);
    assert_eq!(public.sales_tax_maximum, 500_000_000.0);
    assert_eq!(sales_tax_amount(1.0, public.sales_tax_rate, public.sales_tax_maximum), 10_000.0);
    assert_eq!(sales_tax_amount(2_500_325.5, public.sales_tax_rate, public.sales_tax_maximum), 12_501.0);
    assert_eq!(sales_tax_amount(1_000_000_000_000.0, public.sales_tax_rate, public.sales_tax_maximum), 500_000_000.0);
    let private = calculate(TYPE_AUCTION, true, 1.0, 0.0, 1440, 5, 0);
    assert_eq!(private.sales_tax_maximum, 0.0);
    assert_eq!(sales_tax_amount(1_000_000.0, private.sales_tax_rate, private.sales_tax_maximum), 0.0);
    assert_eq!(sales_tax_amount(1_000_000.0, 0.0, 500_000_000.0), 0.0);
}
