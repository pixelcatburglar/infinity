//! Validation of the decoded JSON export and matching client constants. Native
//! resource decoding, SDE reference closure and gameplay semantics are outside
//! this publisher's scope.

use anyhow::{bail, ensure, Context, Result};
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{collections::{BTreeMap, BTreeSet}, fs, io::Read, path::{Component, Path, PathBuf}};

/// The source export's maximum JSON or constants file size, in bytes.
pub(super) const MAX_BYTES: u64 = 4 * 1024 * 1024;
/// Maximum nested JSON container depth accepted by the publisher.
const MAX_DEPTH: usize = 64;
pub(super) const FAMILIES: [&str; 7] = ["shipadaptionnodes", "shipadaptiontrees", "shipadaptionnodetypes", "shipadaptionnodeunlocktypes", "shipadaptionconfig", "shipadaptionchips", "shipadaptionchipexchangeconfig"];

#[derive(Deserialize)]
struct SourceManifest {
    schema: String,
    schema_version: u32,
    client_build: String,
    datasets: BTreeMap<String, Dataset>,
    validation: Value,
}
#[derive(Deserialize)]
struct Dataset { path: String, records: usize, sha256: String }
#[derive(Deserialize)]
struct Envelope { client_build: String, records: Vec<Record>, schema: String, schema_version: u32, source_resource: String }
#[derive(Deserialize)]
struct Record { key: String, data: Value }

pub(super) struct Bundle {
    pub build: String,
    pub source_hash: String,
    pub validation: Value,
    pub files: BTreeMap<String, (usize, Vec<u8>)>,
}

pub(super) fn hash(bytes: &[u8]) -> String { sde_import::encoding::hex(&Sha256::digest(bytes)) }

pub(super) fn read_regular(path: &Path) -> Result<Vec<u8>> {
    let meta = fs::symlink_metadata(path).with_context(|| format!("reading {}", path.display()))?;
    ensure!(meta.file_type().is_file(), "not a regular file: {}", path.display());
    ensure!(meta.len() <= MAX_BYTES, "file exceeds size limit: {}", path.display());
    let mut bytes = Vec::new();
    fs::File::open(path)?.take(MAX_BYTES + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() as u64 <= MAX_BYTES, "file exceeds size limit: {}", path.display());
    Ok(bytes)
}

fn json(bytes: &[u8]) -> Result<Value> {
    let value: Value = serde_json::from_slice(bytes)?;
    fn check(value: &Value, depth: usize) -> Result<()> {
        ensure!(depth <= MAX_DEPTH, "JSON nesting exceeds limit");
        match value {
            Value::Null => bail!("null is not permitted"),
            Value::Array(a) => for v in a { check(v, depth + 1)?; },
            Value::Object(o) => for v in o.values() { check(v, depth + 1)?; },
            _ => (),
        }
        Ok(())
    }
    check(&value, 0)?;
    Ok(value)
}

fn safe_member(root: &Path, name: &str) -> Result<PathBuf> {
    let path = Path::new(name);
    ensure!(!name.is_empty() && path.components().all(|c| matches!(c, Component::Normal(_))), "invalid dataset path: {name}");
    let joined = root.join(path);
    let resolved = fs::canonicalize(&joined)?;
    ensure!(resolved.starts_with(root), "dataset escapes input root: {name}");
    // Reject a symlink as the final member even when its target remains inside.
    ensure!(fs::symlink_metadata(&joined)?.file_type().is_file(), "dataset is not a regular file: {name}");
    Ok(joined)
}

