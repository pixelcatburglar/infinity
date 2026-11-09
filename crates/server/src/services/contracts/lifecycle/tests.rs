use super::{expiry_time, FILETIME_MINUTE};

mod requested_acceptance;
mod corporate;
mod corporate_auction;
mod corporate_fixture;
mod corporate_edges;
mod requested_active;
mod transitions;
mod fees;
mod volume;
mod wrap;
mod gm_expiry;
mod mixed_ownership;


#[test]
fn expiry_time_preserves_minutes_and_saturates_large_durations() {
    assert_eq!(expiry_time(42, 2), 42 + 2 * FILETIME_MINUTE);
    assert_eq!(expiry_time(42, i128::from(i64::MAX)), i64::MAX);
}
