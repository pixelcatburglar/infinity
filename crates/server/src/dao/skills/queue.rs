//! Skill queue reads, writes and finished-entry collection in `game.skill_queue`.
//! This DAO has no direct client mirror or wire contract; owning services supply those.
//! Training timing, transactions, refusals, notifications and World are caller concerns.
//! start_time/end_time hold the full queue state: an all-None head is paused; a
//! Some/None head is active but blocked by a zero training rate; rows past a
//! stopped time cursor carry no times. `active_characters` selects rows with a
//! finite end_time, so blocked queues keep no training deadline and are not
//! seeded for wakeup; they recover through real attribute or expiry changes.
use crate::db::{executor::Executor, Error};
use tokio_postgres::Row;

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct QueueEntry { pub position: i32, pub type_id: i32, pub to_level: i32, pub start_sp: i64, pub dest_sp: i64, pub start_time: Option<i64>, pub end_time: Option<i64> }
impl QueueEntry { fn from_row(r: &Row) -> Self { Self { position: r.get("position"), type_id: r.get("type_id"), to_level: r.get("to_level"), start_sp: r.get("start_sp"), dest_sp: r.get("dest_sp"), start_time: r.get("start_time"), end_time: r.get("end_time") } } }
pub(crate) struct Active { pub type_id: i32, pub start_sp: i64, pub dest_sp: i64, pub start_time: Option<i64>, pub end_time: Option<i64> }
impl Active { fn from_row(r: &Row) -> Self { Self { type_id: r.get("type_id"), start_sp: r.get("start_sp"), dest_sp: r.get("dest_sp"), start_time: r.get("start_time"), end_time: r.get("end_time") } } }
pub(crate) async fn active_characters(ex: &(impl Executor + ?Sized)) -> Result<Vec<i64>, Error> {
    Ok(ex.query("SELECT character_id FROM game.skill_queue WHERE position = 0 AND end_time IS NOT NULL ORDER BY character_id", &[]).await?.iter().map(|r| r.get("character_id")).collect())
}
pub(crate) async fn active(ex: &(impl Executor + ?Sized), char_id: i64) -> Result<Option<Active>, Error> {
    Ok(ex.query_opt("SELECT type_id, start_sp, dest_sp, start_time, end_time FROM game.skill_queue WHERE character_id = $1 AND position = 0", &[&char_id]).await?.as_ref().map(Active::from_row))
}
pub(crate) async fn list(ex: &(impl Executor + ?Sized), char_id: i64) -> Result<Vec<QueueEntry>, Error> {
    Ok(ex.query("SELECT position, type_id, to_level, start_sp, dest_sp, start_time, end_time\n             FROM game.skill_queue WHERE character_id = $1 ORDER BY position", &[&char_id]).await?.iter().map(QueueEntry::from_row).collect())
}
pub(crate) async fn clear(ex: &(impl Executor + ?Sized), char_id: i64) -> Result<(), Error> {
    ex.execute("DELETE FROM game.skill_queue WHERE character_id = $1", &[&char_id]).await?; Ok(())
}
pub(crate) async fn insert(ex: &(impl Executor + ?Sized), char_id: i64, entry: &QueueEntry) -> Result<(), Error> {
    ex.execute("INSERT INTO game.skill_queue (character_id, position, type_id, to_level, start_sp, dest_sp, start_time, end_time)\n                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8)", &[&char_id, &entry.position, &entry.type_id, &entry.to_level, &entry.start_sp, &entry.dest_sp, &entry.start_time, &entry.end_time]).await?; Ok(())
}
