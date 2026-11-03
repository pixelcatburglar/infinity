//! Persistent per-character daily Alpha injector cooldowns in `game.character_alpha_injections`.
//! The client reads the deadline through `skillsvc.py`'s `GetNextAlphaInjectionDateTime`.
//! Eligibility, transactions, scheduling and notifications are owned by services.

use crate::db::{self, executor::Executor};
use tokio_postgres::Row;

struct Deadline {
    next_available: i64,
}

impl Deadline {
    fn from_row(row: &Row) -> Self {
        Self { next_available: row.get("next_available") }
    }
}

pub(crate) async fn next(ex: &(impl Executor + ?Sized), char_id: i64) -> Result<i64, db::Error> {
    Ok(ex.query_opt(
        "SELECT next_available FROM game.character_alpha_injections WHERE character_id = $1",
        &[&char_id],
    ).await?.as_ref().map(Deadline::from_row).map_or(0, |row| row.next_available))
}

/// The caller holds the account and character locks and includes this claim in the item/SP transaction.
pub(crate) async fn claim(ex: &(impl Executor + ?Sized), char_id: i64, now: i64, next_available: i64) -> Result<bool, db::Error> {
    Ok(ex.execute(
        "INSERT INTO game.character_alpha_injections (character_id, next_available) VALUES ($1, $2)
         ON CONFLICT (character_id) DO UPDATE SET next_available = EXCLUDED.next_available, available_notified = false
         WHERE game.character_alpha_injections.next_available <= $3",
        &[&char_id, &next_available, &now],
    ).await? == 1)
}

pub(crate) async fn mark_available(ex: &(impl Executor + ?Sized), char_id: i64, expected: i64, now: i64) -> Result<bool, db::Error> {
    Ok(ex.execute(
        "UPDATE game.character_alpha_injections SET available_notified = true
         WHERE character_id = $1 AND next_available = $2 AND next_available <= $3 AND NOT available_notified",
        &[&char_id, &expected, &now],
    ).await? == 1)
}
