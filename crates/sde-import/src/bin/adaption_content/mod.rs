//! Publishes a validated decoded adaptation JSON bundle and/or an extracted
//! sovereignty development-index dataset, and optionally switches the current
//! pointer. Native decoding, runtime SDE reference closure, and node, task and
//! effect gameplay semantics are not provided by this tool. Adaptation input
//! constants come from the matching client's `shipAdaptionSystem/common/const.py`;
//! the indices dataset is parsed from the matching client's `devIndexUtil.py`,
//! `occupancyCalculator.py` and `dogma/const.py` under a fixed literal whitelist.
//! The operator must supply inputs from the same client build; the hash records
//! provenance but cannot establish that match. The external output root must
//! already exist and is canonicalized before publication.

mod input;
mod sovereignty;

use anyhow::{bail, ensure, Context, Result};
use serde::Serialize;
use serde_json::Value;
use std::{collections::BTreeMap, fs, io::Write, path::{Path, PathBuf}, sync::atomic::{AtomicU64, Ordering}};

#[derive(Serialize)]
struct Source { sha256: String, bytes: u64 }
#[derive(Serialize)]
struct Dataset { path: String, records: u64, sha256: String }
#[derive(Serialize)]
struct Provenance { source_validation: Value, chip_type_symbols: Vec<String> }
#[derive(Serialize)]
struct Validation { scope: &'static str, node_task_effect_gameplay_enabled: bool }
#[derive(Serialize)]
struct Adaption { source_manifest_sha256: String, constants_sha256: String, chip_types: Vec<i32>, provenance: Provenance, validation: Validation }
#[derive(Serialize)]
struct Manifest {
    schema: &'static str,
    schema_version: u32,
    client_build: String,
    sources: BTreeMap<String, Source>,
    datasets: BTreeMap<String, Dataset>,
    #[serde(skip_serializing_if = "Option::is_none")]
    adaption: Option<Adaption>,
}

static SERIAL: AtomicU64 = AtomicU64::new(0);
struct Staging(PathBuf);
impl Drop for Staging { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); } }

fn stage(root: &Path) -> Result<Staging> {
    loop {
        let name = format!(".adaption-stage-{}-{}", std::process::id(), SERIAL.fetch_add(1, Ordering::Relaxed));
        let path = root.join(name);
        match fs::create_dir(&path) {
            Ok(()) => return Ok(Staging(path)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.into()),
        }
    }
}

fn regular_dir(path: &Path) -> Result<()> {
    ensure!(fs::symlink_metadata(path)?.file_type().is_dir(), "not a regular directory: {}", path.display());
    Ok(())
}

fn verify_existing(path: &Path, manifest: &[u8], files: &BTreeMap<String, Vec<u8>>) -> Result<()> {
    regular_dir(path)?;
    regular_dir(&path.join("datasets"))?;
    ensure!(input::read_regular(&path.join("manifest.json"))? == manifest, "existing package manifest differs");
    ensure!(fs::read_dir(path)?.count() == 2 && fs::read_dir(path.join("datasets"))?.count() == files.len(), "existing package contains unexpected files");
    for (name, data) in files {
        ensure!(input::read_regular(&path.join("datasets").join(format!("{name}.json")))? == *data, "existing package dataset differs: {name}");
    }
    Ok(())
}

