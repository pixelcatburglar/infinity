//! Imports a dependency-closed client static-data projection and remaining SDE bundles.
//! All source inputs are prepared before any database mutation.
//!
//! Usage: sde-import --sde <dir> --client-dir <dir> --typed-sql <file>
//!     --report-dir <dir> [--jobs N]
//! Connection string comes from `DATABASE_URL`.

use anyhow::{bail, Context, Result};
use bytes::Bytes;
use futures_util::SinkExt;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::Semaphore;
use tokio_postgres::{Client, NoTls};

mod client;
mod report;

/// Importer design: flush COPY data in one-mebibyte chunks.
const COPY_CHUNK: usize = 1 << 20;
/// Importer design: initial extra room for a COPY row beyond the flush threshold.
const COPY_BUFFER_HEADROOM: usize = 4096;
/// JSON syntax: a backslash escape consists of the slash and escaped byte.
const JSON_ESCAPE_BYTES: usize = 2;
/// JSON syntax: the six ASCII bytes of a NUL Unicode escape.
const JSON_NUL_ESCAPE_BYTES: usize = 6;

struct Args {
    sde: PathBuf,
    client_dir: PathBuf,
    report_dir: PathBuf,
    typed_sql: PathBuf,
    jobs: usize,
    db_url: String,
}

fn parse_args() -> Result<Args> {
    let mut sde = None;
    let mut client_dir = None;
    let mut report_dir = None;
    let mut typed_sql = None;
    let mut jobs = None;
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        let value = it.next().with_context(|| format!("{a} needs a value"))?;
        if value.starts_with("--") { bail!("{a} needs a value") }
        match a.as_str() {
            "--sde" if sde.is_none() => sde = Some(PathBuf::from(value)),
            "--client-dir" if client_dir.is_none() => client_dir = Some(PathBuf::from(value)),
            "--report-dir" if report_dir.is_none() => report_dir = Some(PathBuf::from(value)),
            "--typed-sql" if typed_sql.is_none() => typed_sql = Some(PathBuf::from(value)),
            "--jobs" if jobs.is_none() => jobs = Some(value.parse::<usize>()?),
            other => bail!("unknown or duplicate argument {other}"),
        }
    }
    Ok(Args {
        sde: sde.context("--sde <dir> is required")?,
        client_dir: client_dir.context("--client-dir <dir> is required")?,
        report_dir: report_dir.context("--report-dir <dir> is required")?,
        typed_sql: typed_sql.context("--typed-sql <file> is required")?,
        jobs: jobs.unwrap_or(4).max(1),
        db_url: std::env::var("DATABASE_URL").context("DATABASE_URL is not set")?,
    })
}

pub struct Job {
    pub schema: &'static str,
    pub table: String,
    pub source: JobSource,
}

pub enum JobSource {
    PreparedBytes(Arc<Vec<u8>>),
    ClientRows(Vec<String>),
}

/// File stem -> safe lower-case identifier (`mapObjects__celestials` -> `mapobjects__celestials`).
fn table_name(path: &Path) -> String {
    let stem = path.file_stem().unwrap_or_default().to_string_lossy();
    let mut name: String = stem
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '_' })
        .collect();
    if name.starts_with(|c: char| c.is_ascii_digit()) {
        name.insert(0, 't');
    }
    name
}

