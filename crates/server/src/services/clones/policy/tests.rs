use super::*;

#[test]
fn cooldown_applies_configured_skill_reduction_without_overflow() {
    assert_eq!(jump_cooldown(24, 2, 3), 18 * i128::from(FILETIME_HOUR));
    assert_eq!(jump_cooldown(2, 3, 1), 0);
    assert_eq!(jump_cooldown(i64::MAX, i64::MAX, 2), 0);
    assert_eq!(jump_cooldown(i64::MAX, 1, 0), i128::from(i64::MAX) * i128::from(FILETIME_HOUR));
}
