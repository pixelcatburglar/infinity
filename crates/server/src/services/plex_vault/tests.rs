//! Exact vault arithmetic at storage boundaries.
use super::*;

#[test]
fn exact_domain_and_checked_changes() {
    assert_eq!(next_balance(MAX_BALANCE, VaultChange::Add(-1)), Some(MAX_BALANCE - 1));
    assert_eq!(next_balance(MAX_BALANCE, VaultChange::Add(1)), None);
    assert_eq!(next_balance(0, VaultChange::Add(-1)), None);
    assert_eq!(next_balance(0, VaultChange::Set(MAX_BALANCE)), Some(MAX_BALANCE));
    assert_eq!(next_balance(0, VaultChange::Set(-1)), None);
    assert_eq!(next_balance(MAX_BALANCE, VaultChange::Add(i128::MAX)), None);
}

#[test]
fn market_kinds_carry_only_their_real_identities() {
    let fill = VaultKind::MarketFill { order_id: 7, trade_id: 9 };
    assert_eq!(fill.as_str(), "market_fill");
    assert_eq!(fill.market_ids(), (Some(7), Some(9)));
    let refund = VaultKind::MarketRefund { order_id: 7 };
    assert_eq!(refund.as_str(), "market_refund");
    assert_eq!(refund.market_ids(), (Some(7), None));
    assert_eq!(VaultKind::GmAdd.market_ids(), (None, None));
}
