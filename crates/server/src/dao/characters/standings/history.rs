//! Character NPC standing change history persistence in `game.standing_history`.
//! The shared standing mutation writer inserts one row per actual change; the
//! standings panel reads pairs newest-first through standingMgr. Rows are
//! removed with their character by the table's `ON DELETE CASCADE`.

use crate::db::{executor::Executor, Error};
use tokio_postgres::Row;

/// One standing change to journal.
pub(crate) struct NewEntry<'a> {
    pub char_id: i64,
    pub from_id: i64,
    pub old_standing: f64,
    pub new_standing: f64,
    /// `eve/common/script/mgt/appLogConst.py` event constant of the cause.
    pub event_type_id: i64,
    /// Client FILETIME at which the mutation was requested, captured before locks.
    pub occurred_at: i64,
    /// GM actor or script-caller character; no foreign key so actor deletion keeps history.
    pub actor_id: Option<i64>,
    /// Localization message ID naming the mission, when the cause is a mission.
    pub mission_name_id: Option<i64>,
    /// Fallback or free-form reason the client shows when the event carries no name.
    pub message: &'a str,
}

pub(crate) struct HistoryRow {
    pub event_id: i64,
    pub from_id: i64,
    pub old_standing: f64,
    pub new_standing: f64,
    pub event_type_id: i64,
    pub occurred_at: i64,
    pub actor_id: Option<i64>,
    pub mission_name_id: Option<i64>,
    pub message: String,
}
impl HistoryRow {
    fn from_row(row: &Row) -> Self {
        Self {
            event_id: row.get("event_id"),
            from_id: row.get("from_id"),
            old_standing: row.get("old_standing"),
            new_standing: row.get("new_standing"),
            event_type_id: row.get("event_type_id"),
            occurred_at: row.get("occurred_at"),
            actor_id: row.get("actor_id"),
            mission_name_id: row.get("mission_name_id"),
            message: row.get("message"),
        }
    }
}

pub(crate) async fn insert(ex: &(impl Executor + ?Sized), entry: &NewEntry<'_>) -> Result<(), Error> {
    ex.execute(
        "INSERT INTO game.standing_history (char_id, from_id, old_standing, new_standing, event_type_id, occurred_at, actor_id, mission_name_id, message)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
        &[&entry.char_id, &entry.from_id, &entry.old_standing, &entry.new_standing,
          &entry.event_type_id, &entry.occurred_at, &entry.actor_id, &entry.mission_name_id, &entry.message],
    ).await?;
    Ok(())
}

/// History for one (character, owner) pair, newest first by insertion order.
pub(crate) async fn list(ex: &(impl Executor + ?Sized), character: i64, from: i64) -> Result<Vec<HistoryRow>, Error> {
    Ok(ex.query(
        "SELECT event_id, from_id, old_standing, new_standing, event_type_id, occurred_at, actor_id, mission_name_id, message
         FROM game.standing_history WHERE char_id = $1 AND from_id = $2 ORDER BY event_id DESC",
        &[&character, &from],
    ).await?.iter().map(HistoryRow::from_row).collect())
}
