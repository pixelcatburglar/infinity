//! Filesystem-only decoding and immutable input preparation.
//! Client resources are independently decoded; the old SDE is comparison-only
//! for registered families and supplies immutable bytes for the remaining jobs.
//! Layout support is limited to the observed decoder projections; unsupported
//! data and failed validation stop preparation before database mutation.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use anyhow::{bail, Context, Result};
use serde_json::Value;
use crate::report::{ReportKey, manifest::{OwnershipSummary, ReportRun, Reports, UnknownRegion}};
use super::{blueprints, catalogues, dogma, families, input, localization, materials, normalize, ownership, types, validate, Decoded, FamilyRows, Prepared};
/// Exact resource attribution for a filesystem failure.
#[derive(Debug)]
pub struct FamilyContext { pub family: String, pub resource: String, pub stage: &'static str, pub path: Option<&'static str> }
impl std::fmt::Display for FamilyContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {} ({})", self.stage, self.family, self.resource)
    }
}
impl std::error::Error for FamilyContext {}
fn family_context(family: &families::Family, stage: &'static str) -> FamilyContext {
    FamilyContext { family: family.name.into(), resource: family.resource.into(), stage, path: None }
}
/// Run-local retained copy of the hashed SQLite source.
const BLUEPRINT_SOURCE: &str = "source.sqlite";

fn verify_blueprint_source(source: &input::Input, path: &Path) -> Result<()> {
    let mut staged = source.clone();
    staged.path = path.to_owned();
    input::recheck(&staged).context("verifying staged blueprint source SHA-256")
}

fn stage_blueprint_source(source: &input::Input, run: &ReportRun) -> Result<PathBuf> {
    blueprints::check_journals(&source.path).context("checking original blueprint journals")?;
    run.write_sidecar(BLUEPRINT_SOURCE, &source.bytes).context("staging blueprint source")?;
    let path = run.directory().join(BLUEPRINT_SOURCE).canonicalize().context("resolving staged blueprint source")?;
    verify_blueprint_source(source, &path)?;
    Ok(path)
}

