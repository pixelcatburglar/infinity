//! Roguelike exploration response for `roguelikeMgr`.
//! Mirrors the client's `eve.roguelike.exploration.Responsed` message.
//! Uses the login user and system clock, not database state.
//! Weekly ships and runs are not offered.

use super::super::{Call, CallResult};
use crate::client::Client;
use infinity_core::consts::SECONDS_PER_HOUR;
use marshal::Value;
use std::time::{SystemTime, UNIX_EPOCH};

/// The roguelike lab's weekly reset (Monday), at the daily downtime turnover.
const WEEKLY_RESET_HOUR_UTC: u64 = 3;
/// Unix time convention: seconds per day.
const DAY_SECONDS: u64 = 86_400;
/// Standard calendar unit: days per week.
const DAYS_PER_WEEK: u64 = 7;
/// Calendar offset: the Unix epoch was three days after Monday.
const UNIX_EPOCH_MONDAY_OFFSET_DAYS: u64 = 3;

/// Unix seconds of the next Monday reset.
fn next_weekly_reset(now: u64) -> u64 {
    let today = now / DAY_SECONDS * DAY_SECONDS + WEEKLY_RESET_HOUR_UTC * SECONDS_PER_HOUR as u64;
    (0..=DAYS_PER_WEEK)
        .map(|d| today + d * DAY_SECONDS)
        .find(|t| *t > now && (t / DAY_SECONDS + UNIX_EPOCH_MONDAY_OFFSET_DAYS) % DAYS_PER_WEEK == 0)
        .unwrap_or(today + DAYS_PER_WEEK * DAY_SECONDS)
}

/// CallExploreRequested(): answered with OnExploreResponsed(text-format
/// eve.roguelike.exploration.Responsed). No weekly ships or runs are offered.
fn explore(c: &mut Client) -> CallResult {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let text = format!(
        "user {{ sequential: {} }}\nweekly_total_times: 0\nweekly_used_times: 0\nnext_refresh_time {{ seconds: {} }}\nmax_level: 0\n",
        c.login.user_id,
        next_weekly_reset(now)
    );
    c.notify("OnExploreResponsed", vec![Value::str(text)]);
    Ok(Value::None)
}

pub(super) fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    match call.method {
        "CallExploreRequested" => Some(explore(c)),
        _ => None,
    }
}

#[cfg(test)] mod tests;