fn collect_jobs(root: &Path, observed: &mut Vec<client::input::Input>) -> Result<(Vec<Job>, client::FamilyRows, Vec<client::input::Input>)> {
    let mut jobs = Vec::new();
    let mut old_native = client::FamilyRows::new();
    let mut inputs = Vec::new();
    let mut seen = HashSet::new();
    for entry in std::fs::read_dir(root).with_context(|| format!("reading {}", root.display()))? {
        let dir = entry?.path();
        let name = dir.file_name().unwrap_or_default().to_string_lossy().into_owned();
        if !dir.is_dir() || !name.ends_with("-jsonl") { continue }
        let schema = if name.starts_with("eve-online-client-static-data-") {
            "sde_native"
        } else if name.starts_with("eve-online-static-data-") {
            "sde_compat"
        } else { continue };
        let mut files = std::fs::read_dir(&dir)?.map(|entry| entry.map(|e| e.path())).collect::<std::io::Result<Vec<_>>>()?;
        files.retain(|path| path.extension().is_some_and(|ext| ext == "jsonl"));
        files.sort();
        for path in files {
            let table = table_name(&path);
            if !seen.insert((schema, table.clone())) {
                bail!("table name collision in {schema}: {table} ({})", path.display());
            }
            let relative = path.strip_prefix(root)?.to_str().context("non-UTF-8 SDE input path")?;
            let context = || client::prepare::FamilyContext { family: table.clone(), resource: relative.into(), stage: "snapshot read or decode", path: None };
            let mut input = client::input::read_input(root, &table, relative).with_context(context)?;
            observed.push(input.clone());
            let text = std::str::from_utf8(&input.bytes).with_context(context)?;
            let mut snapshot = Vec::new();
            let mut record_count = 0u64;
            for (n, line) in text.lines().enumerate() {
                let line = line.trim();
                if line.is_empty() { continue }
                let sanitized = sanitize(line);
                let value: serde_json::Value = serde_json::from_str(&sanitized)
                    .with_context(|| format!("{}:{}: invalid JSON", path.display(), n + 1)).with_context(context)?;
                record_count += 1;
                if schema == "sde_native" && client::families::is_migrated(schema, &table) { snapshot.push(value); }
            }
            input.decoded_records = Some(record_count);
            observed.last_mut().expect("observed input").decoded_records = Some(record_count);
            if client::families::is_migrated(schema, &table) {
                if schema == "sde_native" { old_native.insert(table, snapshot); }
            } else {
                jobs.push(Job { schema, table, source: JobSource::PreparedBytes(input.bytes.clone()) });
            }
            inputs.push(input);
        }
    }
    if seen.is_empty() { bail!("no *-jsonl bundles found under {}", root.display()) }
    jobs.sort_by_key(|job| std::cmp::Reverse(match &job.source { JobSource::PreparedBytes(bytes) => bytes.len(), JobSource::ClientRows(rows) => rows.len() }));
    Ok((jobs, old_native, inputs))
}

/// Make Python `json.dumps` output acceptable to PostgreSQL `jsonb`:
/// bare `NaN` / `Infinity` / `-Infinity` become `null`, and `\u0000`
/// escapes (rejected by jsonb) are dropped. Only ASCII is rewritten, so the
/// result stays valid UTF-8.
fn sanitize(line: &str) -> std::borrow::Cow<'_, str> {
    if !line.contains("NaN") && !line.contains("Infinity") && !line.contains("\\u0000") {
        return line.into();
    }
    let b = line.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    let mut in_str = false;
    while i < b.len() {
        let c = b[i];
        if in_str {
            if c == b'\\' {
                if b[i..].starts_with(b"\\u0000") {
                    i += JSON_NUL_ESCAPE_BYTES;
                    continue;
                }
                out.extend_from_slice(&b[i..(i + JSON_ESCAPE_BYTES).min(b.len())]);
                i += JSON_ESCAPE_BYTES;
                continue;
            }
            if c == b'"' {
                in_str = false;
            }
            out.push(c);
            i += 1;
            continue;
        }
        if c == b'"' {
            in_str = true;
        } else if let Some(tok) = [&b"-Infinity"[..], b"Infinity", b"NaN"].into_iter().find(|t| b[i..].starts_with(t)) {
            out.extend_from_slice(b"null");
            i += tok.len();
            continue;
        }
        out.push(c);
        i += 1;
    }
    String::from_utf8(out).expect("sanitize only rewrites ASCII").into()
}

fn key_of(v: &serde_json::Value) -> Option<String> {
    match v.get("_key")? {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Number(n) => Some(n.to_string()),
        other => Some(other.to_string()),
    }
}

fn csv_field(out: &mut Vec<u8>, s: &str) {
    out.push(b'"');
    for &b in s.as_bytes() {
        if b == b'"' {
            out.push(b'"');
        }
        out.push(b);
    }
    out.push(b'"');
}

