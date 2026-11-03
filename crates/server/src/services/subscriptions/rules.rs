//! Shared account clone-grade eligibility and checked deadline arithmetic.
//! Eligibility mirrors `omega/client/clone_grade_svc.py` using authority rows in
//! `game.account_clone_grades`. Subscription commerce and full Alpha skill and
//! ship restrictions are outside these rules.
use crate::{client::unix_ns_from_filetime, consts::{FAR_FUTURE, FILETIME_SECOND}, dao::accounts::subscriptions as dao, db::executor::Executor, messages, services::{notify_error, CallError}};

/// `clonegrade/const.py`: client Alpha clone grade.
pub(crate) const ALPHA: i16 = 0;
/// `clonegrade/const.py`: client Omega clone grade.
pub(crate) const OMEGA: i16 = 1;

pub(crate) fn effective_grade(row: &dao::CloneGrade, at: i64) -> i16 {
    if row.grade == OMEGA && row.grace_until.is_none_or(|deadline| at < deadline) { OMEGA } else { ALPHA }
}

pub(crate) fn subscription_time(row: &dao::CloneGrade, at: i64) -> Result<i64, CallError> {
    if effective_grade(row, at) == ALPHA { return Ok(0); }
    unix_ns_from_filetime(row.omega_until.unwrap_or(FAR_FUTURE))
        .ok_or_else(|| CallError::Internal(anyhow::anyhow!("clone-grade subscription time outside Unix-nanosecond range")))
}

pub(crate) async fn current(ex: &(impl Executor + ?Sized), user: i64, at: i64) -> Result<i16, CallError> {
    Ok(effective_grade(&dao::by_user(ex, user).await?, at))
}

pub(crate) fn grant_deadlines(grade: i16, duration: Option<i64>, grace_secs: i64, at: i64) -> Result<(Option<i64>, Option<i64>), CallError> {
    let invalid = || notify_error(messages::subscriptions::INVALID_GRADE_REQUEST);
    match (grade, duration, grace_secs) {
        (ALPHA | OMEGA, None, 0) => Ok((None, None)),
        (OMEGA, Some(seconds), grace) if seconds > 0 && grace >= 0 => {
            let omega = seconds.checked_mul(FILETIME_SECOND).and_then(|delta| at.checked_add(delta)).ok_or_else(invalid)?;
            let until = grace.checked_mul(FILETIME_SECOND).and_then(|delta| omega.checked_add(delta)).ok_or_else(invalid)?;
            unix_ns_from_filetime(omega).ok_or_else(invalid)?;
            unix_ns_from_filetime(until).ok_or_else(invalid)?;
            Ok((Some(omega), Some(until)))
        }
        _ => Err(invalid()),
    }
}
