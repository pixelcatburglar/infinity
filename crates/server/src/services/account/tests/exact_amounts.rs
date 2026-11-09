use super::super::*;

#[test]
fn journal_projects_exact_above_cap_and_wider_snapshot_symmetrically() {
    let cap = personal_client_cap();
    let one_cent = IskAmount::from_cents(1).unwrap();
    assert_eq!(cap, IskAmount::from_cents(100_000_000_000_000_000_000).unwrap());
    let above = IskAmount::from_cents(100_000_000_000_000_000_001).unwrap();
    let row = |amount, balance: &str| Row { transaction_id: 1, date: 1, entry_type_id: 1, owner1: 1, owner2: 1, reference_id: 1, amount, balance: balance.into(), description: String::new() };
    let positive = journal_row(row(above, "99999999999999999999999999999999999.99"), cap).unwrap();
    assert_eq!(super::super::super::value_field(&positive, "amount"), Some(&Value::Float(cap.wire_isk())));
    assert_eq!(super::super::super::value_field(&positive, "balance"), Some(&Value::Float(cap.wire_isk())));
    let negative = journal_row(row(above.checked_neg().unwrap(), "-99999999999999999999999999999999999.99"), cap).unwrap();
    assert_eq!(super::super::super::value_field(&negative, "amount"), Some(&Value::Float(-cap.wire_isk())));
    assert_eq!(super::super::super::value_field(&negative, "balance"), Some(&Value::Float(-cap.wire_isk())));
    for corrupt in ["NaN", "1.001", "100000000000000000000000000000000000.00"] {
        assert!(matches!(journal_row(row(above, corrupt), cap), Err(CallError::Internal(_))), "{corrupt}");
    }
    assert_eq!(above.checked_sub(cap), Some(one_cent));
}
