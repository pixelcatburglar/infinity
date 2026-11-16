use super::*;

#[test]
fn loot_rejects_unpublished_null_and_other_categories() {
    assert_eq!(loot_category(Some(true), Some(CATEGORY_MODULE)), Some(CATEGORY_MODULE));
    assert_eq!(loot_category(Some(true), Some(CATEGORY_CHARGE)), Some(CATEGORY_CHARGE));
    assert_eq!(loot_category(Some(false), Some(CATEGORY_MODULE)), None);
    assert_eq!(loot_category(None, Some(CATEGORY_CHARGE)), None);
    assert_eq!(loot_category(Some(true), None), None);
    assert_eq!(loot_category(Some(true), Some(0)), None);
}

#[test]
fn bounty_payout_scales_and_rejects_nonfinite_results() {
    assert_eq!(bounty_payout(123.45, 1.0), Some(123.45));
    assert_eq!(bounty_payout(123.45, 0.0), Some(0.0));
    assert_eq!(bounty_payout(100.0, 1.5), Some(150.0));
    assert_eq!(bounty_payout(f64::MAX, 2.0), None);
    assert_eq!(bounty_payout(f64::NAN, 1.0), None);
}

#[test]
fn loot_ceiling_preserves_nan_and_floors_at_minimum_ceiling() {
    assert!(loot_ceiling(f64::NAN).is_nan());
    assert_eq!(loot_ceiling(0.0), NPC_LOOT_MINIMUM_CEILING);
    assert_eq!(loot_ceiling(49_999.9), NPC_LOOT_MINIMUM_CEILING);
    assert_eq!(loot_ceiling(NPC_LOOT_MINIMUM_CEILING), NPC_LOOT_MINIMUM_CEILING);
    assert_eq!(loot_ceiling(60_000.0), 60_000.0);
}
