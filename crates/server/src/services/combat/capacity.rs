//! Server policy for refusing combat when the bounded facts ledger is full.
//! The client receives CustomNotify; warnings use the existing checkpoint interval.
//! This does not schedule a checkpoint or alter the actor's capacity.
use std::sync::Mutex;
use std::time::{Duration, Instant};
use crate::services::{user_error, CallError};
use marshal::Value;

static LAST_WARNING: Mutex<Option<Instant>> = Mutex::new(None);

fn warning_due(last: &mut Option<Instant>, now: Instant, interval: Duration) -> bool {
    if last.is_some_and(|previous| now.saturating_duration_since(previous) < interval) { return false; }
    *last = Some(now);
    true
}

/// A full ledger refuses the effective cycle; the checkpoint policy is unchanged.
pub(crate) fn refusal(system: i64) -> CallError {
    let interval = Duration::from_secs(crate::config::game().space.position_snapshot_secs as u64);
    if warning_due(&mut LAST_WARNING.lock().unwrap(), Instant::now(), interval) {
        tracing::warn!(system, "combat facts capacity full; effective cycles are refused until checkpoint recovery");
    }
    user_error(infinity_core::messages::MSG_CUSTOM_NOTIFY, vec![("notify".into(), Value::unicode(infinity_core::messages::effects::COMBAT_CAPACITY_FULL))])
}

/// Identify the shared refusal without treating other CustomNotify errors as capacity.
pub(crate) fn is_refusal(error: &CallError) -> bool {
    matches!(error, CallError::User { msg, dict } if msg == infinity_core::messages::MSG_CUSTOM_NOTIFY
        && dict.len() == 1 && dict[0].0 == "notify"
        && dict[0].1 == Value::unicode(infinity_core::messages::effects::COMBAT_CAPACITY_FULL))
}

#[cfg(test)] mod tests;