async fn load_table(client: &mut Client, job: &Job) -> Result<u64> {
    let fq = format!("\"{}\".\"{}\"", job.schema, job.table);
    let tx = client.transaction().await?;
    tx.batch_execute(&format!(
        "DROP TABLE IF EXISTS {fq};
         CREATE TABLE {fq} (row_no integer PRIMARY KEY, key text, data jsonb NOT NULL);"
    ))
    .await?;

    let sink = tx
        .copy_in::<_, Bytes>(&format!("COPY {fq} (row_no, key, data) FROM STDIN (FORMAT csv)"))
        .await?;
    futures_util::pin_mut!(sink);

    let lines: Box<dyn Iterator<Item = std::borrow::Cow<'_, str>> + Send + '_> = match &job.source {
        JobSource::PreparedBytes(bytes) => Box::new(std::str::from_utf8(bytes)?.lines().map(|line| sanitize(line.trim()))),
        JobSource::ClientRows(rows) => Box::new(rows.iter().map(|row| std::borrow::Cow::Borrowed(row.as_str()))),
    };
    let mut buf = Vec::with_capacity(COPY_CHUNK + COPY_BUFFER_HEADROOM);
    let mut rows = 0u64;
    for line in lines {
        if line.is_empty() { continue }
        let key = match &job.source {
            JobSource::PreparedBytes(_) => key_of(&serde_json::from_str(&line)?),
            JobSource::ClientRows(_) => {
                let value = report::diff::parse_json(&line).map_err(anyhow::Error::msg)?;
                let report::diff::ExactValue::Object(object) = value else { bail!("client row is not an object") };
                match object.get("_key").context("client row lacks _key")? {
                    report::diff::ExactValue::String(key) => Some(key.clone()),
                    report::diff::ExactValue::Number(number) => match report::ReportKey::integer(&number.token).map_err(anyhow::Error::msg)? {
                        report::ReportKey::Integer(key) => Some(key),
                        report::ReportKey::String(_) => unreachable!(),
                    },
                    _ => bail!("invalid client row key"),
                }
            }
        };
        buf.extend_from_slice(rows.to_string().as_bytes());
        buf.push(b',');
        if let Some(k) = key {
            csv_field(&mut buf, &k);
        }
        buf.push(b',');
        csv_field(&mut buf, &line);
        buf.push(b'\n');
        rows += 1;
        if buf.len() >= COPY_CHUNK {
            sink.send(Bytes::from(std::mem::take(&mut buf))).await?;
        }
    }
    if !buf.is_empty() {
        sink.send(Bytes::from(buf)).await?;
    }
    let copied = sink.as_mut().finish().await?;
    if copied != rows {
        bail!("{fq}: wrote {rows} rows but COPY reported {copied}");
    }
    tx.batch_execute(&format!("CREATE INDEX ON {fq} (key);")).await?;
    tx.commit().await?;
    Ok(rows)
}

async fn connect(url: &str) -> Result<Client> {
    let (client, conn) = tokio_postgres::connect(url, NoTls).await.context("connecting to PostgreSQL")?;
    tokio::spawn(async move {
        if let Err(e) = conn.await {
            eprintln!("postgres connection error: {e}");
        }
    });
    Ok(client)
}

fn initial_manifest(run_id: String) -> report::manifest::Manifest {
    use report::manifest::*;
    Manifest {
        schema_version: 1, decoder_version: env!("CARGO_PKG_VERSION").into(), status: "preflight-failed".into(),
        phase: "filesystem-preflight".into(),
        run_metadata: RunMetadata { run_id, started_at: format!("{:?}", std::time::SystemTime::now()),
            importer_version: env!("CARGO_PKG_VERSION").into(), ..Default::default() },
        build: BuildIdentity::default(), inputs: Vec::new(), families: Vec::new(), summary: Summary::default(),
    }
}

fn input_identities(inputs: &[client::input::Input]) -> Vec<report::manifest::InputIdentity> {
    inputs.iter().map(|input| {
        let family = client::families::FAMILIES.iter().find(|f| f.name == input.family && input.relative_path.eq_ignore_ascii_case(f.resource));
        let (format, schema) = match family.map(|f| f.decoder) {
            Some(client::families::Decoder::Blueprints) => ("sqlite", "fsdlite cache(key,value)"),
            Some(client::families::Decoder::Localization) => ("pickle", "data-only localization pickle"),
            Some(_) => ("cfsd", "observed cFSD consumer projection"),
            None if input.family == "typed_sql" => ("sql", "typed projection SQL"),
            None => ("jsonl", if input.relative_path.starts_with("eve-online-client-static-data-") { "sde_native" } else { "sde_compat" }),
        };
        report::manifest::InputIdentity {
            family: input.family.clone(), relative_path: input.relative_path.clone(), sha256: input.sha256.clone(),
            byte_length: input.bytes.len() as u64, format: format.into(), schema_identity: Some(schema.into()),
            build_evidence: None,
        }
    }).collect()
}

