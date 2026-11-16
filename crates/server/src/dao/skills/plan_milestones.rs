//! Plan milestone definition persistence in `game.skill_plan_milestones`.
//! Owner scope follows the parent `game.skill_plans` row: personal plans are seen by
//! their owner and past viewers, corporation plans only by current live members.
//! The gateway supplies protocol shapes; transactions and ownership are service concerns.
//! This module does not evaluate skills or send notifications.
use crate::db::{self, executor::Executor};
use crate::dao::skills::plans::Owner;
use tokio_postgres::Row;

pub(crate) struct Milestone { pub milestone_id: Vec<u8>, pub plan_id: Vec<u8>, pub kind: String, pub type_id: i32, pub level: Option<i32>, pub description: String }
impl Milestone {
    fn from_row(row: &Row) -> Self { Self { milestone_id: row.get("milestone_id"), plan_id: row.get("plan_id"), kind: row.get("kind"), type_id: row.get("type_id"), level: row.get("level"), description: row.get("description") } }
}
const COLUMNS: &str = "m.milestone_id, m.plan_id, m.kind, m.type_id, m.level, m.description";
pub(crate) async fn list(ex: &(impl Executor + ?Sized), plan_id: &[u8]) -> Result<Vec<Milestone>, db::Error> {
    Ok(ex.query(&format!("SELECT {COLUMNS} FROM game.skill_plan_milestones m WHERE m.plan_id = $1 ORDER BY m.created_at, m.milestone_id"), &[&plan_id]).await?.iter().map(Milestone::from_row).collect())
}
pub(crate) async fn for_plans(ex: &(impl Executor + ?Sized), plan_ids: &[&[u8]]) -> Result<Vec<Milestone>, db::Error> {
    Ok(ex.query(&format!("SELECT {COLUMNS} FROM game.skill_plan_milestones m WHERE m.plan_id = ANY($1::bytea[]) ORDER BY m.plan_id, m.milestone_id"), &[&plan_ids]).await?.iter().map(Milestone::from_row).collect())
}
pub(crate) async fn get(ex: &(impl Executor + ?Sized), id: &[u8]) -> Result<Option<Milestone>, db::Error> {
    Ok(ex.query_opt(&format!("SELECT {COLUMNS} FROM game.skill_plan_milestones m WHERE m.milestone_id = $1"), &[&id]).await?.as_ref().map(Milestone::from_row))
}
/// Definitions the viewer may track: personal plans they own or already viewed, and
/// plans of their current live corporation. A stored snapshot alone grants nothing.
pub(crate) async fn for_viewer(ex: &(impl Executor + ?Sized), character_id: i64) -> Result<Vec<Milestone>, db::Error> {
    Ok(ex.query(&format!("SELECT {COLUMNS} FROM game.skill_plan_milestones m JOIN game.skill_plans p ON p.plan_id = m.plan_id
        WHERE (p.character_id IS NOT NULL AND (p.character_id = $1 OR EXISTS (SELECT 1 FROM game.skill_plan_milestone_progress s WHERE s.character_id = $1 AND s.milestone_id = m.milestone_id)))
           OR (p.corporation_id IS NOT NULL AND EXISTS (SELECT 1 FROM game.characters ch JOIN game.corporations co ON co.corporation_id = ch.corporation_id AND NOT co.deleted WHERE ch.character_id = $1 AND NOT ch.deleted AND ch.corporation_id = p.corporation_id))
        ORDER BY m.plan_id, m.milestone_id"), &[&character_id]).await?.iter().map(Milestone::from_row).collect())
}
pub(crate) async fn create(ex: &(impl Executor + ?Sized), id: &[u8], plan_id: &[u8], kind: &str, type_id: i32, level: Option<i32>) -> Result<u64, db::Error> {
    ex.execute("INSERT INTO game.skill_plan_milestones(milestone_id, plan_id, kind, type_id, level) VALUES ($1, $2, $3, $4, $5)", &[&id, &plan_id, &kind, &type_id, &level]).await
}
/// Guarded by the parent plan's owner; personal plans match their character,
/// corporation plans match the owning corporation.
const OWNER_MATCH: &str = "p.character_id IS NOT DISTINCT FROM $2 AND p.corporation_id IS NOT DISTINCT FROM $3";
pub(crate) async fn delete(ex: &(impl Executor + ?Sized), id: &[u8], owner: Owner) -> Result<u64, db::Error> {
    let (character, corporation) = owner.columns();
    ex.execute(&format!("DELETE FROM game.skill_plan_milestones m USING game.skill_plans p WHERE m.milestone_id = $1 AND p.plan_id = m.plan_id AND {OWNER_MATCH}"), &[&id, &character, &corporation]).await
}
pub(crate) async fn set_description(ex: &(impl Executor + ?Sized), id: &[u8], owner: Owner, description: &str) -> Result<u64, db::Error> {
    let (character, corporation) = owner.columns();
    ex.execute(&format!("UPDATE game.skill_plan_milestones m SET description = $4 FROM game.skill_plans p WHERE m.milestone_id = $1 AND p.plan_id = m.plan_id AND {OWNER_MATCH}"), &[&id, &character, &corporation, &description]).await
}