fn publish(adaption: Option<(&Path, &Path)>, indices: Option<(&Path, &str)>, output: &Path, switch: bool) -> Result<String> {
    let mut client_build = None;
    let mut datasets = BTreeMap::new();
    let mut files = BTreeMap::new();
    let mut sources = BTreeMap::new();
    let mut adaption_section = None;
    if let Some((input_root, constants_path)) = adaption {
        let bundle = input::load(input_root)?;
        let constants = input::read_regular(constants_path)?;
        let (chip_types, symbols) = input::constants(&constants)?;
        client_build = Some(bundle.build.clone());
        for (name, (records, data)) in &bundle.files {
            datasets.insert(name.clone(), Dataset { path: format!("datasets/{name}.json"), records: u64::try_from(*records)?, sha256: input::hash(data) });
            files.insert(name.clone(), data.clone());
        }
        adaption_section = Some(Adaption {
            source_manifest_sha256: bundle.source_hash, constants_sha256: input::hash(&constants), chip_types,
            provenance: Provenance { source_validation: bundle.validation, chip_type_symbols: symbols },
            validation: Validation { scope: "chip-ledger", node_task_effect_gameplay_enabled: false },
        });
    }
    if let Some((source_root, build)) = indices {
        match &client_build {
            Some(previous) => ensure!(*previous == build, "client builds differ between content domains"),
            None => client_build = Some(build.to_owned()),
        }
        let extracted = sovereignty::extract(source_root, build)?;
        datasets.insert(sovereignty::INDICES_FAMILY.to_owned(), Dataset {
            path: format!("datasets/{}.json", sovereignty::INDICES_FAMILY), records: 1, sha256: input::hash(&extracted.bytes),
        });
        files.insert(sovereignty::INDICES_FAMILY.to_owned(), extracted.bytes);
        for (name, identity) in extracted.sources {
            sources.insert(name, Source { sha256: identity.sha256, bytes: identity.bytes });
        }
    }
    let manifest = Manifest {
        schema: "infinity-content", schema_version: 2,
        client_build: client_build.context("at least one content domain is required")?,
        sources, datasets, adaption: adaption_section,
    };
    let root = fs::canonicalize(output).context("output root must already exist")?;
    regular_dir(&root)?;
    let bytes = serde_json::to_vec(&manifest)?;
    ensure!(bytes.len() as u64 <= input::MAX_BYTES, "output manifest exceeds size limit");
    let digest = input::hash(&bytes);
    let staged = stage(&root)?;
    fs::create_dir(staged.0.join("datasets"))?;
    fs::write(staged.0.join("manifest.json"), &bytes)?;
    for (name, data) in &files { fs::write(staged.0.join("datasets").join(format!("{name}.json")), data)?; }
    let build_dir = root.join(&manifest.client_build);
    match fs::create_dir(&build_dir) {
        Ok(()) => (),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => regular_dir(&build_dir)?,
        Err(e) => return Err(e.into()),
    }
    let target = build_dir.join(&digest);
    if target.exists() || fs::symlink_metadata(&target).is_ok() {
        verify_existing(&target, &bytes, &files)?;
    } else if let Err(e) = fs::rename(&staged.0, &target) {
        if fs::symlink_metadata(&target).is_ok() { verify_existing(&target, &bytes, &files)?; } else { return Err(e.into()); }
    }
    let relative = format!("{}/{digest}", manifest.client_build);
    if switch {
        let current = root.join("current");
        match fs::symlink_metadata(&current) {
            Ok(meta) => ensure!(meta.file_type().is_file(), "current is not a regular file"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(e) => return Err(e.into()),
        }
        let pointer_stage = stage(&root)?;
        let pointer = pointer_stage.0.join("current");
        let mut file = fs::OpenOptions::new().write(true).create_new(true).open(&pointer)?;
        writeln!(file, "{relative}")?;
        drop(file);
        fs::rename(pointer, current)?;
    }
    Ok(relative)
}

pub(super) fn run(args: impl Iterator<Item = String>) -> Result<()> {
    let mut input = None; let mut constants = None; let mut source = None; let mut build = None;
    let mut output = None; let mut switch = false;
    let mut args = args;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--input" => { ensure!(input.is_none(), "duplicate --input"); input = Some(args.next().context("missing --input value")?); }
            "--client-constants" => { ensure!(constants.is_none(), "duplicate --client-constants"); constants = Some(args.next().context("missing --client-constants value")?); }
            "--sovereignty-source" => { ensure!(source.is_none(), "duplicate --sovereignty-source"); source = Some(args.next().context("missing --sovereignty-source value")?); }
            "--client-build" => { ensure!(build.is_none(), "duplicate --client-build"); build = Some(args.next().context("missing --client-build value")?); }
            "--output" => { ensure!(output.is_none(), "duplicate --output"); output = Some(args.next().context("missing --output value")?); }
            "--publish" => { ensure!(!switch, "duplicate --publish"); switch = true; }
            _ => bail!("unknown argument: {arg}"),
        }
    }
    let adaption = match (input, constants) {
        (Some(input), Some(constants)) => Some((PathBuf::from(input), PathBuf::from(constants))),
        (None, None) => None,
        _ => bail!("--input and --client-constants must be given together"),
    };
    let indices = match (source, build) {
        (Some(source), Some(build)) => Some((PathBuf::from(source), build)),
        (None, None) => None,
        _ => bail!("--sovereignty-source and --client-build must be given together"),
    };
    let output = PathBuf::from(output.context("missing --output")?);
    let result = publish(
        adaption.as_ref().map(|(input, constants)| (input.as_path(), constants.as_path())),
        indices.as_ref().map(|(source, build)| (source.as_path(), build.as_str())),
        &output, switch,
    )?;
    println!("{result}");
    Ok(())
}

#[cfg(test)]
mod tests;
