//! Immutable authored Lua package bytes; no client-facing data or garbage collection.
use crate::db::{self, executor::Executor};

pub(crate) mod activations;

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct PackageRow {
    pub namespace: String,
    pub version: String,
    pub schema_version: i32,
    pub compiler_identity: String,
    pub registration: Vec<u8>,
    pub source: Vec<u8>,
    pub data: Vec<u8>,
    pub fingerprint: Vec<u8>,
}
impl PackageRow {
    fn from_row(row: &tokio_postgres::Row) -> Self {
        Self { namespace: row.get("namespace"), version: row.get("version"), schema_version: row.get("schema_version"), compiler_identity: row.get("compiler_identity"), registration: row.get("registration"), source: row.get("source"), data: row.get("data"), fingerprint: row.get("fingerprint") }
    }
}
pub(crate) async fn load(ex: &(impl Executor + ?Sized), namespace: &str, version: &str) -> Result<Option<PackageRow>, db::Error> {
    Ok(ex.query_opt("SELECT namespace, version, schema_version, compiler_identity, registration, source, data, fingerprint FROM game.lua_content_versions WHERE namespace = $1 AND version = $2", &[&namespace, &version]).await?.map(|row| PackageRow::from_row(&row)))
}
pub(crate) async fn store(ex: &(impl Executor + ?Sized), row: &PackageRow) -> Result<bool, db::Error> {
    ex.execute("INSERT INTO game.lua_content_versions (namespace, version, schema_version, compiler_identity, registration, source, data, fingerprint) VALUES ($1,$2,$3,$4,$5,$6,$7,$8) ON CONFLICT (namespace, version) DO NOTHING", &[&row.namespace, &row.version, &row.schema_version, &row.compiler_identity, &row.registration, &row.source, &row.data, &row.fingerprint]).await?;
    Ok(load(ex, &row.namespace, &row.version).await?.as_ref() == Some(row))
}
