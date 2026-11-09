//! Read-only, byte-exact proof for typed tables outside the migrated closure.
use std::collections::{BTreeMap, BTreeSet};
use anyhow::{bail, Context, Result};
use futures_util::TryStreamExt;
use sha2::{Digest, Sha256};
use tokio_postgres::{types::ToSql, Client};
use crate::report::{diff::ExactValue, manifest::ReadbackDifference, ReportKey};
use super::{Readback, FAMILIES};
use sde_import::encoding::hex;

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub struct Column { pub name: String, pub data_type: String, pub not_null: bool }
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub struct TableSnapshot { pub columns: Vec<Column>, pub rows: u64, pub sha256: String }
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct UnmigratedSnapshot {
    /// All physical table names, including the separately verified migrated tables.
    pub tables: BTreeSet<String>,
    pub unmigrated: BTreeMap<String, TableSnapshot>,
}
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct UnmigratedReadback {
    pub status: String,
    pub expected_rows: Option<u64>, pub actual_rows: Option<u64>,
    pub expected_sha256: Option<String>, pub actual_sha256: Option<String>,
    pub expected_columns: Option<Vec<Column>>, pub actual_columns: Option<Vec<Column>>,
}
fn migrated() -> BTreeSet<&'static str> { FAMILIES.iter().flat_map(|f| f.typed_tables.iter().copied()).collect() }
async fn tables(client: &Client) -> Result<BTreeSet<String>> {
    let rows = client.query("SELECT c.relname AS table_name FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname='sde' AND c.relkind IN ('r','p') ORDER BY c.relname", &[]).await?;
    rows.iter().map(|r| r.try_get("table_name").map_err(Into::into)).collect()
}
async fn columns(client: &Client, table: &str) -> Result<Vec<Column>> {
    let rows = client.query("SELECT a.attname AS column_name, pg_catalog.format_type(a.atttypid,a.atttypmod) AS data_type, a.attnotnull AS not_null FROM pg_catalog.pg_attribute a JOIN pg_catalog.pg_class c ON c.oid=a.attrelid JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname='sde' AND c.relname=$1 AND a.attnum>0 AND NOT a.attisdropped ORDER BY a.attnum", &[&table]).await?;
    rows.iter().map(|r| Ok(Column { name: r.try_get("column_name")?, data_type: r.try_get("data_type")?, not_null: r.try_get("not_null")? })).collect()
}
fn add_record(hash: &mut Sha256, record: &[u8]) {
    hash.update((record.len() as u64).to_be_bytes());
    hash.update(record);
}
async fn contents(client: &Client, table: &str) -> Result<(u64, String)> {
    // Identifier comes from pg_catalog, quoted rather than interpolated as SQL syntax.
    let identifier = table.replace('"', "\"\"");
    let sql = format!("SELECT pg_catalog.record_send(t) AS record_bytes FROM sde.\"{identifier}\" AS t ORDER BY record_bytes");
    let stream = client.query_raw(&sql, std::iter::empty::<&(dyn ToSql + Sync)>()).await?;
    futures_util::pin_mut!(stream);
    let mut hash = Sha256::new(); let mut count = 0u64;
    while let Some(row) = stream.try_next().await? {
        let record: &[u8] = row.try_get("record_bytes")?;
        add_record(&mut hash, record);
        count = count.checked_add(1).context("typed row count overflow")?;
    }
    Ok((count, hex(&hash.finalize())))
}
async fn capture(client: &Client) -> Result<UnmigratedSnapshot> {
    let names = tables(client).await?;
    let migrated = migrated();
    if names.is_empty() { bail!("existing sde schema has no typed tables; a prior working baseline is required"); }
    for required in &migrated { if !names.contains(*required) { bail!("existing typed baseline is missing migrated table {required}"); } }
    let mut unmigrated = BTreeMap::new();
    for name in names.iter().filter(|name| !migrated.contains(name.as_str())) {
        let cols = columns(client, name).await?;
        let (rows, sha256) = contents(client, name).await.with_context(|| format!("snapshotting sde.{name}"))?;
        unmigrated.insert(name.clone(), TableSnapshot { columns: cols, rows, sha256 });
    }
    if unmigrated.is_empty() { bail!("existing sde schema has no non-migrated typed baseline"); }
    Ok(UnmigratedSnapshot { tables: names, unmigrated })
}
pub(crate) async fn snapshot_unmigrated(client: &Client) -> Result<UnmigratedSnapshot> {
    client.batch_execute("BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY").await?;
    let result = capture(client).await;
    let end = client.batch_execute(if result.is_ok() { "COMMIT" } else { "ROLLBACK" }).await;
    match (result, end) { (Ok(value), Ok(())) => Ok(value), (Err(error), _) => Err(error), (Ok(_), Err(error)) => Err(error.into()) }
}
fn difference(out: &mut Readback, table: &str, path: &str, expected: Option<String>, actual: Option<String>) {
    out.differences.push(ReadbackDifference { view: "typed".into(), table: table.into(), key: ReportKey::String(table.into()), path: path.into(), expected_present: expected.is_some(), actual_present: actual.is_some(), expected_value: expected.map(ExactValue::String), actual_value: actual.map(ExactValue::String) });
}
fn compare_names(out: &mut Readback, baseline: &UnmigratedSnapshot, current: &BTreeSet<String>) {
    for name in baseline.tables.difference(current) { difference(out, name, "/table", Some("present".into()), None); }
    for name in current.difference(&baseline.tables) { difference(out, name, "/table", None, Some("present".into())); }
}
fn compare_table(out: &mut Readback, name: &str, expected: &TableSnapshot, actual: Option<&TableSnapshot>) {
    let mut stats = UnmigratedReadback { status: "passed".into(), expected_rows: Some(expected.rows), actual_rows: actual.map(|x| x.rows), expected_sha256: Some(expected.sha256.clone()), actual_sha256: actual.map(|x| x.sha256.clone()), expected_columns: Some(expected.columns.clone()), actual_columns: actual.map(|x| x.columns.clone()) };
    if let Some(actual) = actual {
        if expected.columns != actual.columns { if !out.differences.iter().any(|d| d.table == name && d.path == "/columns") { difference(out, name, "/columns", Some(format!("{:?}", expected.columns)), Some(format!("{:?}", actual.columns))); } stats.status = "failed".into(); }
        if expected.rows != actual.rows { difference(out, name, "/row_count", Some(expected.rows.to_string()), Some(actual.rows.to_string())); stats.status = "failed".into(); }
        if expected.sha256 != actual.sha256 { difference(out, name, "/sha256", Some(expected.sha256.clone()), Some(actual.sha256.clone())); stats.status = "failed".into(); }
    } else { stats.status = "failed".into(); }
    out.unmigrated.insert(name.into(), stats);
}
pub(super) async fn check(client: &Client, baseline: &UnmigratedSnapshot, out: &mut Readback) -> Result<()> {
    let current = tables(client).await?;
    compare_names(out, baseline, &current);
    for (name, expected) in &baseline.unmigrated {
        if !current.contains(name) { compare_table(out, name, expected, None); continue; }
        let cols = columns(client, name).await.with_context(|| format!("reading schema of sde.{name}"))?;
        if cols != expected.columns {
            difference(out, name, "/columns", Some(format!("{:?}", expected.columns)), Some(format!("{cols:?}")));
            out.unmigrated.insert(name.clone(), UnmigratedReadback { status: "failed".into(), expected_rows: Some(expected.rows), expected_sha256: Some(expected.sha256.clone()), expected_columns: Some(expected.columns.clone()), actual_columns: Some(cols.clone()), ..UnmigratedReadback::default() });
        }
        let (rows, sha256) = contents(client, name).await.with_context(|| format!("reading contents of sde.{name}"))?;
        compare_table(out, name, expected, Some(&TableSnapshot { columns: cols, rows, sha256 }));
    }
    for name in current.difference(&baseline.tables) {
        let cols = columns(client, name).await.with_context(|| format!("reading schema of extra sde.{name}"))?;
        let (rows, sha256) = contents(client, name).await.with_context(|| format!("reading extra sde.{name}"))?;
        out.unmigrated.insert(name.clone(), UnmigratedReadback { status: "failed".into(), actual_rows: Some(rows), actual_sha256: Some(sha256), actual_columns: Some(cols), ..UnmigratedReadback::default() });
    }
    Ok(())
}
#[cfg(test)] mod tests;
