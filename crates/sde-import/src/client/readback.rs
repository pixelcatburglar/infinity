//! Complete client-import database readback against the prepared exact rows.
mod typed;
pub(crate) use typed::{prepare_typed, ProjectionContext, TypedExpected};
mod unchanged;
pub(super) use unchanged::{snapshot_unmigrated, UnmigratedSnapshot};
pub use unchanged::UnmigratedReadback;

use std::collections::{BTreeMap, BTreeSet};
use anyhow::{anyhow, Context, Result};
use crate::report::{diff::{self, ExactValue}, manifest::ReadbackDifference, ReportKey};
use super::{Decoded, families::FAMILIES};

#[derive(Clone, Debug, Default)]
pub struct FamilyReadback {
    pub status: String,
    pub compared_native_rows: u64,
    pub compared_compat_rows: u64,
    pub compared_typed_rows: u64,
    pub compared_fields: u64,
    pub mismatch_count: u64,
}
#[derive(Clone, Debug, Default)]
pub struct Readback {
    pub families: BTreeMap<String, FamilyReadback>,
    pub differences: Vec<ReadbackDifference>,
    pub terminal_error: Option<String>,
    pub unmigrated: BTreeMap<String, UnmigratedReadback>,
}
impl Readback {
    pub fn passed(&self) -> bool { self.terminal_error.is_none() && self.differences.is_empty() }
    fn record(&mut self, family: &str, view: &str, table: &str, key: ReportKey, expected: Option<&ExactValue>, actual: Option<&ExactValue>) {
        let differences = diff::compare(family, key.clone(), expected, actual, "", "");
        let stats = self.families.get_mut(family).expect("registered family");
        stats.compared_fields += field_count(expected.or(actual));
        stats.mismatch_count += differences.len() as u64;
        self.differences.extend(differences.into_iter().map(|d| ReadbackDifference {
            view: view.into(), table: table.into(), key: key.clone(), path: d.path,
            expected_present: d.old_present, actual_present: d.new_present,
            expected_value: d.old_value, actual_value: d.new_value,
        }));
    }
}
fn field_count(value: Option<&ExactValue>) -> u64 {
    match value {
        Some(ExactValue::Object(v)) => v.values().map(|v| field_count(Some(v))).sum(),
        Some(ExactValue::Array(v)) => v.iter().map(|v| field_count(Some(v))).sum(),
        Some(_) => 1, None => 0,
    }
}
fn exact(text: &str) -> Result<ExactValue> { diff::parse_json(text).map_err(|e| anyhow!(e)) }
fn key(value: &ExactValue) -> Result<(String, ReportKey)> {
    let ExactValue::Object(object) = value else { return Err(anyhow!("row is not an object")) };
    match object.get("_key") {
        Some(ExactValue::String(s)) => Ok((s.clone(), ReportKey::String(s.clone()))),
        Some(ExactValue::Number(n)) => Ok((n.token.clone(), ReportKey::integer(&n.token).map_err(|e| anyhow!(e))?)),
        _ => Err(anyhow!("row has no integer/string _key")),
    }
}
fn expected_rows(family: &str, rows: &[String]) -> Result<BTreeMap<String, (ReportKey, ExactValue)>> {
    let mut result = BTreeMap::new();
    for row in rows {
        let (value, (k, report_key)) = (|| {
            let value = exact(row)?;
            let key = key(&value)?;
            Ok::<_, anyhow::Error>((value, key))
        })().context("/_key").with_context(|| ProjectionContext { family: family.into(), key: None })?;
        if result.contains_key(&k) { return Err(anyhow!("duplicate prepared key")).context("/_key").context(ProjectionContext { family: family.into(), key: Some(report_key) }); }
        result.insert(k, (report_key, value));
    }
    Ok(result)
}
fn raw_target<'a>(family: &'a super::families::Family, view: &str) -> Option<(&'static str, &'a str)> {
    if view == "native" { Some(("sde_native", family.native_table)) }
    else { family.compat_table.map(|table| ("sde_compat", table)) }
}
async fn raw(client: &tokio_postgres::Client, decoded: &Decoded, output: &mut Readback, family: &super::families::Family, view: &str) -> Result<()> {
    let (schema, table) = raw_target(family, view).context("family has no destination for readback")?;
    let source = if view == "native" { &decoded.native_raw } else { &decoded.compat_raw };
    let expected = expected_rows(family.name, source.get(family.name).with_context(|| format!("missing {view} family {}", family.name))?)?;
    let query = format!("SELECT key AS row_key, data::text AS row_data, row_no AS row_number FROM \"{schema}\".\"{table}\" ORDER BY key, row_no");
    let rows = client.query(&query, &[]).await?;
    let stats = output.families.get_mut(family.name).expect("registered family");
    if view == "native" { stats.compared_native_rows += rows.len() as u64; } else { stats.compared_compat_rows += rows.len() as u64; }
    let mut actual: BTreeMap<String, Vec<ExactValue>> = BTreeMap::new();
    for row in rows {
        let k: Option<String> = row.try_get("row_key")?;
        let text: String = row.try_get("row_data")?;
        let row_no: i32 = row.try_get("row_number")?;
        let value = exact(&text).with_context(|| format!("{schema}.{table} row_no {row_no}: invalid data"))?;
        if let Some(k) = k {
            actual.entry(k).or_default().push(value);
        } else {
            let report_key = key(&value).with_context(|| format!("{schema}.{table} row_no {row_no}: null database key and no usable data _key"))?.1;
            output.record(family.name, view, table, report_key, None, Some(&value));
        }
    }
    let keys: BTreeSet<_> = expected.keys().chain(actual.keys()).cloned().collect();
    for k in keys {
        let e = expected.get(&k);
        let a = actual.get(&k);
        let report_key = e.map(|x| x.0.clone()).or_else(|| a.and_then(|v| v.first()).and_then(|v| key(v).ok()).map(|x| x.1)).unwrap_or_else(|| ReportKey::String(k.clone()));
        output.record(family.name, view, table, report_key.clone(), e.map(|x| &x.1), a.and_then(|v| v.first()));
        if let Some(duplicates) = a { for duplicate in duplicates.iter().skip(1) { output.record(family.name, view, table, report_key.clone(), None, Some(duplicate)); } }
    }
    Ok(())
}

