//! Durable publication order for immutable server-authored Lua packages.
//! No client protocol or activation garbage collection is provided.
use crate::db::{self, executor::Executor};
use super::PackageRow;

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

pub(crate) async fn append_if_changed(ex: &(impl Executor + ?Sized), row: &PackageRow, published_at: i64) -> Result<bool, db::Error> {
    if row.fingerprint.len() != 32 || published_at <= 0 || super::load(ex, &row.namespace, &row.version).await?.as_ref() != Some(row) {
        return Err(db::Error::Progress(anyhow::anyhow!("invalid or missing retained authored package activation")));
    }
    let changed = ex.execute(
        "INSERT INTO game.lua_content_activations (namespace, version, fingerprint, published_at) SELECT $1, $2, $3, $4 WHERE NOT EXISTS (SELECT 1 FROM (SELECT version, fingerprint FROM game.lua_content_activations WHERE namespace = $1 ORDER BY ordinal DESC LIMIT 1) latest WHERE latest.version = $2 AND latest.fingerprint = $3)",
        &[&row.namespace, &row.version, &row.fingerprint, &published_at],
    ).await?;
    Ok(changed != 0)
}

pub(crate) async fn list(ex: &(impl Executor + ?Sized), namespace: &str) -> Result<Vec<ActivationRow>, db::Error> {
    Ok(ex.query("SELECT ordinal, namespace, version, fingerprint, published_at FROM game.lua_content_activations WHERE namespace = $1 ORDER BY ordinal ASC", &[&namespace]).await?.iter().map(ActivationRow::from_row).collect())
}

#[cfg(test)]
mod tests;