/// Decode every registered client source independently before reading the old bundle.
pub fn decode_inputs(root: &Path, reports: &mut Reports, inputs: &mut Vec<input::Input>, run: &ReportRun) -> Result<(FamilyRows, BTreeMap<String, Vec<String>>, BTreeMap<String, Value>)> {
    let mut native = FamilyRows::new();
    let mut raw = BTreeMap::new();
    let mut metadata = BTreeMap::new();
    let start = inputs.len();
    let mut first_error = None;
    for family in families::FAMILIES {
        match input::read_input(root, family.name, family.resource) {
            Ok(source) => inputs.push(source),
            Err(error) => if first_error.is_none() { first_error = Some(error.context(family_context(family, "read"))); },
        }
    }
    if let Some(error) = first_error { return Err(error) }
    for (index, family) in families::FAMILIES.iter().enumerate() {
        let source = &inputs[start + index];
        let rows = (|| -> Result<_> {
        let rows = match family.decoder {
            families::Decoder::Types => types::decode(family.name, &source.bytes)?,
            families::Decoder::Catalogues => catalogues::decode(family.name, &source.bytes)?,
            families::Decoder::Dogma => dogma::decode(family.name, &source.bytes)?,
            families::Decoder::Materials => materials::decode(family.name, &source.bytes)?,
            families::Decoder::Blueprints => {
                let staged = stage_blueprint_source(source, run)?;
                metadata.insert(family.name.to_owned(), serde_json::json!({
                    "source_relative_path": source.relative_path, "source_sha256": source.sha256,
                    "owned_copy": run.relative_path(BLUEPRINT_SOURCE), "owned_copy_sha256": source.sha256,
                }));
                let decoded = blueprints::decode(&staged)?;
                raw.insert(family.name.to_owned(), decoded.raw_records);
                decoded.records
            }
            families::Decoder::Localization => {
                let decoded = localization::decode(family.name, &source.bytes)?;
                metadata.insert(family.name.to_owned(), decoded.metadata);
                decoded.records
            }
        };
        Ok(rows)
        })().with_context(|| family_context(family, "decode"))?;
        inputs[start + index].decoded_records = Some(rows.len() as u64);
        let source = &inputs[start + index];
        (|| -> Result<()> {
        if matches!(family.decoder, families::Decoder::Types | families::Decoder::Catalogues | families::Decoder::Dogma | families::Decoder::Materials) {
            let ownership = ownership::analyze(family.name, &source.bytes)?;
            if ownership.unowned_bytes != 0 { bail!("{} has {} unowned bytes", family.name, ownership.unowned_bytes) }
            reports.ownership_summary.push(OwnershipSummary {
                family: family.name.into(), source_relative_path: source.relative_path.clone(), source_sha256: source.sha256.clone(),
                source_bytes: ownership.source_bytes as u64, owned_bytes: ownership.owned_bytes as u64,
                unknown_bytes: ownership.unknown_bytes as u64, unowned_bytes: ownership.unowned_bytes as u64,
            });
            for interval in ownership.intervals.into_iter().filter(|i| i.kind == ownership::Kind::Unknown) {
                let owner_keys = interval.owner_keys.into_iter().map(|k| ReportKey::integer(&k.to_string()).map_err(anyhow::Error::msg)).collect::<Result<Vec<_>>>()?;
                reports.unknown_regions.push(UnknownRegion {
                    family: family.name.into(), key: owner_keys.first().cloned(), owner_keys,
                    source_relative_path: source.relative_path.clone(), source_sha256: source.sha256.clone(),
                    offset: interval.offset as u64, length: interval.length as u64, bit_range: None,
                    classification: "unknown".into(), reason: interval.label, hex: interval.hex, sidecar_path: None, sidecar_sha256: None,
                });
            }
        }
        input::recheck(source).with_context(|| format!("checking {} source identity", family.name))?;
        Ok(())
        })().with_context(|| family_context(family, "ownership or identity"))?;
        native.insert(family.name.to_owned(), rows);
    }
    Ok((native, raw, metadata))
}
/// Prepare decoded client rows and all old-bundle COPY bytes before connecting.
pub fn prepare(client_dir: &Path, sde_root: &Path, typed_sql: &Path, reports: &mut Reports, inputs: &mut Vec<input::Input>, run: &ReportRun) -> Result<Prepared> {
    let (native, mut native_raw, metadata) = decode_inputs(client_dir, reports, inputs, run)?;
    let (mut jobs, old_native, old_inputs) = crate::collect_jobs(sde_root, inputs)?;
    let validation = validate::validate(&native, &old_native)?;
    for mut warning in validation.warnings {
        if let Some(source) = inputs.iter().find(|source| source.family == warning.family) {
            warning.source_relative_path.clone_from(&source.relative_path);
            warning.source_sha256.clone_from(&source.sha256);
        }
        reports.issues.push(warning);
    }
    let compat = normalize::normalize(&native)?;
    for (family, rows) in &native {
        if !native_raw.contains_key(family) {
            native_raw.insert(family.clone(), rows.iter().map(serde_json::to_string).collect::<serde_json::Result<Vec<_>>>()?);
        }
    }
    let mut compat_raw = BTreeMap::new();
    for (family, rows) in &compat {
        let raw = if family == "blueprints" {
            native_raw.get(family).context("missing exact blueprint rows")?.clone()
        } else {
            rows.iter().map(serde_json::to_string).collect::<serde_json::Result<Vec<_>>>()?
        };
        compat_raw.insert(family.clone(), raw);
    }
    let decoded = Decoded { native, native_raw, compat_raw };
    for family in families::FAMILIES {
        let source = inputs.iter().find(|input| input.family == family.name).context("missing client input identity")?;
        let old_source = old_inputs.iter().find(|input| input.family == family.name &&
            input.relative_path.split('/').next().is_some_and(|s| s.starts_with("eve-online-client-static-data-")))
            .with_context(|| family_context(family, "missing native snapshot"))?;
        let old_text = std::str::from_utf8(&old_source.bytes).with_context(|| FamilyContext { family: family.name.into(), resource: old_source.relative_path.clone(), stage: "snapshot decode", path: None })?;
        let mut old = exact_rows(old_text.lines().filter(|line| !line.trim().is_empty()).map(|line| crate::sanitize(line.trim()).into_owned()))
            .with_context(|| FamilyContext { family: family.name.into(), resource: old_source.relative_path.clone(), stage: "snapshot decode", path: None })?;
        if let Some(projection) = project_language(family, old_source, &mut old)? {
            reports.issues.push(crate::report::manifest::Issue {
                severity: "warning".into(), code: "snapshot-projection".into(), family: family.name.into(),
                key: None, path: "/languageID".into(), message: "Snapshot languageID is resource-family provenance, not payload".into(),
                source_relative_path: old_source.relative_path.clone(), source_sha256: old_source.sha256.clone(),
                evidence: serde_json::json!({
                    "expected_language": projection.expected_language,
                    "validated_rows": projection.validated_rows,
                    "snapshot_relative_path": old_source.relative_path,
                    "snapshot_sha256": old_source.sha256,
                    "reason": "resource-family provenance, not payload"
                }),
            });
        }
        let new = exact_rows(decoded.native_raw.get(family.name).context("missing native family")?.iter().cloned())?;
        for (key, row) in &new {
            check_native_storage(family, key, row)?;
        }
        let keys: std::collections::BTreeSet<_> = old.keys().chain(new.keys()).cloned().collect();
        for key in keys {
            reports.differences.extend(crate::report::source::compare(family.name, key.clone(), old.get(&key), new.get(&key), &source.relative_path, &source.sha256).map_err(anyhow::Error::msg)?);
        }
    }
    let typed_expected = prepare_typed(&decoded)?;
    for family in families::FAMILIES {
        jobs.push(crate::Job { schema: "sde_native", table: family.native_table.into(),
            source: crate::JobSource::ClientRows(decoded.native_raw.get(family.name).context("missing native family")?.clone()) });
        if let Some(table) = family.compat_table {
            jobs.push(crate::Job { schema: "sde_compat", table: table.into(),
                source: crate::JobSource::ClientRows(decoded.compat_raw.get(family.name).context("missing compat family")?.clone()) });
        }
    }
    let sql_parent = typed_sql.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."));
    let sql_name = typed_sql.file_name().and_then(|s| s.to_str()).context("invalid typed SQL filename")?;
    let sql_context = || FamilyContext { family: "typed_sql".into(), resource: sql_name.into(), stage: "SQL read or decode", path: None };
    let sql_input = input::read_input(sql_parent, "typed_sql", sql_name).with_context(sql_context)?;
    inputs.push(sql_input.clone());
    let typed_sql = std::str::from_utf8(&sql_input.bytes).with_context(sql_context)?.to_owned();
    for input in inputs.iter() {
        input::recheck(input).with_context(|| FamilyContext { family: input.family.clone(), resource: input.relative_path.clone(), stage: "identity recheck", path: None })?;
    }
    Ok(Prepared { decoded, typed_expected, inputs: std::mem::take(inputs), jobs, typed_sql, metadata })
}