async fn collect(client: &tokio_postgres::Client, decoded: &Decoded, expected: TypedExpected, baseline: &UnmigratedSnapshot, result: &mut Readback) -> Result<()> {
    for family in FAMILIES {
        raw(client, decoded, result, family, "native").await?;
        if raw_target(family, "compat").is_some() { raw(client, decoded, result, family, "compat").await?; }
    }
    unchanged::check(client, baseline, result).await?;
    typed::read(client, expected, result).await
}
fn finish(result: &mut Readback, outcome: Result<()>) {
    if let Err(error) = outcome { result.terminal_error = Some(format!("{error:#}")); }
    for stats in result.families.values_mut() {
        stats.status = if result.terminal_error.is_none() && stats.mismatch_count == 0 { "passed" } else { "failed" }.into();
    }
}
pub(super) async fn readback(client: &tokio_postgres::Client, prepared: super::Prepared, baseline: &UnmigratedSnapshot) -> Result<Readback> {
    let super::Prepared { decoded, typed_expected, .. } = prepared;
    let mut result = Readback::default();
    for family in FAMILIES { result.families.insert(family.name.into(), FamilyReadback::default()); }
    let outcome = match client.batch_execute("BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY").await {
        Err(error) => Err(error.into()),
        Ok(()) => {
            let outcome = collect(client, &decoded, typed_expected, baseline, &mut result).await;
            let end = client.batch_execute(if outcome.is_ok() { "COMMIT" } else { "ROLLBACK" }).await;
            match (outcome, end) {
                (Err(error), _) => Err(error),
                (Ok(()), Err(error)) => Err(error.into()),
                (Ok(()), Ok(())) => Ok(()),
            }
        }
    };
    finish(&mut result, outcome);
    Ok(result)
}
#[cfg(test)] mod tests;
