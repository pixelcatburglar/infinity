//! Define and durably publish run-specific offline import reports.
//! This writer is not a client service or protocol mirror.
//! Manifests record input identities, family counts, issues and readback results;
//! sidecars hold differences and byte ownership evidence.
//! Reports describe import evidence, not a complete client schema or gameplay.
use super::{diff::{Difference, ExactValue}, ReportKey};
use serde::Serialize;
use std::{fs::{self, File, OpenOptions}, io::{self, Write}, path::{Path, PathBuf}, sync::atomic::{AtomicU64, Ordering}, time::{SystemTime, UNIX_EPOCH}};

#[derive(Clone, Debug, Default, Serialize)]
pub struct RunMetadata { pub run_id: String, pub started_at: String, pub finished_at: Option<String>, pub elapsed_seconds: Option<f64>, pub importer_version: String }
#[derive(Clone, Debug, Default, Serialize)]
pub struct BuildIdentity { pub client_build: Option<String>, pub client_build_source: Option<String>, pub sde_build: Option<String>, pub sde_build_source: Option<String> }
#[derive(Clone, Debug, Serialize)]
pub struct InputIdentity { pub family: String, pub relative_path: String, pub sha256: String, pub byte_length: u64, pub format: String, pub schema_identity: Option<String>, pub build_evidence: Option<String> }
#[derive(Clone, Debug, Default, Serialize)]
pub struct Counts {
    pub input_records: u64, pub snapshot_records: u64, pub output_native_records: u64, pub output_compat_records: u64,
    pub client_only_records: u64, pub sde_only_records: u64, pub common_records: u64, pub changed_records: u64, pub changed_fields: u64,
    pub representation_only_fields: u64, pub missing_required: u64, pub unsupported: u64, pub unknown_regions: u64, pub unknown_bytes: u64,
    pub unowned_bytes: u64, pub reference_errors: u64, pub localization_errors: u64, pub warnings: u64,
}
#[derive(Clone, Debug, Serialize)]
pub struct FieldOwnership { pub path: String, pub authority: String, pub evidence: String, pub consumer_required: bool }
#[derive(Clone, Debug, Serialize)]
pub struct Supplement { pub path: String, pub language: Option<String>, pub message_id: Option<String>, pub source: String, pub absence_evidence: String }
#[derive(Clone, Debug, Default, Serialize)]
pub struct ReportFiles { pub differences: String, pub unknown_regions: String, pub ownership_summary: String, pub warnings: String, pub readback: String }
#[derive(Clone, Debug, Default, Serialize)]
pub struct Validation { pub status: String, pub issue_count: u64 }
#[derive(Clone, Debug, Default, Serialize)]
pub struct Readback { pub status: String, pub compared_native_rows: u64, pub compared_compat_rows: u64, pub compared_typed_rows: u64, pub compared_fields: u64, pub mismatch_count: u64 }
#[derive(Clone, Debug, Serialize)]
pub struct Family {
    pub family: String, pub status: String, pub reason: Option<String>, pub required_families: Vec<String>, pub native_table: Option<String>,
    pub compat_table: Option<String>, pub typed_tables: Vec<String>, pub field_ownership: Vec<FieldOwnership>, pub supplements: Vec<Supplement>,
    pub counts: Counts, pub report_files: ReportFiles, pub validation: Validation, pub readback: Readback,
}
#[derive(Clone, Debug, Default, Serialize)]
pub struct Summary { pub enabled_families: u64, pub not_migrated_families: u64, pub blocked_families: u64, pub error_count: u64, pub warning_count: u64, pub readback_mismatches: u64 }
#[derive(Clone, Debug, Serialize)]
pub struct Manifest { pub schema_version: u32, pub decoder_version: String, pub status: String, pub phase: String, pub run_metadata: RunMetadata, pub build: BuildIdentity, pub inputs: Vec<InputIdentity>, pub families: Vec<Family>, pub summary: Summary }
#[derive(Clone, Debug, Serialize)]
pub struct Issue { pub severity: String, pub code: String, pub family: String, pub key: Option<ReportKey>, pub path: String, pub message: String, pub source_relative_path: String, pub source_sha256: String, pub evidence: serde_json::Value }
#[derive(Clone, Debug, Serialize)]
pub struct UnknownRegion { pub family: String, pub key: Option<ReportKey>, pub owner_keys: Vec<ReportKey>, pub source_relative_path: String, pub source_sha256: String, pub offset: u64, pub length: u64, pub bit_range: Option<(u8,u8)>, pub classification: String, pub reason: String, pub hex: Option<String>, pub sidecar_path: Option<String>, pub sidecar_sha256: Option<String> }
#[derive(Clone, Debug, Serialize)]
pub struct OwnershipSummary { pub family: String, pub source_relative_path: String, pub source_sha256: String, pub source_bytes: u64, pub owned_bytes: u64, pub unknown_bytes: u64, pub unowned_bytes: u64 }
#[derive(Clone, Debug, Serialize)]
pub struct ReadbackDifference { pub view: String, pub table: String, pub key: ReportKey, pub path: String, pub expected_present: bool, pub actual_present: bool, pub expected_value: Option<ExactValue>, pub actual_value: Option<ExactValue> }
#[derive(Clone, Debug, Default)]
pub struct Reports { pub differences: Vec<Difference>, pub issues: Vec<Issue>, pub unknown_regions: Vec<UnknownRegion>, pub ownership_summary: Vec<OwnershipSummary>, pub readback_differences: Vec<ReadbackDifference> }