/// Report all registered destinations when filesystem preparation cannot complete.
fn blocked_manifest(manifest: &mut report::manifest::Manifest, inputs: &[client::input::Input], reports: &mut report::manifest::Reports, run: &report::manifest::ReportRun, error: &anyhow::Error) {
    use report::manifest::*;
    let context = error.downcast_ref::<client::prepare::FamilyContext>();
    if let Some(context) = context {
        let source = inputs.iter().find(|input| input.family == context.family && input.relative_path.eq_ignore_ascii_case(&context.resource));
        let projection = error.downcast_ref::<client::readback::ProjectionContext>();
        let path = error.chain().map(ToString::to_string).filter(|part| part.starts_with('/')).collect::<String>();
        reports.issues.push(Issue { severity: "error".into(), code: "import".into(), family: context.family.clone(),
            key: projection.and_then(|p| p.key.clone()), path: if path.is_empty() { context.path.unwrap_or("").into() } else { path }, message: format!("{error:#}"), source_relative_path: source.map_or_else(|| context.resource.clone(), |s| s.relative_path.clone()),
            source_sha256: source.map_or_else(String::new, |s| s.sha256.clone()), evidence: serde_json::json!({"stage": context.stage}) });
    }
    for family in client::families::FAMILIES {
        let source = inputs.iter().find(|input| input.family == family.name && input.relative_path.eq_ignore_ascii_case(family.resource));
        let failed = context.is_some_and(|c| c.family == family.name);
        let reason = if failed { format!("{} failed; input count {}",
            context.expect("failed context").stage, if source.and_then(|s| s.decoded_records).is_some() { "known" } else { "unknown (reported as 0)" })
        } else if source.and_then(|s| s.decoded_records).is_some() {
            "decoded; dependency closure blocked by filesystem preparation failure".into()
        } else if source.is_none() && context.is_some_and(|c| c.stage == "read") {
            "resource read failed; input count unknown (reported as 0)".into()
        } else {
            "not processed; input count unknown (reported as 0)".into()
        };
        let mut counts = Counts { input_records: source.and_then(|s| s.decoded_records).unwrap_or(0), ..Default::default() };
        counts.snapshot_records = inputs.iter().find(|input| input.family == family.name && input.relative_path.starts_with("eve-online-client-static-data-"))
            .and_then(|input| input.decoded_records).unwrap_or(0);
        counts.unknown_regions = reports.unknown_regions.iter().filter(|region| region.family == family.name).count() as u64;
        if let Some(summary) = reports.ownership_summary.iter().find(|s| s.family == family.name) {
            counts.unknown_bytes = summary.unknown_bytes;
            counts.unowned_bytes = summary.unowned_bytes;
        }
        manifest.families.push(Family { family: family.name.into(), status: "blocked".into(),
            reason: Some(format!("{reason}; unavailable snapshot/ownership counts are unknown (reported as 0); no output committed")),
            required_families: family.required_families.iter().map(|s| (*s).into()).collect(),
            native_table: Some(family.native_table.into()), compat_table: family.compat_table.map(str::to_owned),
            typed_tables: family.typed_tables.iter().map(|s| (*s).into()).collect(), field_ownership: Vec::new(), supplements: Vec::new(), counts,
            report_files: ReportFiles { differences: run.relative_path("differences.jsonl"), unknown_regions: run.relative_path("unknown-regions.jsonl"),
                ownership_summary: run.relative_path("ownership-summary.json"), warnings: run.relative_path("issues.jsonl"), readback: run.relative_path("readback-differences.jsonl") },
            validation: Validation { status: "not-run".into(), issue_count: reports.issues.iter().filter(|i| i.family == family.name).count() as u64 }, readback: Readback { status: "not-run".into(), ..Default::default() } });
    }
    manifest.summary.blocked_families = client::families::FAMILIES.len() as u64;
    manifest.summary.enabled_families = 0;
}
fn field_ownership(family: &str, prepared: &client::Prepared) -> Vec<report::manifest::FieldOwnership> {
    let (fields, typed) = client::families::recovered_fields(family);
    let source = prepared.inputs.iter().find(|i| i.family == family && client::families::FAMILIES.iter().any(|f| f.name == family && i.relative_path.eq_ignore_ascii_case(f.resource))).expect("registered input");
    let mut result: Vec<_> = fields.split_whitespace().map(|field| {
        report::manifest::FieldOwnership { consumer_required: typed.split_whitespace().any(|name| name == field),
            authority: format!("client {} sha256 {}", source.relative_path, source.sha256),
            evidence: "observed native decoder projection".into(), path: format!("/{field}") }
    }).collect();
    let aliases: &[(&str, &str, bool)] = match family {
        "types" => &[("typeNameID", "name", true), ("descriptionID", "description", false)],
        "groups" => &[("groupNameID", "name", true)],
        "categories" => &[("categoryNameID", "name", true)],
        "dogmaattributes" => &[("displayNameID", "displayName", false)],
        _ => &[],
    };
    let english = prepared.inputs.iter().find(|i| i.family == "localization_fsd_en_us" && i.relative_path.eq_ignore_ascii_case("localizationfsd/localization_fsd_en-us.pickle")).expect("English source");
    for &(id, field, required) in aliases {
        result.push(report::manifest::FieldOwnership { path: format!("/compat/{field}"),
            authority: format!("client {} sha256 {}", english.relative_path, english.sha256),
            evidence: format!("English text resolved from native /{id}; no fallback"),
            consumer_required: required || matches!((family, field), ("types", "description") | ("dogmaattributes", "displayName")) });
    }
    for &(source_field, target) in match family {
        "dogmaattributes" => &[("categoryID", "attributeCategoryID")][..],
        "dogmaeffects" => &[("effectName", "name"), ("effectCategory", "effectCategoryID")][..],
        _ => &[],
    } {
        result.push(report::manifest::FieldOwnership { path: format!("/compat/{target}"), authority: format!("client {} sha256 {}", source.relative_path, source.sha256),
            evidence: format!("compat alias of native /{source_field}"), consumer_required: true });
    }
    result
}

