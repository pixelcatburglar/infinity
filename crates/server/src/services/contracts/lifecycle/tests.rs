use super::{expiry_time, sum_volume, FILETIME_MINUTE};

mod requested_acceptance;
mod corporate;
mod corporate_auction;
mod corporate_fixture;
mod corporate_edges;
mod requested_active;
mod transitions;
mod fees;

#[test]
fn volume_sum_preserves_sql_zero_and_nonempty_values() {
    assert_eq!(sum_volume(std::iter::empty()).to_bits(), 0.0f64.to_bits());
    assert_eq!(sum_volume([1.25, 2.5, 4.0].into_iter()).to_bits(), 7.75f64.to_bits());
}

#[test]
fn expiry_time_preserves_minutes_and_saturates_large_durations() {
    assert_eq!(expiry_time(42, 2), 42 + 2 * FILETIME_MINUTE);
    assert_eq!(expiry_time(42, i128::from(i64::MAX)), i64::MAX);
}
