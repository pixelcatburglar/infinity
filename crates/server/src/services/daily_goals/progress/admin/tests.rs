use super::*;

#[test]
fn audit_snapshot_preserves_full_counter_and_reward_state() {
    let state = GoalState { progress: u64::MAX, remainder: crate::lua::events::combat::BinaryAmount { coefficient: "1".into(), exponent: -1 }, completed_at: Some(17), earned_omega: true, paid: false, has_unclaimed: true, revision: 2 };
    let rewards = [state::RewardState { earned: true, revision: 3 }];
    let snapshot = state_json(&state, &rewards);
    assert_eq!(snapshot["progress"], u64::MAX.to_string());
    assert_eq!(snapshot["remainder_coefficient"], "1");
    assert_eq!(snapshot["rewards"][0]["revision"], 3);
}