fn fill_manifest(manifest: &mut report::manifest::Manifest, prepared: &client::Prepared, reports: &report::manifest::Reports, run: &report::manifest::ReportRun) {
    use report::manifest::*;
    manifest.inputs = input_identities(&prepared.inputs);
    if let Some(input) = prepared.inputs.iter().find(|input| input.family == "_sde" &&
        input.relative_path.split('/').next().is_some_and(|part| part.starts_with("eve-online-static-data-"))) {
        if let Ok(text) = std::str::from_utf8(&input.bytes) {
            for line in text.lines() {
                if let Ok(row) = serde_json::from_str::<serde_json::Value>(&sanitize(line)) {
                    if row.get("_key").and_then(|key| key.as_str()) == Some("sde") {
                        manifest.build.sde_build = row.get("buildNumber").filter(|value| !value.is_null()).map(|value| value.to_string());
                        manifest.build.sde_build_source = manifest.build.sde_build.as_ref().map(|_| input.relative_path.clone());
                    }
                }
            }
        }
    }
    for family in client::families::FAMILIES {
        let differences: Vec<_> = reports.differences.iter().filter(|d| d.family == family.name).collect();
        let mut counts = Counts::default();
        counts.input_records = prepared.decoded.native[family.name].len() as u64;
        counts.client_only_records = differences.iter().filter(|d| d.kind == "client-only").count() as u64;
        counts.sde_only_records = differences.iter().filter(|d| d.kind == "sde-only").count() as u64;
        counts.snapshot_records = prepared.inputs.iter().find(|input| input.family == family.name &&
            input.relative_path.split('/').next().is_some_and(|part| part.starts_with("eve-online-client-static-data-")))
            .map_or(0, |input| input.bytes.split(|byte| *byte == b'\n').filter(|line| !line.iter().all(u8::is_ascii_whitespace)).count() as u64);
        counts.common_records = counts.input_records.saturating_sub(counts.client_only_records);
        counts.changed_fields = differences.iter().filter(|d| d.kind == "changed").count() as u64;
        counts.changed_records = differences.iter().filter(|d| d.kind == "changed").map(|d| &d.key).collect::<std::collections::BTreeSet<_>>().len() as u64;
        counts.representation_only_fields = differences.iter().filter(|d| d.numeric_classification == "f64-representation-only").count() as u64;
        counts.warnings = reports.issues.iter().filter(|i| i.family == family.name && i.severity == "warning").count() as u64;
        counts.missing_required = reports.issues.iter().filter(|i| i.family == family.name && i.code == "missing-required").count() as u64;
        counts.unsupported = reports.issues.iter().filter(|i| i.family == family.name && i.code == "unsupported").count() as u64;
        counts.reference_errors = reports.issues.iter().filter(|i| i.family == family.name && i.code == "reference" && i.severity == "error").count() as u64;
        counts.localization_errors = reports.issues.iter().filter(|i| i.family == family.name && i.code == "localization" && i.severity == "error").count() as u64;
        counts.unknown_regions = reports.unknown_regions.iter().filter(|r| r.family == family.name).count() as u64;
        if let Some(summary) = reports.ownership_summary.iter().find(|s| s.family == family.name) {
            counts.unknown_bytes = summary.unknown_bytes; counts.unowned_bytes = summary.unowned_bytes;
        }
        manifest.families.push(Family {
            family: family.name.into(), status: "enabled".into(), reason: None,
            required_families: family.required_families.iter().map(|s| (*s).into()).collect(),
            native_table: Some(family.native_table.into()), compat_table: family.compat_table.map(str::to_owned),
            typed_tables: family.typed_tables.iter().map(|s| (*s).into()).collect(),
            field_ownership: field_ownership(family.name, prepared),
            supplements: Vec::new(), counts, report_files: ReportFiles {
                differences: run.relative_path("differences.jsonl"), unknown_regions: run.relative_path("unknown-regions.jsonl"),
                ownership_summary: run.relative_path("ownership-summary.json"), warnings: run.relative_path("issues.jsonl"),
                readback: run.relative_path("readback-differences.jsonl"),
            }, validation: Validation { status: "passed".into(), issue_count: 0 },
            readback: Readback { status: "not-run".into(), ..Default::default() },
        });
    }
    for job in &prepared.jobs {
        if client::families::is_migrated(job.schema, &job.table) { continue }
        let JobSource::PreparedBytes(bytes) = &job.source else { unreachable!("unmigrated job must use prepared bytes") };
        let count = std::str::from_utf8(bytes).expect("prepared input was validated").lines().filter(|line| !line.trim().is_empty()).count() as u64;
        if let Some(existing) = manifest.families.iter_mut().find(|f| f.family == job.table) {
            if job.schema == "sde_native" { existing.native_table = Some(job.table.clone()); }
            if job.schema == "sde_compat" { existing.compat_table = Some(job.table.clone()); }
            existing.counts.input_records += count;
            existing.counts.snapshot_records += count;
        } else {
            let counts = Counts { input_records: count, snapshot_records: count, ..Default::default() };
            manifest.families.push(Family { family: job.table.clone(), status: "not-migrated".into(), reason: Some(format!("remaining SDE bundle; input and snapshot counts sum native and compat rows; prepared counts: {}", run.relative_path("prepared-counts.json"))),
                required_families: Vec::new(), native_table: (job.schema == "sde_native").then(|| job.table.clone()),
                compat_table: (job.schema == "sde_compat").then(|| job.table.clone()), typed_tables: Vec::new(), field_ownership: Vec::new(),
                supplements: Vec::new(), counts, report_files: ReportFiles::default(),
                validation: Validation { status: "passed".into(), issue_count: 0 }, readback: Readback { status: "not-run".into(), ..Default::default() } });
        }
    }
    manifest.summary.enabled_families = client::families::FAMILIES.len() as u64;
    manifest.summary.not_migrated_families = manifest.families.iter().filter(|f| f.status == "not-migrated").count() as u64;
    manifest.summary.warning_count = reports.issues.iter().filter(|i| i.severity == "warning").count() as u64;
}

