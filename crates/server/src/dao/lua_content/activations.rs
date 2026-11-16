//! Durable publication order for immutable server-authored Lua packages.
//! No client protocol or activation garbage collection is provided.
//!
//! Lock order for every writer of retained packages, domain state or delivery
//! receipts: the transaction-scoped namespace advisory lock from
//! `lock_namespace` first: `publish` takes it exclusively, prepared
//! deliveries take it shared, then package, domain and delivery rows in that
//! order. An operation spanning several namespaces locks them in ascending
//! lexicographic order, and no lock other than a namespace lock may be taken
//! before it. No Lua, World or service call may be awaited while a namespace
//! lock is held. This module never opens a transaction or touches the outbox.
use crate::db::{self, executor::Executor};
use super::PackageRow;

/// Fixed advisory-lock scope prefix; PostgreSQL hashes it to a stable key shared
/// by every server process, never a per-process hash map.
const LOCK_PREFIX: &str = "lua-content-activations:";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ActivationRow {
    pub ordinal: i64,
    pub namespace: String,
    pub version: String,
    pub fingerprint: Vec<u8>,
    pub published_at: i64,
}

impl ActivationRow {
    fn from_row(row: &tokio_postgres::Row) -> Self {
        Self {
            ordinal: row.get("ordinal"),
            namespace: row.get("namespace"),
            version: row.get("version"),
            fingerprint: row.get("fingerprint"),
            published_at: row.get("published_at"),
        }
    }
}

/// Transaction-scoped advisory fence for one publication namespace: shared for
/// prepared reads that must hold back publication, exclusive for `publish`.
/// The caller must hold a transaction; outside one the lock releases at once.
pub(crate) async fn lock_namespace(ex: &(impl Executor + ?Sized), namespace: &str, exclusive: bool) -> Result<(), db::Error> {
    if namespace.is_empty() {
        return Err(db::Error::Progress(anyhow::anyhow!("empty Lua content namespace lock")));
    }
    let key = format!("{LOCK_PREFIX}{namespace}");
    let function = if exclusive { "pg_advisory_xact_lock" } else { "pg_advisory_xact_lock_shared" };
    ex.execute(&format!("SELECT {function}(hashtextextended($1, 0))"), &[&key]).await?;
    Ok(())
}

/// The only writer entry point: take every touched namespace's exclusive lock
/// in lexicographic order, then store packages and append activations in
/// ascending (namespace, version) order inside the caller's transaction.
/// Returns exactly the activation rows this call appended.
pub(crate) async fn publish(ex: &(impl Executor + ?Sized), rows: &[PackageRow], published_at: i64) -> Result<Vec<ActivationRow>, db::Error> {
    let mut namespaces: Vec<&str> = rows.iter().map(|row| row.namespace.as_str()).collect();
    namespaces.sort_unstable();
    namespaces.dedup();
    for namespace in namespaces {
        lock_namespace(ex, namespace, true).await?;
    }
    let mut ordered: Vec<&PackageRow> = rows.iter().collect();
    ordered.sort_by(|a, b| a.namespace.cmp(&b.namespace).then_with(|| a.version.cmp(&b.version)));
    let mut appended = Vec::new();
    for row in ordered {
        super::store(ex, row).await?;
        if let Some(activation) = append_if_changed(ex, row, published_at).await? {
            appended.push(activation);
        }
    }
    Ok(appended)
}

async fn append_if_changed(ex: &(impl Executor + ?Sized), row: &PackageRow, published_at: i64) -> Result<Option<ActivationRow>, db::Error> {
    if row.fingerprint.len() != 32 || published_at <= 0 || super::load(ex, &row.namespace, &row.version).await?.as_ref() != Some(row) {
        return Err(db::Error::Progress(anyhow::anyhow!("invalid or missing retained authored package activation")));
    }
    Ok(ex.query_opt(
        "INSERT INTO game.lua_content_activations (namespace, version, fingerprint, published_at) SELECT $1, $2, $3, $4 WHERE NOT EXISTS (SELECT 1 FROM (SELECT version, fingerprint FROM game.lua_content_activations WHERE namespace = $1 ORDER BY ordinal DESC LIMIT 1) latest WHERE latest.version = $2 AND latest.fingerprint = $3) RETURNING ordinal, namespace, version, fingerprint, published_at",
        &[&row.namespace, &row.version, &row.fingerprint, &published_at],
    ).await?.map(|row| ActivationRow::from_row(&row)))
}

/// Whole-table read used once by the startup catalogue; callers group by
/// namespace locally, so the statement returns plain insertion order.
pub(crate) async fn load_all(ex: &(impl Executor + ?Sized)) -> Result<Vec<ActivationRow>, db::Error> {
    Ok(ex.query("SELECT ordinal, namespace, version, fingerprint, published_at FROM game.lua_content_activations", &[]).await?.iter().map(ActivationRow::from_row).collect())
}

#[cfg(all(test, feature = "tests-storage"))]
mod tests;
