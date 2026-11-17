//! Corporation governance behavior tests using isolated database transactions.
mod fixtures;
mod shares;
mod share_boundaries;
mod dividends;
mod votes;
mod races;
mod protocol;
mod admission;
mod governance_permissions;
mod governance_members;
mod governance_profile;
mod governance_ceo;
mod vote_actions;
mod vote_elections;
mod item_locks;
mod governance_kicks;
mod governance_exit;
#[test]
fn player_corporation_id_range_is_inclusive() {
    for id in [98_000_000, 98_500_000, 98_999_999] {
        assert!(super::is_player_corp(id), "{id}");
    }
    for id in [i64::MIN, -1, 0, 1_000_001, 90_000_001, 97_999_999, 99_000_000, 99_999_999,
        100_000_000, 1_500_000_123, i64::MAX] {
        assert!(!super::is_player_corp(id), "{id}");
    }
}
