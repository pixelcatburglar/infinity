use super::rules::{self, ALPHA, OMEGA};
use crate::dao::accounts::subscriptions::CloneGrade;
use infinity_core::consts::{FAR_FUTURE, FILETIME_SECOND};

pub(super) const NOW: i64 = infinity_core::consts::FILETIME_UNIX_EPOCH + 100 * FILETIME_SECOND;

pub(super) fn row(grade: i16, omega_until: Option<i64>, grace_until: Option<i64>) -> CloneGrade {
    CloneGrade { user_id: 7, grade, omega_until, grace_until, revision: 3 }
}

#[test]
fn effective_grade_covers_unlimited_alpha_finite_and_grace_boundaries() {
    assert_eq!(rules::effective_grade(&row(ALPHA, None, None), NOW), ALPHA);
    assert_eq!(rules::effective_grade(&row(OMEGA, None, None), NOW), OMEGA);
    let expiry = NOW + 20 * FILETIME_SECOND;
    let grace = expiry + 30 * FILETIME_SECOND;
    let finite = row(OMEGA, Some(expiry), Some(grace));
    assert_eq!(rules::effective_grade(&finite, expiry - 1), OMEGA);
    assert_eq!(rules::effective_grade(&finite, expiry), OMEGA);
    assert_eq!(rules::effective_grade(&finite, expiry + 1), OMEGA);
    assert_eq!(rules::effective_grade(&finite, grace - 1), OMEGA);
    assert_eq!(rules::effective_grade(&finite, grace), ALPHA);
    assert_eq!(rules::effective_grade(&finite, grace + 1), ALPHA);
}

#[test]
fn subscription_time_is_unix_nanoseconds_and_alpha_is_zero() {
    assert_eq!(rules::subscription_time(&row(ALPHA, None, None), NOW).unwrap(), 0);
    assert_eq!(rules::subscription_time(&row(OMEGA, None, None), NOW).unwrap(), crate::client::unix_ns_from_filetime(FAR_FUTURE).unwrap());
    let expiry = NOW + FILETIME_SECOND;
    let finite = row(OMEGA, Some(expiry), Some(expiry + FILETIME_SECOND));
    assert_eq!(rules::subscription_time(&finite, expiry).unwrap(), crate::client::unix_ns_from_filetime(expiry).unwrap());
    assert_eq!(rules::subscription_time(&finite, expiry + FILETIME_SECOND).unwrap(), 0);
}

#[test]
fn grant_deadlines_accepts_only_documented_requests_and_checked_ranges() {
    assert_eq!(rules::grant_deadlines(ALPHA, None, 0, NOW).unwrap(), (None, None));
    assert_eq!(rules::grant_deadlines(OMEGA, None, 0, NOW).unwrap(), (None, None));
    let (omega, grace) = rules::grant_deadlines(OMEGA, Some(10), 20, NOW).unwrap();
    assert_eq!((omega, grace), (Some(NOW + 10 * FILETIME_SECOND), Some(NOW + 30 * FILETIME_SECOND)));
    for request in [
        (2, None, 0), (ALPHA, Some(1), 0), (OMEGA, Some(0), 0),
        (OMEGA, Some(-1), 0), (OMEGA, Some(1), -1),
        (OMEGA, Some(i64::MAX), 0), (OMEGA, Some(1), i64::MAX),
    ] {
        assert!(rules::grant_deadlines(request.0, request.1, request.2, NOW).is_err(), "request {request:?}");
    }
}

#[test]
fn delayed_grace_is_normalized_by_effective_grade_without_default_grace() {
    let expiry = NOW - FILETIME_SECOND;
    let grace = NOW + FILETIME_SECOND;
    assert_eq!(rules::effective_grade(&row(OMEGA, Some(expiry), Some(grace)), NOW), OMEGA);
    assert_eq!(rules::effective_grade(&row(OMEGA, Some(expiry), Some(NOW)), NOW), ALPHA);
    assert_eq!(rules::grant_deadlines(OMEGA, Some(1), 0, NOW).unwrap().1, Some(NOW + FILETIME_SECOND));
    assert!(rules::grant_deadlines(OMEGA, Some(1), 1, i64::MAX / FILETIME_SECOND).is_err());
}

#[test]
fn malformed_rows_are_safe_for_alpha_and_subscription_time() {
    assert_eq!(rules::effective_grade(&row(99, Some(NOW), Some(NOW)), NOW), ALPHA);
    assert_eq!(rules::subscription_time(&row(99, Some(NOW), Some(NOW)), NOW).unwrap(), 0);
}
