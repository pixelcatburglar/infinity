//! Personal plan milestone definition persistence in `game.skill_plan_milestones`.
//! The gateway supplies protocol shapes; transactions and ownership are service concerns.
//! This module does not evaluate skills or send notifications.
use crate::db::{self, executor::Executor};
use tokio_postgres::Row;

pub(crate) struct Milestone { pub milestone_id: Vec<u8>, pub plan_id: Vec<u8>, pub kind: String, pub type_id: i32, pub level: Option<i32>, pub description: String }
impl Milestone {
    fn from_row(row: &Row) -> Self { Self { milestone_id: row.get("milestone_id"), plan_id: row.get("plan_id"), kind: row.get("kind"), type_id: row.get("type_id"), level: row.get("level"), description: row.get("description") } }
}
pub(crate) async fn list(ex: &(impl Executor + ?Sized), plan_id: &[u8]) -> Result<Vec<Milestone>, db::Error> {
    Ok(ex.query("SELECT milestone_id, plan_id, kind, type_id, level, description FROM game.skill_plan_milestones WHERE plan_id = $1 ORDER BY created_at, milestone_id", &[&plan_id]).await?.iter().map(Milestone::from_row).collect())
}
pub(crate) async fn for_plans(ex: &(impl Executor + ?Sized), plan_ids: &[&[u8]]) -> Result<Vec<Milestone>, db::Error> {
    Ok(ex.query("SELECT milestone_id, plan_id, kind, type_id, level, description FROM game.skill_plan_milestones WHERE plan_id = ANY($1::bytea[]) ORDER BY plan_id, milestone_id", &[&plan_ids]).await?.iter().map(Milestone::from_row).collect())
}
pub(crate) async fn get(ex: &(impl Executor + ?Sized), id: &[u8]) -> Result<Option<Milestone>, db::Error> {
    Ok(ex.query_opt("SELECT milestone_id, plan_id, kind, type_id, level, description FROM game.skill_plan_milestones WHERE milestone_id = $1", &[&id]).await?.as_ref().map(Milestone::from_row))
}
/// Definitions owned by the viewer plus shared definitions already viewed.
pub(crate) async fn for_viewer(ex: &(impl Executor + ?Sized), character_id: i64) -> Result<Vec<Milestone>, db::Error> {
    Ok(ex.query("SELECT m.milestone_id, m.plan_id, m.kind, m.type_id, m.level, m.description FROM game.skill_plan_milestones m JOIN game.skill_plans p ON p.plan_id = m.plan_id WHERE p.character_id = $1 OR EXISTS (SELECT 1 FROM game.skill_plan_milestone_progress s WHERE s.character_id = $1 AND s.milestone_id = m.milestone_id) ORDER BY m.plan_id, m.milestone_id", &[&character_id]).await?.iter().map(Milestone::from_row).collect())
}
pub(crate) async fn create(ex: &(impl Executor + ?Sized), id: &[u8], plan_id: &[u8], kind: &str, type_id: i32, level: Option<i32>) -> Result<u64, db::Error> {
    ex.execute("INSERT INTO game.skill_plan_milestones(milestone_id, plan_id, kind, type_id, level) VALUES ($1, $2, $3, $4, $5)", &[&id, &plan_id, &kind, &type_id, &level]).await
}
pub(crate) async fn delete(ex: &(impl Executor + ?Sized), id: &[u8], character_id: i64) -> Result<u64, db::Error> {
    ex.execute("DELETE FROM game.skill_plan_milestones m USING game.skill_plans p WHERE m.milestone_id = $1 AND p.plan_id = m.plan_id AND p.character_id = $2", &[&id, &character_id]).await
}
pub(crate) async fn set_description(ex: &(impl Executor + ?Sized), id: &[u8], character_id: i64, description: &str) -> Result<u64, db::Error> {
    ex.execute("UPDATE game.skill_plan_milestones m SET description = $3 FROM game.skill_plans p WHERE m.milestone_id = $1 AND p.plan_id = m.plan_id AND p.character_id = $2", &[&id, &character_id, &description]).await
}
