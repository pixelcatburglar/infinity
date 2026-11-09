//! Personal skill plan persistence for the public gateway in `game.skill_plans`.
//! This DAO has no direct client mirror or wire contract; the gateway supplies its response contract.
//! UUID generation, transactions, refusals, notifications and World are caller concerns.
use crate::db::{self, executor::Executor};
use tokio_postgres::Row;
const PLAN_COLUMNS: &str = "name, description, skill_ids, levels, plan_id";
/// Server startup audit page size bounds memory use.
const AUDIT_PAGE_SIZE: i64 = 100;
pub(crate) struct Plan { pub name: String, pub description: String, pub skill_ids: Vec<i32>, pub levels: Vec<i32>, pub plan_id: Vec<u8> }
impl Plan { fn from_row(r: &Row) -> Self { Self { name: r.get("name"), description: r.get("description"), skill_ids: r.get("skill_ids"), levels: r.get("levels"), plan_id: r.get("plan_id") } } }
struct PlanId { plan_id: Vec<u8> }
impl PlanId { fn from_row(r: &Row) -> Self { Self { plan_id: r.get("plan_id") } } }
#[derive(Clone, Copy)]
pub(crate) enum Field { Name, Description }
pub(crate) async fn create(ex: &(impl Executor + ?Sized), id: &[u8], me: i64, name: &str, description: &str, skills: &[i32], levels: &[i32]) -> Result<u64, db::Error> {
    ex.execute("INSERT INTO game.skill_plans (plan_id, character_id, name, description, skill_ids, levels) VALUES ($1, $2, $3, $4, $5, $6)", &[&id, &me, &name, &description, &skills, &levels]).await
}
pub(crate) async fn all(ex: &(impl Executor + ?Sized), me: i64) -> Result<Vec<Vec<u8>>, db::Error> {
    Ok(ex.query("SELECT plan_id FROM game.skill_plans WHERE character_id = $1 ORDER BY created_at", &[&me]).await?.iter().map(PlanId::from_row).map(|r| r.plan_id).collect())
}
/// Startup validation pages legacy plan definitions without changing them.
pub(crate) async fn audit_page(ex: &(impl Executor + ?Sized), after: &[u8]) -> Result<Vec<(i64, Plan)>, db::Error> {
    Ok(ex.query(&format!("SELECT character_id, {PLAN_COLUMNS} FROM game.skill_plans WHERE plan_id > $1 ORDER BY plan_id LIMIT $2"), &[&after, &AUDIT_PAGE_SIZE]).await?.iter().map(|row| (row.get("character_id"), Plan::from_row(row))).collect())
}
pub(crate) async fn get(ex: &(impl Executor + ?Sized), id: &[u8], owner: Option<i64>) -> Result<Option<Plan>, db::Error> {
    Ok(ex.query_opt(&format!("SELECT {PLAN_COLUMNS} FROM game.skill_plans WHERE plan_id = $1 AND ($2::bigint IS NULL OR character_id = $2)"), &[&id, &owner]).await?.as_ref().map(Plan::from_row))
}
pub(crate) async fn lock(ex: &(impl Executor + ?Sized), id: &[u8]) -> Result<bool, db::Error> {
    Ok(ex.query_opt("SELECT plan_id FROM game.skill_plans WHERE plan_id = $1 FOR UPDATE", &[&id]).await?.is_some())
}
pub(crate) async fn lock_many(ex: &(impl Executor + ?Sized), ids: &[Vec<u8>]) -> Result<(), db::Error> {
    ex.query("SELECT plan_id FROM game.skill_plans WHERE plan_id = ANY($1) ORDER BY plan_id FOR UPDATE", &[&ids]).await?;
    Ok(())
}
pub(crate) async fn delete(ex: &(impl Executor + ?Sized), me: i64, id: &[u8]) -> Result<u64, db::Error> {
    ex.execute("DELETE FROM game.skill_plans WHERE character_id = $1 AND plan_id = $2", &[&me, &id]).await
}
pub(crate) async fn set_field(ex: &(impl Executor + ?Sized), me: i64, id: &[u8], field: Field, value: &str) -> Result<u64, db::Error> {
    let column = match field { Field::Name => "name", Field::Description => "description" };
    ex.execute(&format!("UPDATE game.skill_plans SET {column} = $3 WHERE character_id = $1 AND plan_id = $2"), &[&me, &id, &value]).await
}
pub(crate) async fn set_requirements(ex: &(impl Executor + ?Sized), me: i64, id: &[u8], skills: &[i32], levels: &[i32]) -> Result<u64, db::Error> {
    ex.execute("UPDATE game.skill_plans SET skill_ids = $3, levels = $4 WHERE character_id = $1 AND plan_id = $2", &[&me, &id, &skills, &levels]).await
}

#[cfg(test)] mod tests;
