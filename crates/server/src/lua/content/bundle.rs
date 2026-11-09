//! Immutable, bounded adaptation bundle loaded once for a Lua generation.
//! The publisher's manifest and dataset envelopes are retained verbatim; no
//! node, mission or effect gameplay is implemented by this loader.

use std::{collections::{BTreeMap, BTreeSet}, fs, io::Read, path::{Component, Path}};
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use crate::hash::sha256_hex;

pub(super) const FAMILIES: [&str; 7] = ["shipadaptionnodes", "shipadaptiontrees", "shipadaptionnodetypes", "shipadaptionnodeunlocktypes", "shipadaptionconfig", "shipadaptionchips", "shipadaptionchipexchangeconfig"];

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    schema: String, schema_version: u32, client_build: String,
    source_manifest_sha256: String, constants_sha256: String,
    chip_types: Vec<i32>, datasets: BTreeMap<String, Dataset>,
    provenance: Provenance, validation: Validation,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Dataset { path: String, records: usize, sha256: String }
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Provenance { source_validation: Value, chip_type_symbols: Vec<String> }
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Validation { scope: String, node_task_effect_gameplay_enabled: bool }
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope { client_build: String, records: Vec<Record>, schema: String, schema_version: u32, source_resource: String }
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Record { pub key: String, pub data: Value }

/// Serialized into the retained package, independent of the deployment root.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::lua) struct Bundle {
    pub manifest: Vec<u8>,
    pub files: BTreeMap<String, Vec<u8>>,
    #[serde(skip)]
    pub(super) parsed: BTreeMap<String, Vec<Record>>,
    #[serde(skip)]
    pub(super) chip_types: Vec<i32>,
}

