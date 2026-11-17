//! Corporation blueprint lock persistence: enacted locks in `game.corp_item_locks`
//! plus the pending reservation column `game.corp_vote_cases.reserved_item_id` that
//! open lockdown or unlock cases hold over one item. The reservation lives in the
//! vote DAO's table but is read here because mutators treat "locked or reserved" as
//! one fact on the shared inventory boundary.
//! This DAO has no direct client mirror or wire contract; policy, transactions and
//! notifications are caller concerns.

use crate::db::{self, executor::Executor};
use tokio_postgres::Row;

/// One enacted corporation blueprint lock.
pub(crate) struct LockRow {
    pub item_id: i64,
    pub corporation_id: i64,
    pub station_id: i64,
    /// The vote case whose sanctioned action created this lock.
    pub case_id: i64,
    pub locked_at: i64,
}

impl LockRow {
    fn from_row(r: &Row) -> Self {
        Self { item_id: r.get("item_id"), corporation_id: r.get("corporation_id"), station_id: r.get("station_id"), case_id: r.get("case_id"), locked_at: r.get("locked_at") }
    }
}

const LOCK_COLUMNS: &str = "item_id,corporation_id,station_id,case_id,locked_at";

/// The enacted lock on `item`, whatever corporation holds it.
pub(crate) async fn get(ex: &(impl Executor + ?Sized), item: i64) -> Result<Option<LockRow>, db::Error> {
    static SQL: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| format!("SELECT {LOCK_COLUMNS} FROM game.corp_item_locks WHERE item_id = $1"));
    Ok(ex.query_opt(SQL.as_str(), &[&item]).await?.as_ref().map(LockRow::from_row))
}

/// Every enacted lock the corporation holds at `station`, in item ID order.
pub(crate) async fn list(ex: &(impl Executor + ?Sized), corporation: i64, station: i64) -> Result<Vec<LockRow>, db::Error> {
    static SQL: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| format!("SELECT {LOCK_COLUMNS} FROM game.corp_item_locks WHERE corporation_id = $1 AND station_id = $2 ORDER BY item_id"));
    Ok(ex.query(SQL.as_str(), &[&corporation, &station]).await?.iter().map(LockRow::from_row).collect())
}

/// Stations where the corporation holds at least one enacted lock.
pub(crate) async fn locations(ex: &(impl Executor + ?Sized), corporation: i64) -> Result<Vec<i64>, db::Error> {
    Ok(ex.query("SELECT DISTINCT station_id FROM game.corp_item_locks WHERE corporation_id = $1 ORDER BY 1", &[&corporation]).await?.iter().map(|row| row.get("station_id")).collect())
}

/// Whether any candidate is under an enacted lock or reserved by a vote case.
/// Mutation checks do not exempt the caller's own case: a pending reservation
/// already makes the item immutable. An empty candidate set never queries.
pub(crate) async fn blocked(ex: &(impl Executor + ?Sized), ids: &[i64]) -> Result<bool, db::Error> {
    if ids.is_empty() { return Ok(false); }
    Ok(ex.query_opt("SELECT 1 WHERE EXISTS (SELECT 1 FROM game.corp_item_locks WHERE item_id = ANY($1)) OR EXISTS (SELECT 1 FROM game.corp_vote_cases WHERE reserved_item_id = ANY($1))", &[&ids]).await?.is_some())
}

/// Whether any candidate is reserved by a case other than `own_case`. The unique
/// reservation index makes a second case's reservation impossible, but the check
/// keeps admission honest under the caller's locks. An empty set never queries.
pub(crate) async fn reserved(ex: &(impl Executor + ?Sized), ids: &[i64], own_case: Option<i64>) -> Result<bool, db::Error> {
    if ids.is_empty() { return Ok(false); }
    Ok(ex.query_opt("SELECT 1 WHERE EXISTS (SELECT 1 FROM game.corp_vote_cases WHERE reserved_item_id = ANY($1) AND ($2::bigint IS NULL OR id <> $2))", &[&ids, &own_case]).await?.is_some())
}

/// Enact a lock; `false` when the item is already locked by any case.
pub(crate) async fn insert(ex: &(impl Executor + ?Sized), row: &LockRow) -> Result<bool, db::Error> {
    Ok(ex.execute("INSERT INTO game.corp_item_locks (item_id,corporation_id,station_id,case_id,locked_at) VALUES ($1,$2,$3,$4,$5) ON CONFLICT (item_id) DO NOTHING", &[&row.item_id, &row.corporation_id, &row.station_id, &row.case_id, &row.locked_at]).await? != 0)
}

/// Delete the lock only while it still belongs to `corporation` and was created by
/// `expected_lock_case`; the removed row tells the caller which case's sanctioned
/// action to rescind.
pub(crate) async fn remove(ex: &(impl Executor + ?Sized), item: i64, corporation: i64, expected_lock_case: i64) -> Result<Option<LockRow>, db::Error> {
    static SQL: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| format!("DELETE FROM game.corp_item_locks WHERE item_id = $1 AND corporation_id = $2 AND case_id = $3 RETURNING {LOCK_COLUMNS}"));
    Ok(ex.query_opt(SQL.as_str(), &[&item, &corporation, &expected_lock_case]).await?.as_ref().map(LockRow::from_row))
}