pub(super) fn load(root: &Path) -> Result<Bundle> {
    let root = fs::canonicalize(root)?;
    let bytes = read_regular(&safe_member(&root, "manifest.json")?)?;
    let manifest: SourceManifest = serde_json::from_value(json(&bytes)?)?;
    ensure!(manifest.schema == "cfsd-adaptation-bundle" && manifest.schema_version == 1, "unsupported source manifest schema");
    ensure!(!manifest.client_build.is_empty() && Path::new(&manifest.client_build).components().count() == 1 && matches!(Path::new(&manifest.client_build).components().next(), Some(Component::Normal(_))) && !manifest.client_build.contains(|c| c == '/' || c == '\\'), "invalid client build");
    let missing: Vec<_> = FAMILIES.iter().filter(|f| !manifest.datasets.contains_key(**f)).copied().collect();
    ensure!(missing.is_empty(), "missing datasets: {}", missing.join(", "));
    ensure!(manifest.datasets.len() == FAMILIES.len(), "unexpected datasets in source manifest");
    let mut files = BTreeMap::new();
    for family in FAMILIES {
        let item = &manifest.datasets[family];
        ensure!(item.path == format!("datasets/{family}.json"), "invalid dataset path: {family}");
        let data = read_regular(&safe_member(&root, &item.path)?)?;
        ensure!(hash(&data) == item.sha256, "dataset hash mismatch: {family}");
        let envelope: Envelope = serde_json::from_value(json(&data)?)?;
        ensure!(envelope.client_build == manifest.client_build, "dataset build mismatch: {family}");
        ensure!(envelope.schema == "cfsd-adaptation-dataset" && envelope.schema_version == 1 && !envelope.source_resource.is_empty(), "invalid dataset envelope: {family}");
        ensure!(item.records > 0, "empty dataset: {family}");
        ensure!(envelope.records.len() == item.records, "dataset count mismatch: {family}");
        let singleton = family == "shipadaptionconfig" || family == "shipadaptionchipexchangeconfig";
        if singleton { ensure!(envelope.records.len() == 1, "invalid singleton count: {family}"); }
        let mut keys = BTreeSet::new();
        for record in &envelope.records {
            ensure!(record.data.is_object(), "record data is not an object: {family}");
            if singleton {
                ensure!(record.key.is_empty(), "invalid singleton key: {family}");
            } else {
                let key: i64 = record.key.parse().context("record key is not an integer")?;
                let allows_zero = family == "shipadaptionnodetypes" || family == "shipadaptionnodeunlocktypes";
                ensure!((key > 0 || (allows_zero && key == 0)) && record.key == key.to_string() && keys.insert(key), "invalid or duplicate record key: {family}");
                ensure!(record.data.get("_key").and_then(Value::as_i64) == Some(key), "record key mismatch: {family}");
            }
        }
        files.insert(family.to_owned(), (item.records, data));
    }
    Ok(Bundle { build: manifest.client_build, source_hash: hash(&bytes), validation: manifest.validation, files })
}

fn identifier(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_') && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

pub(super) fn constants(bytes: &[u8]) -> Result<(Vec<i32>, Vec<String>)> {
    let text = std::str::from_utf8(bytes)?;
    let mut assignments: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut tuple = None;
    let mut collecting = false;
    let mut body = String::new();
    for line in text.lines() {
        let indented = line.starts_with(char::is_whitespace);
        let line = line.split('#').next().unwrap_or("").trim();
        if collecting {
            body.push(' '); body.push_str(line);
            if line.contains(')') { collecting = false; tuple = Some(body.clone()); }
            continue;
        }
        if indented { continue; }
        if let Some((name, rhs)) = line.split_once('=') {
            let name = name.trim(); let rhs = rhs.trim();
            if name == "TypeAdaptionChips" {
                ensure!(tuple.is_none(), "duplicate TypeAdaptionChips assignment");
                body = rhs.to_owned();
                if rhs.contains(')') { tuple = Some(body.clone()); } else { collecting = true; }
            } else if identifier(name) {
                assignments.entry(name.to_owned()).or_default().push(rhs.to_owned());
            }
        }
    }
    ensure!(!collecting, "unterminated TypeAdaptionChips tuple");
    let tuple = tuple.context("missing TypeAdaptionChips assignment")?;
    let inner = tuple.strip_prefix('(').and_then(|s| s.strip_suffix(')')).context("TypeAdaptionChips must be a tuple")?;
    ensure!(inner.contains(','), "TypeAdaptionChips must be a tuple");
    let mut symbols = Vec::new(); let mut ids = BTreeSet::new(); let mut seen = BTreeSet::new();
    let parts: Vec<_> = inner.split(',').collect();
    for (index, part) in parts.iter().enumerate() {
        let name = part.trim();
        if name.is_empty() && index + 1 == parts.len() { continue; }
        ensure!(identifier(name) && seen.insert(name.to_owned()), "invalid or duplicate tuple symbol: {name}");
        let values = assignments.get(name).with_context(|| format!("missing constant {name}"))?;
        ensure!(values.len() == 1, "duplicate constant assignment: {name}");
        let value: i32 = values[0].parse().with_context(|| format!("constant {name} is not a literal i32"))?;
        ensure!(value > 0 && ids.insert(value), "nonpositive or duplicate constant: {name}");
        symbols.push(name.to_owned());
    }
    ensure!(!symbols.is_empty(), "empty TypeAdaptionChips tuple");
    Ok((ids.into_iter().collect(), symbols))
}
