use super::*;

#[test]
fn client_completion_formula_matrix_and_balance_edges() {
    let expected = [[2, 1, 0, 0], [1, 1, 0, 0], [0, 0, 0, 0], [0, 0, 0, 0]];
    for (bonus, row) in expected.iter().enumerate() {
        for (paid, result) in row.iter().enumerate() {
            assert_eq!(remaining(bonus as u64, paid as u64), *result);
        }
    }
    assert_eq!(remaining(u64::MAX, u64::MAX), 0);
    assert_eq!(remaining(0, u64::MAX), 0);
    assert_eq!(remaining(u64::MAX, 0), 0);
    assert_eq!(price(0), Some(2500));
    assert_eq!(price(1), Some(4500));
    assert_eq!(price(2), None);
    assert_eq!(price(u64::MAX), None);
    for (paid, balance, affordable) in [(0, 2499, false), (0, 2500, true), (1, 4499, false), (1, 4500, true)] {
        assert_eq!(price(paid).is_some_and(|cost| balance >= cost), affordable);
    }
}