fn digest(s: &str) -> bool { s.len() == 64 && s.bytes().all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)) }
fn ordinary(s: &str) -> bool {
    !s.is_empty() && !s.contains('/') && !s.contains('\\') && matches!(Path::new(s).components().next(), Some(Component::Normal(_))) && Path::new(s).components().count() == 1
}
fn regular(root: &Path, relative: &Path, limit: usize) -> Result<Vec<u8>> {
    ensure!(relative.components().all(|c| matches!(c, Component::Normal(_))), "invalid bundle path");
    let path = root.join(relative);
    ensure!(fs::symlink_metadata(&path)?.file_type().is_file(), "bundle member is not a regular file");
    let resolved = path.canonicalize()?;
    ensure!(resolved.starts_with(root), "bundle member escapes root");
    let mut result = Vec::new();
    fs::File::open(resolved)?.take(u64::try_from(limit)?.checked_add(1).context("invalid file limit")?).read_to_end(&mut result)?;
    ensure!(result.len() <= limit, "bundle member exceeds size limit");
    Ok(result)
}
fn json(bytes: &[u8], name: &str) -> Result<Value> {
    let value: Value = serde_json::from_slice(bytes)?;
    super::super::json::validate(&value, name, 0)?;
    Ok(value)
}
impl Bundle {
    pub(in crate::lua) fn load(root: &Path, limit: usize) -> Result<Self> {
        let root = root.canonicalize().context("canonicalizing adaptation root")?;
        ensure!(fs::metadata(&root)?.is_dir(), "adaptation root is not a directory");
        let pointer = regular(&root, Path::new("current"), limit)?;
        let pointer = std::str::from_utf8(&pointer)?.strip_suffix('\n').context("invalid current pointer")?;
        let (build, digest) = pointer.split_once('/').context("invalid current pointer")?;
        ensure!(ordinary(build) && self::digest(digest), "invalid current pointer");
        let directory = root.join(build).join(digest);
        let resolved = directory.canonicalize()?;
        ensure!(resolved.starts_with(&root) && resolved.is_dir(), "bundle directory escapes root");
        let manifest = regular(&resolved, Path::new("manifest.json"), limit)?;
        ensure!(sha256_hex(&manifest) == digest, "manifest digest mismatch");
        let parsed: Manifest = serde_json::from_value(json(&manifest, "manifest")?)?;
        ensure!(parsed.client_build == build, "manifest build mismatch");
        let mut files = BTreeMap::new();
        for family in FAMILIES {
            let entry = parsed.datasets.get(family).with_context(|| format!("missing dataset {family}"))?;
            ensure!(entry.path == format!("datasets/{family}.json"), "invalid dataset path");
            files.insert(family.to_owned(), regular(&resolved, Path::new(&entry.path), limit)?);
        }
        let mut bundle = Self { manifest, files, parsed: BTreeMap::new(), chip_types: Vec::new() };
        bundle.validate(limit)?;
        Ok(bundle)
    }
    pub(super) fn validate(&mut self, limit: usize) -> Result<()> {
        self.parsed.clear();
        self.chip_types.clear();
        ensure!(self.manifest.len() <= limit, "manifest exceeds size limit");
        let manifest: Manifest = serde_json::from_value(json(&self.manifest, "manifest")?)?;
        ensure!(manifest.schema == "infinity-adaption-content" && manifest.schema_version == 1 && ordinary(&manifest.client_build), "invalid manifest identity");
        ensure!(digest(&manifest.source_manifest_sha256) && digest(&manifest.constants_sha256), "invalid provenance digest");
        ensure!(!manifest.chip_types.is_empty() && manifest.chip_types.iter().all(|id| *id > 0) && manifest.chip_types.windows(2).all(|w| w[0] < w[1]), "invalid chip types");
        ensure!(!manifest.provenance.chip_type_symbols.is_empty() && manifest.provenance.source_validation.is_object(), "invalid provenance");
        ensure!(manifest.validation.scope == "chip-ledger" && !manifest.validation.node_task_effect_gameplay_enabled, "invalid validation scope");
        ensure!(manifest.datasets.len() == FAMILIES.len() && self.files.len() == FAMILIES.len(), "unexpected dataset count");
        let mut parsed = BTreeMap::new();
        for family in FAMILIES {
            let item = manifest.datasets.get(family).context("missing dataset descriptor")?;
            let bytes = self.files.get(family).context("missing dataset")?;
            ensure!(bytes.len() <= limit && item.path == format!("datasets/{family}.json") && digest(&item.sha256) && sha256_hex(bytes) == item.sha256, "invalid dataset identity: {family}");
            let envelope: Envelope = serde_json::from_value(json(bytes, family)?)?;
            ensure!(envelope.schema == "cfsd-adaptation-dataset" && envelope.schema_version == 1 && envelope.client_build == manifest.client_build && !envelope.source_resource.is_empty(), "invalid dataset envelope: {family}");
            ensure!(!envelope.records.is_empty() && envelope.records.len() == item.records, "invalid dataset count: {family}");
            let singleton = family == "shipadaptionconfig" || family == "shipadaptionchipexchangeconfig";
            if singleton { ensure!(envelope.records.len() == 1, "invalid singleton: {family}"); }
            let mut keys = BTreeSet::new();
            for record in &envelope.records {
                ensure!(record.data.is_object(), "invalid record: {family}");
                if singleton { ensure!(record.key.is_empty(), "invalid singleton key"); continue; }
                let key: i64 = record.key.parse()?;
                let zero = family == "shipadaptionnodetypes" || family == "shipadaptionnodeunlocktypes";
                ensure!((key > 0 || zero && key == 0) && record.key == key.to_string() && keys.insert(key), "invalid record key: {family}");
                ensure!(record.data.get("_key").and_then(Value::as_i64) == Some(key), "record key mismatch: {family}");
            }
            parsed.insert(family.to_owned(), envelope.records);
        }
        self.chip_types = manifest.chip_types;
        self.parsed = parsed;
        Ok(())
    }
    pub(super) fn manifest_hash(&self) -> String { sha256_hex(&self.manifest) }
    pub(super) fn chip_types(&self) -> &[i32] { &self.chip_types }
    pub(super) fn records(&self, family: &str) -> Result<&[Record]> {
        self.parsed.get(family).map(Vec::as_slice).with_context(|| format!("unknown or unvalidated dataset {family}"))
    }
}

#[cfg(test)]
mod tests;
