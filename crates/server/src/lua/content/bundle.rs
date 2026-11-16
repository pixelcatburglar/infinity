//! Immutable, bounded content bundle loaded once for a Lua generation.
//! The publisher's manifest and dataset envelopes are retained verbatim; the
//! original v1 `infinity-adaption-content` package keeps decoding exactly, so
//! an unrepublished deployment root and retained package are byte-identical.
//! Each declared dataset family requires its registered content consumer.
//! No node, mission or effect gameplay is implemented by this loader.

use std::{collections::{BTreeMap, BTreeSet}, fs, io::Read, path::{Component, Path}};
use anyhow::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use crate::hash::sha256_hex;

pub(super) const FAMILIES: [&str; 7] = ["shipadaptionnodes", "shipadaptiontrees", "shipadaptionnodetypes", "shipadaptionnodeunlocktypes", "shipadaptionconfig", "shipadaptionchips", "shipadaptionchipexchangeconfig"];
/// The sovereignty development-indices dataset family name.
pub(super) const INDICES_FAMILY: &str = "sovereigntyindices";
/// The dataset format declares exactly six index level bands.
const LEVEL_COUNT: usize = 6;

/// Whether `name` is any dataset family this loader understands.
pub(super) fn known_family(name: &str) -> bool { name == INDICES_FAMILY || FAMILIES.contains(&name) }

/// Original package format, decoded exactly as published.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestV1 {
    schema: String, schema_version: u32, client_build: String,
    source_manifest_sha256: String, constants_sha256: String,
    chip_types: Vec<i32>, datasets: BTreeMap<String, DatasetV1>,
    provenance: Provenance, validation: Validation,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DatasetV1 { path: String, records: usize, sha256: String }

/// Current package format: an optional adaptation domain plus named datasets.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestV2 {
    schema: String, schema_version: u32, client_build: String,
    sources: BTreeMap<String, SourceRef>, datasets: BTreeMap<String, DatasetV2>,
    adaption: Option<Adaption>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceRef { sha256: String, bytes: u64 }
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DatasetV2 { path: String, records: u64, sha256: String }
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Adaption {
    source_manifest_sha256: String, constants_sha256: String,
    chip_types: Vec<i32>, provenance: Provenance, validation: Validation,
}
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

#[derive(Deserialize)]
#[serde(untagged)]
enum Parsed { V1(ManifestV1), V2(ManifestV2) }

struct AdaptionRef<'a> {
    source_manifest_sha256: &'a str, constants_sha256: &'a str,
    chip_types: &'a [i32], provenance: &'a Provenance, validation: &'a Validation,
}
struct DatasetItem<'a> { family: &'a str, path: &'a str, records: u64, sha256: &'a str }
struct SourceItem<'a> { name: &'a str, sha256: &'a str, bytes: u64 }

/// The format-neutral view both manifest versions are validated through.
struct Checked<'a> {
    version: u32,
    client_build: &'a str,
    adaption: Option<AdaptionRef<'a>>,
    datasets: Vec<DatasetItem<'a>>,
    sources: Vec<SourceItem<'a>>,
}

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
fn parse_manifest(manifest: &[u8]) -> Result<Parsed> {
    let value = json(manifest, "manifest")?;
    let parsed = Parsed::deserialize(&value)?;
    match &parsed {
        Parsed::V1(m) if m.schema == "infinity-adaption-content" && m.schema_version == 1 => {}
        Parsed::V2(m) if m.schema == "infinity-content" && m.schema_version == 2 => {}
        _ => bail!("invalid manifest identity"),
    }
    Ok(parsed)
}

fn checked(parsed: &Parsed) -> Checked<'_> {
    match parsed {
        Parsed::V1(m) => Checked {
            version: 1,
            client_build: &m.client_build,
            adaption: Some(AdaptionRef {
                source_manifest_sha256: &m.source_manifest_sha256,
                constants_sha256: &m.constants_sha256,
                chip_types: &m.chip_types, provenance: &m.provenance, validation: &m.validation,
            }),
            datasets: m.datasets.iter().map(|(family, item)| DatasetItem {
                family, path: &item.path, records: item.records as u64, sha256: &item.sha256,
            }).collect(),
            sources: Vec::new(),
        },
        Parsed::V2(m) => Checked {
            version: 2,
            client_build: &m.client_build,
            adaption: m.adaption.as_ref().map(|a| AdaptionRef {
                source_manifest_sha256: &a.source_manifest_sha256,
                constants_sha256: &a.constants_sha256,
                chip_types: &a.chip_types, provenance: &a.provenance, validation: &a.validation,
            }),
            datasets: m.datasets.iter().map(|(family, item)| DatasetItem {
                family, path: &item.path, records: item.records, sha256: &item.sha256,
            }).collect(),
            sources: m.sources.iter().map(|(name, item)| SourceItem { name, sha256: &item.sha256, bytes: item.bytes }).collect(),
        },
    }
}