fn prepare_typed(decoded: &Decoded) -> Result<super::readback::TypedExpected> {
    super::readback::prepare_typed(decoded).map_err(|error| {
        if let Some(context) = error.downcast_ref::<super::readback::ProjectionContext>() {
            if let Some(family) = families::FAMILIES.iter().find(|family| family.name == context.family) {
                return error.context(family_context(family, "typed preflight"));
            }
        }
        error
    })
}
fn project_language(family: &families::Family, source: &input::Input, rows: &mut BTreeMap<ReportKey, crate::report::diff::ExactValue>) -> Result<Option<crate::report::source::LanguageProjection>> {
    crate::report::source::project_snapshot_language(family.name, rows).map_err(anyhow::Error::msg)
        .with_context(|| FamilyContext { family: family.name.into(), resource: source.relative_path.clone(), stage: "snapshot provenance projection", path: Some("/languageID") })
}
fn exact_rows(lines: impl Iterator<Item = String>) -> Result<BTreeMap<ReportKey, crate::report::diff::ExactValue>> {
    use crate::report::diff::ExactValue;
    let mut result = BTreeMap::new();
    for line in lines {
        let value = crate::report::diff::parse_json(&line).map_err(anyhow::Error::msg)?;
        let ExactValue::Object(object) = &value else { bail!("snapshot row is not an object") };
        let key = match object.get("_key").context("snapshot row lacks _key")? {
            ExactValue::String(value) => ReportKey::String(value.clone()),
            ExactValue::Number(number) => ReportKey::integer(&number.token).map_err(anyhow::Error::msg)?,
            _ => bail!("invalid snapshot row key"),
        };
        if result.insert(key, value).is_some() { bail!("duplicate snapshot row key") }
    }
    Ok(result)
}

fn check_native_storage(family: &families::Family, key: &ReportKey, row: &crate::report::diff::ExactValue) -> Result<()> {
    check_jsonb_storage(row, "").with_context(|| super::readback::ProjectionContext { family: family.name.into(), key: Some(key.clone()) })
        .with_context(|| family_context(family, "JSONB preflight"))
}

fn check_jsonb_storage(value: &crate::report::diff::ExactValue, path: &str) -> Result<()> {
    fn find(value: &crate::report::diff::ExactValue) -> Option<(String, &'static str)> {
        use crate::report::diff::ExactValue as V;
        match value {
            V::String(s) if s.contains('\0') => Some((String::new(), "NUL is not supported by JSONB")),
            V::Number(n) if !n.fits_postgres_numeric() => Some((String::new(), "number exceeds PostgreSQL JSONB numeric storage limits")),
            V::Array(items) => items.iter().enumerate().find_map(|(index, item)| find(item).map(|(suffix, reason)| (format!("/{index}{suffix}"), reason))),
            V::Object(object) => object.iter().find_map(|(key, item)| {
                let (suffix, reason) = if key.contains('\0') { Some((String::new(), "NUL is not supported by JSONB")) } else { find(item) }?;
                Some((format!("/{}{suffix}", key.replace('~', "~0").replace('/', "~1")), reason))
            }),
            _ => None,
        }
    }
    if let Some((suffix, reason)) = find(value) { return Err(anyhow::anyhow!(reason)).context(format!("{path}{suffix}")); }
    Ok(())
}

#[cfg(test)]
mod tests;