static NEXT_RUN: AtomicU64 = AtomicU64::new(0);
/// Generate the identity that the caller announces before starting the invocation.
pub fn new_run_id() -> String {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
    format!("run-{now}-{}-{}", std::process::id(), NEXT_RUN.fetch_add(1, Ordering::Relaxed))
}
fn sync_dir(path: &Path) -> io::Result<()> { File::open(path)?.sync_all() }
fn write_synced(path: &Path, bytes: &[u8]) -> io::Result<()> { let mut f = OpenOptions::new().write(true).create_new(true).open(path)?; f.write_all(bytes)?; f.sync_all() }
fn json<T: Serialize>(v: &T) -> io::Result<Vec<u8>> { serde_json::to_vec(v).map_err(io::Error::other) }
fn jsonl<T: Serialize>(values: &[T]) -> io::Result<Vec<u8>> { let mut out = Vec::new(); for v in values { serde_json::to_writer(&mut out, v).map_err(io::Error::other)?; out.push(b'\n'); } Ok(out) }
fn ordered_manifest(manifest: &Manifest) -> Manifest {
    let mut m = manifest.clone(); m.inputs.sort_by(|a,b| (&a.family,&a.relative_path).cmp(&(&b.family,&b.relative_path))); m.families.sort_by(|a,b| a.family.cmp(&b.family));
    for f in &mut m.families { f.required_families.sort(); f.typed_tables.sort(); f.field_ownership.sort_by(|a,b| a.path.cmp(&b.path)); f.supplements.sort_by(|a,b| (&a.path,&a.language,&a.message_id).cmp(&(&b.path,&b.language,&b.message_id))); } m
}
/// Owns a fresh directory and publishes a non-success marker before staging it.
pub struct ReportRun { root: PathBuf, directory: PathBuf, run_name: String, finalized: bool }
impl ReportRun {
    pub fn begin(report_dir: &Path, preflight: &Manifest) -> io::Result<Self> {
        let run_name = &preflight.run_metadata.run_id;
        if run_name.is_empty() || !run_name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid report run identity; database must not connect"));
        }
        fs::create_dir_all(report_dir).map_err(|e| io::Error::new(e.kind(), format!("report start failed; database must not connect; any previous manifest is historical, not current evidence: {e}")))?;
        let run = Self { root: report_dir.to_owned(), directory: report_dir.join(run_name), run_name: run_name.clone(), finalized: false };
        let marker = format!(r#"{{"status":"preflight-failed","phase":"preflight","run_metadata":{{"run_id":"{run_name}"}}}}"#);
        run.publish_manifest(marker.as_bytes(), "start").map_err(|e| io::Error::new(e.kind(), format!("report start marker publication failed; database must not connect; any previous manifest is historical, not current evidence; marker durability may be indeterminate: {e}")))?;
        if preflight.status == "success" {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "preflight cannot publish success"));
        }
        let data = json(&ordered_manifest(preflight))?;
        run.publish_manifest(&data, "preflight")?;
        fs::create_dir(&run.directory)?;
        sync_dir(&run.root)?;
        write_synced(&run.directory.join("preflight-manifest.json"), &data)?;
        sync_dir(&run.directory)?;
        Ok(run)
    }
    pub fn relative_path(&self, filename: &str) -> String { format!("{}/{filename}", self.run_name) }
    /// Run-private directory for files staged through this report writer.
    pub fn directory(&self) -> &Path { &self.directory }
    /// Durably create a run-local sidecar without allowing paths or replacement.
    pub fn write_sidecar(&self, name: &str, bytes: &[u8]) -> io::Result<()> {
        if self.finalized {
            return Err(io::Error::new(io::ErrorKind::AlreadyExists, "report finalization already attempted"));
        }
        if name.is_empty() || name == "." || name == ".." || name.bytes().any(|b| matches!(b, b'/' | b'\\' | 0)) || matches!(name, "preflight-manifest.json" | "differences.jsonl" | "issues.jsonl" | "unknown-regions.jsonl" | "ownership-summary.json" | "readback-differences.jsonl") {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "sidecar name must be a single filename"));
        }
        write_synced(&self.directory.join(name), bytes)?;
        sync_dir(&self.directory)
    }
    /// Durably refresh input identities and preflight evidence before database connection.
    pub fn update_preflight(&mut self, manifest: &Manifest) -> io::Result<()> {
        if manifest.run_metadata.run_id != self.run_name {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "report run identity changed"));
        }
        if self.finalized {
            return Err(io::Error::new(io::ErrorKind::AlreadyExists, "report finalization already attempted"));
        }
        if manifest.status == "success" {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "preflight cannot publish success"));
        }
        let data = json(&ordered_manifest(manifest))?;
        let temp = self.directory.join(format!(".preflight-{}.tmp", NEXT_RUN.fetch_add(1, Ordering::Relaxed)));
        write_synced(&temp, &data)?;
        fs::rename(temp, self.directory.join("preflight-manifest.json"))?;
        sync_dir(&self.directory)?;
        self.publish_manifest(&data, "preflight-update")
    }
    fn publish_manifest(&self, data: &[u8], suffix: &str) -> io::Result<()> {
        let temp = self.root.join(format!(".{}-{suffix}-{}.tmp", self.run_name, NEXT_RUN.fetch_add(1, Ordering::Relaxed))); write_synced(&temp, data)?;
        fs::rename(temp, self.root.join("manifest.json"))?; sync_dir(&self.root)
    }
    /// Reports are durable before the manifest is atomically replaced. On failure the preflight remains.
    pub fn finalize(&mut self, manifest: &Manifest, reports: &Reports) -> io::Result<()> {
        if self.finalized {
            return Err(io::Error::new(io::ErrorKind::AlreadyExists, "report finalization already attempted"));
        }
        self.finalized = true;
        if manifest.run_metadata.run_id != self.run_name {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "report run identity changed"));
        }
        if manifest.status == "success" && (manifest.families.iter().any(|f| f.status == "blocked" || (f.status == "enabled" && (f.readback.status != "passed" || f.readback.mismatch_count != 0))) || manifest.summary.error_count != 0 || manifest.summary.readback_mismatches != 0 || reports.issues.iter().any(|i| i.severity == "error") || !reports.readback_differences.is_empty()) {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "success requires no errors, blocked families, or readback mismatches"));
        }
        let mut r = reports.clone();
        r.differences.sort_by(|a,b| (&a.family,&a.key,&a.path,&a.kind).cmp(&(&b.family,&b.key,&b.path,&b.kind)));
        r.issues.sort_by(|a,b| (&a.family,&a.key,&a.path,&a.code).cmp(&(&b.family,&b.key,&b.path,&b.code)));
        r.unknown_regions.sort_by(|a,b| (&a.family,&a.key,a.offset,&a.classification).cmp(&(&b.family,&b.key,b.offset,&b.classification)));
        for region in &mut r.unknown_regions { region.owner_keys.sort(); }
        r.ownership_summary.sort_by(|a,b| (&a.family,&a.source_relative_path).cmp(&(&b.family,&b.source_relative_path)));
        r.readback_differences.sort_by(|a,b| (&a.view,&a.table,&a.key,&a.path).cmp(&(&b.view,&b.table,&b.key,&b.path)));
        for (name, data) in [("differences.jsonl", jsonl(&r.differences)?), ("issues.jsonl", jsonl(&r.issues)?), ("unknown-regions.jsonl", jsonl(&r.unknown_regions)?), ("ownership-summary.json", json(&r.ownership_summary)?), ("readback-differences.jsonl", jsonl(&r.readback_differences)?)] {
            write_synced(&self.directory.join(name), &data)?;
        }
        sync_dir(&self.directory)?; self.publish_manifest(&json(&ordered_manifest(manifest))?, "final")
    }
}
#[cfg(test)] mod tests;