fn digest(s: &str) -> bool { s.len() == 64 && s.bytes().all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)) }
fn ordinary(s: &str) -> bool {
    !s.is_empty() && !s.contains('/') && !s.contains('\\') && matches!(Path::new(s).components().next(), Some(Component::Normal(_))) && Path::new(s).components().count() == 1
}
/// A logical relative source name: normal path components, possibly nested.
fn logical(s: &str) -> bool {
    let path = Path::new(s);
    !s.is_empty() && !s.contains('\\') && path.components().all(|c| matches!(c, Component::Normal(_)))
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

fn integer(value: &Value) -> Result<i64> { value.as_i64().context("expected an integer") }
fn number(value: &Value) -> Result<f64> { value.as_f64().filter(|v| v.is_finite()).context("expected a finite number") }
fn members<'a>(value: &'a Value, keys: &[&str]) -> Result<&'a Map<String, Value>> {
    let object = value.as_object().context("expected an object")?;
    ensure!(object.len() == keys.len() && keys.iter().all(|k| object.contains_key(*k)), "unexpected object fields");
    Ok(object)
}

/// Validates the indices dataset singleton: unique attribute ids, six
/// contiguous increasing level bands, five increasing strategic thresholds
/// and bounded occupancy tables. Values stay as recorded; nothing is zeroed.
fn indices(data: &Value) -> Result<()> {
    let root = members(data, &["attribute_ids", "industrial_levels", "military_levels", "occupancy", "strategic_seconds"])?;
    let ids = members(&root["attribute_ids"], &["military", "industrial", "strategic"])?;
    let (military, industrial, strategic) = (integer(&ids["military"])?, integer(&ids["industrial"])?, integer(&ids["strategic"])?);
    ensure!(military > 0 && industrial > 0 && strategic > 0 && military != industrial && military != strategic && industrial != strategic, "invalid attribute ids");
    for axis in ["military_levels", "industrial_levels"] {
        let bands = root[axis].as_array().context("expected a band array")?;
        ensure!(bands.len() == LEVEL_COUNT, "invalid level band count: {axis}");
        let mut previous = None;
        for band in bands {
            let band = members(band, &["level", "max_points", "min_points", "modifier"])?;
            let (level, min, max, modifier) = (integer(&band["level"])?, number(&band["min_points"])?, number(&band["max_points"])?, number(&band["modifier"])?);
            ensure!(previous.is_none_or(|(p, _)| level == p + 1) && min >= 0.0 && min < max && modifier > 0.0, "invalid level band: {axis}");
            ensure!(previous.is_some() || (level == 0 && min == 0.0), "first band must start at zero: {axis}");
            if let Some((_, previous_max)) = previous { ensure!(min == previous_max, "level bands must be contiguous: {axis}"); }
            previous = Some((level, max));
        }
    }
    let seconds = root["strategic_seconds"].as_array().context("expected a threshold array")?;
    ensure!(seconds.len() + 1 == LEVEL_COUNT, "invalid strategic threshold count");
    let mut previous = 0i64;
    for value in seconds {
        let threshold = integer(value)?;
        ensure!(threshold > previous, "strategic thresholds must increase");
        previous = threshold;
    }
    let occupancy = members(&root["occupancy"], &["base", "capital", "industrial", "max", "military", "min", "strategic"])?;
    let (base, capital, min, max) = (number(&occupancy["base"])?, number(&occupancy["capital"])?, number(&occupancy["min"])?, number(&occupancy["max"])?);
    ensure!(base > 0.0 && capital > 0.0 && min > 0.0 && min < max, "invalid occupancy bounds");
    for axis in ["military", "industrial", "strategic"] {
        let entries = occupancy[axis].as_array().context("expected an occupancy array")?;
        ensure!(!entries.is_empty(), "empty occupancy table: {axis}");
        let mut previous = None;
        for entry in entries {
            let entry = members(entry, &["bonus", "level"])?;
            let (level, bonus) = (integer(&entry["level"])?, number(&entry["bonus"])?);
            ensure!((0..LEVEL_COUNT as i64).contains(&level) && bonus >= 0.0, "invalid occupancy entry: {axis}");
            ensure!(previous.is_none_or(|p| level == p + 1), "occupancy levels must be contiguous: {axis}");
            previous = Some(level);
        }
    }
    Ok(())
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
        let parsed = parse_manifest(&manifest)?;
        let declared = checked(&parsed);
        ensure!(declared.client_build == build, "manifest build mismatch");
        let mut files = BTreeMap::new();
        for item in &declared.datasets {
            files.insert(item.family.to_owned(), regular(&resolved, Path::new(item.path), limit)?);
        }
        let mut bundle = Self { manifest, files, parsed: BTreeMap::new(), chip_types: Vec::new() };
        bundle.validate(limit)?;
        Ok(bundle)
    }
    pub(super) fn validate(&mut self, limit: usize) -> Result<()> {
        self.parsed.clear();
        self.chip_types.clear();
        ensure!(self.manifest.len() <= limit, "manifest exceeds size limit");
        let parsed = parse_manifest(&self.manifest)?;
        let manifest = checked(&parsed);
        ensure!(ordinary(manifest.client_build), "invalid manifest identity");
        if let Some(adaption) = &manifest.adaption {
            ensure!(digest(adaption.source_manifest_sha256) && digest(adaption.constants_sha256), "invalid provenance digest");
            ensure!(!adaption.chip_types.is_empty() && adaption.chip_types.iter().all(|id| *id > 0) && adaption.chip_types.windows(2).all(|w| w[0] < w[1]), "invalid chip types");
            ensure!(!adaption.provenance.chip_type_symbols.is_empty() && adaption.provenance.source_validation.is_object(), "invalid provenance");
            ensure!(adaption.validation.scope == "chip-ledger" && !adaption.validation.node_task_effect_gameplay_enabled, "invalid validation scope");
        }
        for source in &manifest.sources {
            ensure!(logical(source.name) && digest(source.sha256) && source.bytes > 0, "invalid source identity");
        }
        ensure!(!manifest.datasets.is_empty() && manifest.datasets.len() == self.files.len(), "unexpected dataset count");
        ensure!(manifest.datasets.iter().all(|item| known_family(item.family)), "unknown dataset family");
        let adaption_count = manifest.datasets.iter().filter(|item| FAMILIES.contains(&item.family)).count();
        ensure!(adaption_count == if manifest.adaption.is_some() { FAMILIES.len() } else { 0 }, "incomplete adaptation dataset set");
        ensure!(manifest.version != 1 || manifest.datasets.len() == FAMILIES.len(), "unexpected dataset set for original manifest");
        if manifest.datasets.iter().any(|item| item.family == INDICES_FAMILY) {
            ensure!(!manifest.sources.is_empty(), "missing extraction provenance");
        }
        let mut records = BTreeMap::new();
        for item in &manifest.datasets {
            let family = item.family;
            let bytes = self.files.get(family).context("missing dataset")?;
            ensure!(bytes.len() <= limit && item.path == format!("datasets/{family}.json") && digest(item.sha256) && sha256_hex(bytes) == item.sha256, "invalid dataset identity: {family}");
            let envelope: Envelope = serde_json::from_value(json(bytes, family)?)?;
            ensure!(envelope.client_build == manifest.client_build && !envelope.source_resource.is_empty(), "invalid dataset envelope: {family}");
            ensure!(u64::try_from(envelope.records.len())? == item.records, "invalid dataset count: {family}");
            if family == INDICES_FAMILY {
                ensure!(envelope.schema == "infinity-content-dataset" && envelope.schema_version == 1, "invalid dataset envelope: {family}");
                ensure!(envelope.records.len() == 1 && envelope.records[0].key.is_empty(), "invalid dataset records: {family}");
                indices(&envelope.records[0].data)?;
            } else {
                ensure!(envelope.schema == "cfsd-adaptation-dataset" && envelope.schema_version == 1, "invalid dataset envelope: {family}");
                ensure!(!envelope.records.is_empty(), "invalid dataset records: {family}");
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
            }
            records.insert(family.to_owned(), envelope.records);
        }
        self.chip_types = manifest.adaption.map(|a| a.chip_types.to_vec()).unwrap_or_default();
        self.parsed = records;
        Ok(())
    }
    pub(super) fn manifest_hash(&self) -> String { sha256_hex(&self.manifest) }
    pub(super) fn chip_types(&self) -> &[i32] { &self.chip_types }
    /// Whether the validated bundle contains the named known dataset family.
    /// A family declared but corrupt cannot reach this: the bundle fails first.
    pub(super) fn has_family(&self, name: &str) -> bool { known_family(name) && self.parsed.contains_key(name) }
    /// Registered namespaces required by the validated dataset families.
    pub(in crate::lua) fn required_consumers(&self) -> impl Iterator<Item = (&'static str, &'static str)> + '_ {
        [
            (FAMILIES[0], "ship_adaption", "configured adaptation bundle has no registered consumer"),
            (INDICES_FAMILY, super::indices::NAMESPACE, "configured indices bundle has no registered consumer"),
        ].into_iter().filter(|(family, _, _)| self.has_family(family)).map(|(_, namespace, error)| (namespace, error))
    }
    pub(super) fn records(&self, family: &str) -> Result<&[Record]> {
        self.parsed.get(family).map(Vec::as_slice).with_context(|| format!("unknown or unvalidated dataset {family}"))
    }
}

#[cfg(all(test, feature = "tests-lua"))]
mod tests;
