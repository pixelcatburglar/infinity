//! Exact native amount and monotone counter boundaries.
use super::*;

#[test]
fn fractional_hits_compose_and_cap_without_losing_carry() {
    let half = Increment::Hp(BinaryAmount { coefficient: "1".into(), exponent: -1 });
    let (progress, remainder) = capped(0, 2, &zero(), &half).unwrap();
    assert_eq!(progress, 0);
    assert_eq!(remainder, BinaryAmount { coefficient: "1".into(), exponent: -1 });
    let (progress, remainder) = capped(progress, 2, &remainder, &half).unwrap();
    assert_eq!(progress, 1);
    assert_eq!(remainder, zero());
    let large = Increment::Hp(BinaryAmount { coefficient: "5".into(), exponent: 0 });
    assert_eq!(capped(progress, 2, &remainder, &large).unwrap(), (2, zero()));
}

#[test]
fn unit_increments_are_capped_and_do_not_overflow() {
    let half = BinaryAmount { coefficient: "1".into(), exponent: -1 };
    assert_eq!(capped(3, 10, &half, &Increment::Units(2)).unwrap(), (5, half.clone()));
    assert_eq!(capped(5, 10, &half, &Increment::Units(u64::MAX)).unwrap(), (10, zero()));
    assert!(capped(11, 10, &zero(), &Increment::Units(1)).is_err());
}
