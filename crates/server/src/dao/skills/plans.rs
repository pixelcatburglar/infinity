//! Personal and corporation skill plan persistence in `game.skill_plans`.
//! Exactly one owner per row: a character for personal plans or a player corporation
//! carrying the 16-byte career-path category (all-zero bytes mean "no category").
//! This DAO has no direct client mirror or wire contract; the gateway supplies its response contract.
//! UUID generation, transactions, refusals, notifications and World are caller concerns.
use crate::db::{self, executor::Executor};
use tokio_postgres::Row;
const PLAN_COLUMNS: &str = "name, description, skill_ids, levels, plan_id, character_id, corporation_id, category_id";
/// Server startup audit page size bounds memory use.
const AUDIT_PAGE_SIZE: i64 = 100;

/// Exactly one plan owner; `skill_plans_owner_check` enforces the pairing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Owner { Character(i64), Corporation(i64) }
impl Owner {
    /// (character_id, corporation_id) column values; exactly one side is set.
    pub(crate) fn columns(self) -> (Option<i64>, Option<i64>) {
        match self { Self::Character(id) => (Some(id), None), Self::Corporation(id) => (None, Some(id)) }
    }
    fn from_columns(character: Option<i64>, corporation: Option<i64>) -> Self {
        match (character, corporation) {
            (Some(id), None) => Self::Character(id),
            (None, Some(id)) => Self::Corporation(id),
            _ => unreachable!("skill_plans_owner_check enforces exactly one owner"),
        }
    }
}

pub(crate) struct Plan {
    pub name: String, pub description: String, pub skill_ids: Vec<i32>, pub levels: Vec<i32>,
    pub plan_id: Vec<u8>, pub owner: Owner, pub category: Option<Vec<u8>>,
}
impl Plan {
    fn from_row(r: &Row) -> Self {
        Self {
            name: r.get("name"), description: r.get("description"), skill_ids: r.get("skill_ids"), levels: r.get("levels"), plan_id: r.get("plan_id"),
            owner: Owner::from_columns(r.get("character_id"), r.get("corporation_id")), category: r.get("category_id"),
        }
    }
}
/// Corporation GetAll row: the Summary wire shape carries no requirement arrays.
pub(crate) struct Summary { pub plan_id: Vec<u8>, pub name: String, pub description: String, pub category: Vec<u8> }
impl Summary {
    fn from_row(r: &Row) -> Self { Self { plan_id: r.get("plan_id"), name: r.get("name"), description: r.get("description"), category: r.get("category") } }
}
struct PlanId { plan_id: Vec<u8> }
impl PlanId { fn from_row(r: &Row) -> Self { Self { plan_id: r.get("plan_id") } } }
#[derive(Clone, Copy)]
pub(crate) enum Field { Name, Description }

/// Owner-scoped predicate shared by the guarded reads and writes below.
const OWNER_MATCH: &str = "character_id IS NOT DISTINCT FROM $2 AND corporation_id IS NOT DISTINCT FROM $3";
/// Live-membership predicate for corporation reads, kept in the same statement
/// as the plan rows: the viewer at `$1` is a non-deleted character whose
/// corporation row is absent (an NPC corporation) or not deleted.
const LIVE_MEMBER_CORPORATION: &str = "corporation_id = (SELECT ch.corporation_id FROM game.characters ch LEFT JOIN game.corporations co ON co.corporation_id = ch.corporation_id WHERE ch.character_id = $1 AND NOT ch.deleted AND (co.corporation_id IS NULL OR NOT co.deleted))";

