//! Shared character skill level reads from `game.skills`.
//! This DAO has no direct client mirror; policy and effects belong to callers.
use crate::db::{self, executor::Executor};
use tokio_postgres::Row;
pub(crate) struct SkillLevel { pub type_id: i32, pub level: i32 }
impl SkillLevel { fn from_row(r: &Row) -> Self { Self { type_id: r.get("type_id"), level: r.get("level") } } }
pub(crate) async fn levels(ex: &(impl Executor + ?Sized), char_id: i64) -> Result<Vec<SkillLevel>, db::Error> {
    Ok(ex.query("SELECT type_id, level FROM game.skills WHERE character_id = $1", &[&char_id]).await?.iter().map(SkillLevel::from_row).collect())
}
