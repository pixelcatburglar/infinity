//! Append-only record of applied game schemas in `game.schema_application_history`.
//! Startup database provenance only: no client wire API and no client mirror.
//! All SQL for the table lives here behind `Executor` with rows read by column
//! name; the caller owns the transaction, commit/rollback and refusal policy, so
//! nothing here opens a connection or transaction of its own. There is no
//! recurring work: reads and appends run only inside the schema-on startup.

use crate::db::{self, executor::Executor};

/// The newest recorded schema application.
pub(crate) struct Latest {
    pub(crate) schema_hash: String,
    pub(crate) build_commit: String,
}

/// The history table's newest row and whether the supplied hash was ever recorded.
pub(crate) struct Preflight {
    pub(crate) latest: Option<Latest>,
    pub(crate) known: bool,
}

/// Read the history when its table exists; absent or empty answers none/unknown.
pub(crate) async fn preflight(ex: &(impl Executor + ?Sized), schema_hash: &str) -> Result<Preflight, db::Error> {
    let exists: bool = ex.query_one("SELECT to_regclass('game.schema_application_history') IS NOT NULL AS exists", &[]).await?.get("exists");
    if !exists {
        return Ok(Preflight { latest: None, known: false });
    }
    let row = ex.query_opt(
        "SELECT schema_hash, build_commit, EXISTS (SELECT 1 FROM game.schema_application_history WHERE schema_hash = $1) AS known FROM game.schema_application_history ORDER BY history_id DESC LIMIT 1",
        &[&schema_hash],
    ).await?;
    Ok(match row {
        Some(row) => Preflight {
            latest: Some(Latest { schema_hash: row.get("schema_hash"), build_commit: row.get("build_commit") }),
            known: row.get("known"),
        },
        None => Preflight { latest: None, known: false },
    })
}

/// Insert a history row only when the newest row does not already name this hash and build.
pub(crate) async fn append_if_changed(ex: &(impl Executor + ?Sized), schema_hash: &str, build_commit: &str) -> Result<bool, db::Error> {
    Ok(ex.execute(
        "INSERT INTO game.schema_application_history (schema_hash, build_commit) SELECT $1::text, $2::text WHERE (SELECT (schema_hash, build_commit) FROM game.schema_application_history ORDER BY history_id DESC LIMIT 1) IS DISTINCT FROM ROW($1::text, $2::text)",
        &[&schema_hash, &build_commit],
    ).await? == 1)
}