pub(crate) async fn create(ex: &(impl Executor + ?Sized), id: &[u8], owner: Owner, name: &str, description: &str, skills: &[i32], levels: &[i32], category: Option<&[u8]>) -> Result<u64, db::Error> {
    let (character, corporation) = owner.columns();
    ex.execute("INSERT INTO game.skill_plans (plan_id, character_id, corporation_id, category_id, name, description, skill_ids, levels) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)", &[&id, &character, &corporation, &category, &name, &description, &skills, &levels]).await
}
pub(crate) async fn all(ex: &(impl Executor + ?Sized), owner: Owner) -> Result<Vec<Vec<u8>>, db::Error> {
    let (character, corporation) = owner.columns();
    Ok(ex.query("SELECT plan_id FROM game.skill_plans WHERE character_id IS NOT DISTINCT FROM $1 AND corporation_id IS NOT DISTINCT FROM $2 ORDER BY created_at", &[&character, &corporation]).await?.iter().map(PlanId::from_row).map(|r| r.plan_id).collect())
}
/// Corporation plans the viewing member may list for the GetAll Summary shape,
/// in plan order.
pub(crate) async fn summaries_for_member(ex: &(impl Executor + ?Sized), viewer: i64) -> Result<Vec<Summary>, db::Error> {
    Ok(ex.query(&format!("SELECT plan_id, name, description, category_id AS category FROM game.skill_plans WHERE {LIVE_MEMBER_CORPORATION} ORDER BY created_at"), &[&viewer]).await?.iter().map(Summary::from_row).collect())
}
/// One corporation plan the viewing member may read; departed or deleted
/// members see absence through the same statement's membership predicate.
pub(crate) async fn plan_for_member(ex: &(impl Executor + ?Sized), viewer: i64, id: &[u8]) -> Result<Option<Plan>, db::Error> {
    Ok(ex.query_opt(&format!("SELECT {PLAN_COLUMNS} FROM game.skill_plans WHERE plan_id = $2 AND {LIVE_MEMBER_CORPORATION}"), &[&viewer, &id]).await?.as_ref().map(Plan::from_row))
}
/// Startup validation pages legacy plan definitions without changing them.
pub(crate) async fn audit_page(ex: &(impl Executor + ?Sized), after: &[u8]) -> Result<Vec<Plan>, db::Error> {
    Ok(ex.query(&format!("SELECT {PLAN_COLUMNS} FROM game.skill_plans WHERE plan_id > $1 ORDER BY plan_id LIMIT $2"), &[&after, &AUDIT_PAGE_SIZE]).await?.iter().map(Plan::from_row).collect())
}
/// `owner` scopes the read; `None` returns the definition regardless of owner.
pub(crate) async fn get(ex: &(impl Executor + ?Sized), id: &[u8], owner: Option<Owner>) -> Result<Option<Plan>, db::Error> {
    let (character, corporation) = owner.map_or((None, None), Owner::columns);
    Ok(ex.query_opt(&format!("SELECT {PLAN_COLUMNS} FROM game.skill_plans WHERE plan_id = $1 AND (($2::bigint IS NULL AND $3::bigint IS NULL) OR ({OWNER_MATCH}))"), &[&id, &character, &corporation]).await?.as_ref().map(Plan::from_row))
}
pub(crate) async fn lock(ex: &(impl Executor + ?Sized), id: &[u8]) -> Result<bool, db::Error> {
    Ok(ex.query_opt("SELECT plan_id FROM game.skill_plans WHERE plan_id = $1 FOR UPDATE", &[&id]).await?.is_some())
}
pub(crate) async fn lock_many(ex: &(impl Executor + ?Sized), ids: &[Vec<u8>]) -> Result<(), db::Error> {
    ex.query("SELECT plan_id FROM game.skill_plans WHERE plan_id = ANY($1) ORDER BY plan_id FOR UPDATE", &[&ids]).await?;
    Ok(())
}
pub(crate) async fn delete(ex: &(impl Executor + ?Sized), owner: Owner, id: &[u8]) -> Result<u64, db::Error> {
    let (character, corporation) = owner.columns();
    ex.execute(&format!("DELETE FROM game.skill_plans WHERE plan_id = $1 AND {OWNER_MATCH}"), &[&id, &character, &corporation]).await
}
pub(crate) async fn set_field(ex: &(impl Executor + ?Sized), owner: Owner, id: &[u8], field: Field, value: &str) -> Result<u64, db::Error> {
    let (character, corporation) = owner.columns();
    let column = match field { Field::Name => "name", Field::Description => "description" };
    ex.execute(&format!("UPDATE game.skill_plans SET {column} = $4 WHERE plan_id = $1 AND {OWNER_MATCH}"), &[&id, &character, &corporation, &value]).await
}
pub(crate) async fn set_requirements(ex: &(impl Executor + ?Sized), owner: Owner, id: &[u8], skills: &[i32], levels: &[i32]) -> Result<u64, db::Error> {
    let (character, corporation) = owner.columns();
    ex.execute(&format!("UPDATE game.skill_plans SET skill_ids = $4, levels = $5 WHERE plan_id = $1 AND {OWNER_MATCH}"), &[&id, &character, &corporation, &skills, &levels]).await
}
/// Guarded category write; only corporation rows can carry one.
pub(crate) async fn set_category(ex: &(impl Executor + ?Sized), corporation: i64, id: &[u8], category: &[u8]) -> Result<u64, db::Error> {
    ex.execute("UPDATE game.skill_plans SET category_id = $3 WHERE plan_id = $1 AND corporation_id = $2", &[&id, &corporation, &category]).await
}

#[cfg(all(test, feature = "tests-storage"))] mod tests;
