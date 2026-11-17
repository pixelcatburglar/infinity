use super::*;

#[test]
fn npc_bounty_interval_defaults_and_filetime_bounds() {
    let fixture = Fixture::new();
    let defaults = Config::load_with(&fixture.path(), |_| None).unwrap();
    assert_eq!(defaults.combat.bounty_payout_interval_secs, 1200);
    fixture.write("[combat]\nbounty_payout_interval_secs = 300");
    assert_eq!(Config::load_with(&fixture.path(), |_| None).unwrap().combat.bounty_payout_interval_secs, 300);
    for value in ["0", "-60"] {
        assert!(load_error(&format!("[combat]\nbounty_payout_interval_secs = {value}")).contains("combat.bounty_payout_interval_secs"));
    }
    let over = i64::MAX / infinity_core::consts::FILETIME_SECOND + 1;
    assert!(load_error(&format!("[combat]\nbounty_payout_interval_secs = {over}")).contains("combat.bounty_payout_interval_secs"));
    // A positive interval whose deadline cannot be represented is rejected too.
    let limit = i64::MAX / infinity_core::consts::FILETIME_SECOND;
    assert!(load_error(&format!("[combat]\nbounty_payout_interval_secs = {limit}")).contains("combat.bounty_payout_interval_secs"));
}
