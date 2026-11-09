//! Skill history persistence in `game.skill_history` for skill handler consumers.
//! Event policy and client wire records are supplied by the owning services.
use crate::db::{executor::Executor, Error};
use tokio_postgres::Row;

pub(crate) struct HistoryRow { pub skill_type_id: i32, pub event_type_id: i32, pub absolute_points: i64, pub log_date: i64 }
impl HistoryRow {
    fn from_row(r: &Row) -> Self { Self { skill_type_id: r.get("skill_type_id"), event_type_id: r.get("event_type_id"), absolute_points: r.get("absolute_points"), log_date: r.get("log_date") } }
}
pub(crate) async fn append(ex: &(impl Executor + ?Sized), char_id: i64, type_id: i32, event: i32, points: i64, date: i64) -> Result<i64, Error> {
    Ok(ex.query_one("INSERT INTO game.skill_history (character_id, skill_type_id, event_type_id, absolute_points, log_date) VALUES ($1, $2, $3, $4, $5) RETURNING history_id", &[&char_id, &type_id, &event, &points, &date]).await?.get("history_id"))
}
pub(crate) async fn list(ex: &(impl Executor + ?Sized), char_id: i64, limit: i64) -> Result<Vec<HistoryRow>, Error> {
    Ok(ex.query("SELECT skill_type_id, event_type_id, absolute_points, log_date FROM game.skill_history WHERE character_id = $1 ORDER BY log_date DESC, history_id DESC LIMIT $2", &[&char_id, &limit]).await?.iter().map(HistoryRow::from_row).collect())
}
