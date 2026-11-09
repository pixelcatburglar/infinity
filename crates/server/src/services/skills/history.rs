//! Skill history for skillHistoryPanel.py, backed by game.skill_history.
//! Only events recorded by implemented skill mutations are returned.
//! Missing result counts use the client default; explicit nonnegative counts are passed to the database.
use super::{db_err, keyval, CallError};
use infinity_core::messages;
use crate::{client::Client, dao::skills::history, services::notify_error};
use marshal::Value;

/// Client appConst.py:1543-1560, training completed.
pub(super) const TRAINING_COMPLETE: i32 = 37;
/// Client appConst.py:1543-1560, GM skill grant.
pub(super) const GM_GIVE: i32 = 39;
/// Client appConst.py:1543-1560, skill removed.
pub(super) const REMOVED: i32 = 177;
/// Client appConst.py skillbook injection.
pub(super) const INJECTED: i32 = 56;
/// Client appConst.py free skill points applied.
pub(super) const FREE_POINTS_USED: i32 = 307;
/// Client appConst.py skill extraction.
pub(super) const EXTRACTED: i32 = 431;
/// skillHistoryPanel.py default request.
const DEFAULT_RESULTS: i64 = 50;

pub(super) async fn get(c: &Client, char_id: i64, value: Option<&Value>) -> Result<Value, CallError> {
    let limit = match value {
        None => DEFAULT_RESULTS,
        Some(Value::Int(limit)) if *limit >= 0 => *limit,
        _ => return Err(notify_error(messages::skills::INVALID_HISTORY_REQUEST)),
    };
    let rows = history::list(&c.db, char_id, limit).await.map_err(db_err)?;
    Ok(Value::list(rows.into_iter().map(|r| keyval([
        ("skillTypeID", Value::Int(r.skill_type_id as i64)),
        ("eventTypeID", Value::Int(r.event_type_id as i64)),
        ("absolutePoints", Value::Int(r.absolute_points)),
        ("logDate", Value::Int(r.log_date)),
    ]))))
}

#[cfg(test)] mod tests;