async fn import(args: &Args, manifest: &mut report::manifest::Manifest, reports: &mut report::manifest::Reports, run: &mut report::manifest::ReportRun) -> Result<()> {
    let mut observed = Vec::new();
    let prepared_result = client::prepare::prepare(&args.client_dir, &args.sde, &args.typed_sql, reports, &mut observed, run);
    manifest.inputs = input_identities(&observed);
    let mut prepared = match prepared_result {
        Ok(prepared) => prepared,
        Err(error) => {
            blocked_manifest(manifest, &observed, reports, run, &error);
            return Err(error);
        }
    };
    fill_manifest(manifest, &prepared, reports, run);
    let prepared_counts: std::collections::BTreeMap<String, u64> = prepared.jobs.iter().map(|job| {
        let count = match &job.source {
            JobSource::PreparedBytes(bytes) => std::str::from_utf8(bytes).expect("prepared input was validated").lines().filter(|line| !line.trim().is_empty()).count() as u64,
            JobSource::ClientRows(rows) => rows.len() as u64,
        };
        (format!("{}.{}", job.schema, job.table), count)
    }).collect();
    run.write_sidecar("prepared-counts.json", &serde_json::to_vec(&prepared_counts)?)?;
    for (family, metadata) in &prepared.metadata {
        run.write_sidecar(&format!("{family}-metadata.json"), &serde_json::to_vec(metadata)?)?;
    }
    for (kind, missing) in [("client", manifest.build.client_build.is_none()), ("sde", manifest.build.sde_build.is_none())] {
        if missing {
            reports.issues.push(report::manifest::Issue { severity: "warning".into(), code: "input-identity".into(), family: kind.into(), key: None,
                path: String::new(), message: format!("{kind} build identity unavailable; supplied source hashes identify bytes, not a matching build"),
                source_relative_path: String::new(), source_sha256: String::new(),
                evidence: serde_json::json!({"inputs":manifest.inputs.iter().filter(|i| if kind == "client" { client::families::FAMILIES.iter().any(|f| f.name == i.family && i.relative_path.eq_ignore_ascii_case(f.resource)) } else { i.family == "_sde" }).map(|i| serde_json::json!({"path":i.relative_path,"sha256":i.sha256})).collect::<Vec<_>>()}) });
        }
    }
    manifest.summary.warning_count = reports.issues.iter().filter(|issue| issue.severity == "warning").count() as u64;
    run.update_preflight(manifest)?;
    let mut admin = connect(&args.db_url).await?;
    manifest.phase = "database-preflight".into();
    let preflight = client::preflight_database(&mut admin, &prepared.decoded).await.map_err(|error| {
        reports.issues.push(report::manifest::Issue { severity: "error".into(), code: "persistent-reference".into(), family: "types".into(),
            key: None, path: String::new(), message: format!("{error:#}"), source_relative_path: String::new(), source_sha256: String::new(),
            evidence: serde_json::json!({"phase":"database-preflight"}) });
        error
    })?;
    let baseline = client::snapshot_unmigrated(&admin).await.context("snapshotting unmigrated typed tables before mutation")?;
    println!("database preflight checked {} references in {} columns", preflight.checked_references, preflight.checked_columns.len());
    manifest.status = "import-failed".into();
    manifest.phase = "raw-copy".into();
    admin.batch_execute("CREATE SCHEMA IF NOT EXISTS sde_native; CREATE SCHEMA IF NOT EXISTS sde_compat;").await?;
    let permits = Arc::new(Semaphore::new(args.jobs));
    let mut handles = Vec::new();
    for job in std::mem::take(&mut prepared.jobs) {
        let permit = permits.clone().acquire_owned().await?;
        let url = args.db_url.clone();
        handles.push(tokio::spawn(async move {
            let _permit = permit;
            let mut connection = connect(&url).await?;
            let count = load_table(&mut connection, &job).await.with_context(|| format!("loading {}.{}", job.schema, job.table))?;
            Ok::<_, anyhow::Error>((job.schema, job.table, count))
        }));
    }
    let mut failed = Vec::new();
    let mut copied_counts = std::collections::BTreeMap::new();
    for handle in handles {
        match handle.await {
            Ok(Ok((schema, table, count))) => {
                copied_counts.insert(format!("{schema}.{table}"), count);
                if let Some(family) = manifest.families.iter_mut().find(|f| match schema {
                    "sde_native" => f.native_table.as_deref() == Some(table.as_str()),
                    "sde_compat" => f.compat_table.as_deref() == Some(table.as_str()),
                    _ => false,
                }) {
                    if schema == "sde_native" { family.counts.output_native_records = count; }
                    else { family.counts.output_compat_records = count; }
                } else { failed.push(format!("COPY destination {schema}.{table} missing from manifest")); }
            }
            Ok(Err(error)) => failed.push(format!("{error:#}")),
            Err(error) => failed.push(error.to_string()),
        }
    }
    run.write_sidecar("copy-counts.json", &serde_json::to_vec(&copied_counts)?)?;
    for family in &mut manifest.families {
        if family.status == "not-migrated" {
            family.reason.get_or_insert_with(String::new).push_str(&format!("; copied counts: {}", run.relative_path("copy-counts.json")));
        }
    }
    for (destination, actual) in &copied_counts {
        if prepared_counts.get(destination) != Some(actual) { failed.push(format!("COPY count mismatch for {destination}")); }
    }
    if !failed.is_empty() { bail!("COPY failures: {}", failed.join("; ")) }
    manifest.phase = "typed-sql".into();
    admin.batch_execute(&prepared.typed_sql).await.context("running prepared typed SQL")?;
    manifest.status = "readback-failed".into();
    manifest.phase = "readback".into();
    let result = client::readback_database(&admin, prepared, &baseline).await?;
    for family in &mut manifest.families {
        if let Some(stats) = result.families.get(&family.family) {
            family.readback = report::manifest::Readback { status: stats.status.clone(), compared_native_rows: stats.compared_native_rows,
                compared_compat_rows: stats.compared_compat_rows, compared_typed_rows: stats.compared_typed_rows,
                compared_fields: stats.compared_fields, mismatch_count: stats.mismatch_count };
        }
    }
    manifest.summary.readback_mismatches = result.differences.len() as u64;
    let passed = result.passed();
    if let Some(error) = &result.terminal_error {
        reports.issues.push(report::manifest::Issue { severity: "error".into(), code: "readback".into(), family: String::new(),
            key: None, path: String::new(), message: error.clone(), source_relative_path: String::new(),
            source_sha256: String::new(), evidence: serde_json::json!({"phase":"readback"}) });
    }
    reports.readback_differences = result.differences;
    let proof_path = run.relative_path("typed-unmigrated-readback.json");
    for family in &mut manifest.families {
        if family.status == "not-migrated" {
            family.reason.get_or_insert_with(String::new).push_str(&format!("; typed proof: {proof_path}"));
        }
    }
    for (table, stats) in &result.unmigrated {
        let sources = client::families::unmigrated_typed_sources(table);
        let mut matched = false;
        for family in &mut manifest.families {
            if family.status != "not-migrated" || !sources.contains(&family.family.as_str()) { continue }
            matched = true;
            family.typed_tables.push(table.clone());
            family.readback.compared_typed_rows += stats.actual_rows.unwrap_or(0);
            if stats.status != "passed" { family.readback.status = "failed".into(); family.readback.mismatch_count += 1; }
            else if family.readback.status == "not-run" { family.readback.status = "passed".into(); }
        }
        if !matched {
            use report::manifest::*;
            manifest.families.push(Family { family: format!("sde.{table}"), status: "not-migrated".into(),
                reason: Some(format!("unmigrated typed projection; typed proof: {proof_path}")), required_families: Vec::new(),
                native_table: None, compat_table: None, typed_tables: vec![table.clone()], field_ownership: Vec::new(),
                supplements: Vec::new(), counts: Counts::default(), report_files: ReportFiles::default(),
                validation: Validation { status: "passed".into(), issue_count: 0 },
                readback: Readback { status: stats.status.clone(), compared_typed_rows: stats.actual_rows.unwrap_or(0),
                    mismatch_count: u64::from(stats.status != "passed"), ..Default::default() } });
        }
    }
    manifest.summary.not_migrated_families = manifest.families.iter().filter(|f| f.status == "not-migrated").count() as u64;
    run.write_sidecar("typed-unmigrated-readback.json", &serde_json::to_vec(&serde_json::json!({"baseline": &baseline, "tables": &result.unmigrated}))?)?;
    if !passed { bail!("readback failed with {} mismatches{}", reports.readback_differences.len(), result.terminal_error.as_deref().map(|e| format!(": {e}")).unwrap_or_default()) }
    manifest.status = "success".into();
    manifest.phase = "complete".into();
    Ok(())
}

