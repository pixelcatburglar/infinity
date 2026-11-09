//! Discovery and immutable, hashed source input for client resources.
//! Resource paths come from the registered client inputs, the supplied SDE
//! bundle and typed SQL. Case-insensitive discovery rejects ambiguous names
//! and symlinks; size and SHA-256 identity checks reject changed resources.
//! These checks identify bytes, not an inferred client build or hotfix.
use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};
use sde_import::encoding::hex;
use std::fs::File;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

// Resource allocation ceiling; pickle has a stricter decoder-specific 128 MiB limit.
const MAX_RESOURCE_BYTES: u64 = 1024 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct Input {
    pub family: String,
    pub relative_path: String,
    pub path: PathBuf,
    pub sha256: String,
    pub bytes: std::sync::Arc<Vec<u8>>,
    /// Number of successfully decoded records, absent until decoding completes.
    pub decoded_records: Option<u64>,
}

/// Resolve each component case-insensitively, rejecting aliases and symlinks.
pub fn discover(root: &Path, relative: &str) -> Result<PathBuf> {
    let mut current = root.to_path_buf();
    for component in Path::new(relative).components() {
        let Component::Normal(name) = component else { bail!("invalid resource path {relative}") };
        let target = name.to_str().context("non-UTF-8 resource name")?;
        let mut found = None;
        for entry in std::fs::read_dir(&current).with_context(|| format!("reading {}", current.display()))? {
            let entry = entry?;
            let name = entry.file_name();
            if name.to_str().is_some_and(|name| name.eq_ignore_ascii_case(target)) {
                if found.is_some() { bail!("ambiguous resource component {target} in {}", current.display()) }
                if entry.file_type()?.is_symlink() { bail!("resource symlink is not supported: {}", entry.path().display()) }
                found = Some(entry.path());
            }
        }
        current = found.with_context(|| format!("missing resource {relative}"))?;
    }
    if !current.is_file() { bail!("resource is not a regular file: {}", current.display()) }
    Ok(current)
}

pub fn read_input(root: &Path, family: &str, relative: &str) -> Result<Input> {
    let path = discover(root, relative)?;
    let file = File::open(&path).with_context(|| format!("opening {}", path.display()))?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > MAX_RESOURCE_BYTES { bail!("invalid or oversized resource {}", path.display()) }
    let mut bytes = Vec::new();
    file.take(MAX_RESOURCE_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_RESOURCE_BYTES || bytes.len() as u64 != metadata.len() {
        bail!("resource changed or exceeded limit: {}", path.display())
    }
    let sha256 = hex(&Sha256::digest(&bytes));
    let actual_relative = path.strip_prefix(root).context("resource escaped root")?.to_string_lossy().replace('\\', "/");
    Ok(Input { family: family.to_owned(), relative_path: actual_relative, path, sha256, bytes: std::sync::Arc::new(bytes), decoded_records: None })
}

/// Detect replacement or modification before mutation, including SQLite resources.
pub fn recheck(input: &Input) -> Result<()> {
    let file = File::open(&input.path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() != input.bytes.len() as u64 { bail!("resource identity changed: {}", input.path.display()) }
    let mut hash = Sha256::new();
    let mut limited = file.take(MAX_RESOURCE_BYTES + 1);
    let mut buffer = [0; 65536];
    loop {
        let count = limited.read(&mut buffer)?;
        if count == 0 { break }
        hash.update(&buffer[..count]);
    }
    if hex(&hash.finalize()) != input.sha256 { bail!("resource identity changed: {}", input.path.display()) }
    Ok(())
}

#[cfg(test)]
mod tests;