async fn run_import(args: Args, run_id: String) -> Result<()> {
    let started = Instant::now();
    let mut manifest = initial_manifest(run_id);
    let mut reports = report::manifest::Reports::default();
    let mut run = report::manifest::ReportRun::begin(&args.report_dir, &manifest)?;
    let result = import(&args, &mut manifest, &mut reports, &mut run).await;
    if let Err(error) = &result {
        if error.downcast_ref::<client::prepare::FamilyContext>().is_none() {
            reports.issues.push(report::manifest::Issue { severity: "error".into(), code: "import".into(), family: String::new(),
                key: None, path: String::new(), message: format!("{error:#}"), source_relative_path: String::new(),
                source_sha256: String::new(), evidence: serde_json::Value::Null });
        }
        manifest.summary.error_count = reports.issues.iter().filter(|i| i.severity == "error").count() as u64;
    }
    manifest.run_metadata.finished_at = Some(format!("{:?}", std::time::SystemTime::now()));
    manifest.run_metadata.elapsed_seconds = Some(started.elapsed().as_secs_f64());
    manifest.summary.warning_count = reports.issues.iter().filter(|i| i.severity == "warning").count() as u64;
    for family in &mut manifest.families {
        let issues: Vec<_> = reports.issues.iter().filter(|issue| issue.family == family.family).collect();
        family.counts.warnings = issues.iter().filter(|i| i.severity == "warning").count() as u64;
        family.counts.missing_required = issues.iter().filter(|i| i.code == "missing-required").count() as u64;
        family.counts.unsupported = issues.iter().filter(|i| i.code == "unsupported").count() as u64;
        family.counts.reference_errors = issues.iter().filter(|i| i.severity == "error" && matches!(i.code.as_str(), "reference" | "persistent-reference")).count() as u64;
        family.counts.localization_errors = issues.iter().filter(|i| i.severity == "error" && i.code == "localization").count() as u64;
        family.validation.issue_count = issues.len() as u64;
        if issues.iter().any(|i| i.severity == "error") { family.validation.status = "failed".into(); }
    }
    for family in &manifest.families {
        println!("{} {} authority={} counts={}", family.family, family.status,
            if family.status == "enabled" { "client" } else if family.status == "blocked" { "unavailable" } else { "SDE" }, serde_json::to_string(&family.counts)?);
    }
    for issue in reports.issues.iter().take(5) { eprintln!("{} {}: {}", issue.severity, issue.code, issue.message); }
    run.finalize(&manifest, &reports).context("finalizing import report")?;
    println!("{}: {} errors, {} warnings, {} readback mismatches; manifest: {}",
        manifest.status, manifest.summary.error_count, manifest.summary.warning_count, manifest.summary.readback_mismatches,
        args.report_dir.join("manifest.json").display());
    result?;
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let run_id = report::manifest::new_run_id();
    eprintln!("sde-import run_id={run_id}");
    run_import(parse_args()?, run_id).await
}

#[cfg(test)]
mod tests;
