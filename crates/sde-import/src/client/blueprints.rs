//! Extract blueprint records from the read-only SQLite `blueprints.static` cache.
//! Client `industry/blueprint.py` selects the cache; `fsdlite/storage.py` uses
//! JSON encoding for its `cache.value` payloads.
//! Keys and JSON objects are checked and retained alongside consumer views.
//! SQLite paths must be absolute. Windows paths must be UTF-8; drive and UNC
//! paths are supported, but device namespaces other than verbatim drives and UNC are not.
//! Other cache layouts and industry runtime behavior are not implemented here.
use std::{collections::HashSet, path::{Path, PathBuf}};
use anyhow::{bail, Context, Result};
use rusqlite::{Connection, OpenFlags, types::ValueRef};
use serde_json::{Map, Number, Value};
use crate::report::diff::{self, ExactNumber, ExactValue};

// The exact tree is authoritative; this view is only for consumers with JSON Value domains.
fn consumer_value(value: &ExactValue) -> Result<Value> {
    Ok(match value {
        ExactValue::Null => Value::Null,
        ExactValue::Bool(v) => Value::Bool(*v),
        ExactValue::String(v) => Value::String(v.clone()),
        ExactValue::Number(n) => {
            if n.token == "-0" || n.token.bytes().any(|b| matches!(b, b'.' | b'e' | b'E')) {
                Value::Number(Number::from_f64(n.finite_f64().map_err(anyhow::Error::msg)?).context("nonfinite blueprint number")?)
            } else if let Ok(v) = n.token.parse::<i64>() {
                Value::from(v)
            } else if let Ok(v) = n.token.parse::<u64>() {
                Value::from(v)
            } else {
                bail!("blueprint integer outside JSON Value domain");
            }
        }
        ExactValue::Array(items) => Value::Array(items.iter().map(consumer_value).collect::<Result<Vec<_>>>()?),
        ExactValue::Object(items) => {
            let mut object = Map::new();
            for (key, value) in items { object.insert(key.clone(), consumer_value(value)?); }
            Value::Object(object)
        }
    })
}

fn sibling(path: &Path, suffix: &str) -> PathBuf { let mut name = path.as_os_str().to_owned(); name.push(suffix); PathBuf::from(name) }
pub(super) fn check_journals(path: &Path) -> Result<()> {
    match std::fs::symlink_metadata(sibling(path, "-wal")) { Ok(m) if !m.is_file() || m.len() > 0 => bail!("nonempty or non-regular SQLite WAL"), Ok(_) => (), Err(e) if e.kind() == std::io::ErrorKind::NotFound => (), Err(e) => return Err(e.into()) }
    match std::fs::symlink_metadata(sibling(path, "-journal")) { Ok(_) => bail!("SQLite journal candidate"), Err(e) if e.kind() == std::io::ErrorKind::NotFound => (), Err(e) => return Err(e.into()) }
    Ok(())
}
fn uri(path: &Path) -> Result<String> {
    if !path.is_absolute() { bail!("SQLite path must be absolute"); }
    #[cfg(windows)]
    { uri_from_bytes(path.to_str().context("SQLite path is not UTF-8")?.as_bytes(), true) }
    #[cfg(not(windows))]
    { uri_from_bytes(path.as_os_str().as_encoded_bytes(), false) }
}

fn uri_from_bytes(mut path: &[u8], windows: bool) -> Result<String> {
    let mut out = String::from("file:");
    if windows {
        if path.starts_with(br"\\?\UNC\") {
            path = &path[8..];
            out.push_str("////");
        } else if path.starts_with(br"\\?\") {
            path = &path[4..];
            if !(path.len() >= 3 && path[0].is_ascii_alphabetic() && path[1] == b':' && matches!(path[2], b'\\' | b'/')) {
                bail!("unsupported Windows SQLite path namespace");
            }
            out.push('/');
        } else if path.starts_with(br"\\.\") || path.starts_with(br"\??\") || path.starts_with(b"//?/") || path.starts_with(b"//./") {
            bail!("unsupported Windows SQLite path namespace");
        } else if path.starts_with(br"\\") || path.starts_with(b"//") {
            path = &path[2..];
            out.push_str("////");
        } else if path.len() >= 3 && path[0].is_ascii_alphabetic() && path[1] == b':' && matches!(path[2], b'\\' | b'/') {
            out.push('/');
        } else {
            bail!("SQLite path must be absolute");
        }
        if out == "file:////" {
            let mut components = path.split(|b| matches!(b, b'\\' | b'/'));
            if components.next().is_none_or(|s| s.is_empty()) || components.next().is_none_or(|s| s.is_empty()) {
                bail!("SQLite UNC path must include a server and share");
            }
        }
    } else if !path.starts_with(b"/") {
        bail!("SQLite path must be absolute");
    }
    for (i, &byte) in path.iter().enumerate() {
        let b = if windows && byte == b'\\' { b'/' } else { byte };
        if b.is_ascii_alphanumeric() || b"/-._~".contains(&b) || (windows && b == b':' && i == 1 && out.len() == "file:/C".len()) {
            out.push(b as char);
        } else {
            use std::fmt::Write;
            write!(out, "%{b:02X}")?;
        }
    }
    out.push_str("?immutable=1"); Ok(out)
}

/// Aligned native views and exact JSON payloads. Only `raw_records` is authoritative.
pub struct DecodedBlueprints {
    pub records: Vec<Value>,
    pub raw_records: Vec<String>,
}

/// Own all rows before the connection closes; caller verifies identity before and after preparation.
pub fn decode(path: &Path) -> Result<DecodedBlueprints> {
    check_journals(path)?;
    let db = Connection::open_with_flags(uri(path)?, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI | OpenFlags::SQLITE_OPEN_NO_MUTEX).context("opening immutable blueprint cache")?;
    let mut integrity = db.prepare("PRAGMA integrity_check")?;
    let checks = integrity.query_map([], |row| row.get::<_, String>(0))?.collect::<std::result::Result<Vec<_>, _>>()?;
    if checks != ["ok"] { bail!("blueprint SQLite integrity check failed: {checks:?}"); }
    let mut schema = db.prepare("PRAGMA table_info(cache)")?;
    let columns = schema.query_map([], |row| row.get::<_, String>(1))?.collect::<std::result::Result<Vec<_>, _>>()?;
    if !columns.iter().any(|x| x == "key") || !columns.iter().any(|x| x == "value") { bail!("invalid blueprint cache schema"); }
    let mut stmt = db.prepare("SELECT key, value FROM cache ORDER BY key")?;
    let mut query = stmt.query([])?;
    let mut seen = HashSet::new(); let mut result = Vec::new(); let mut raw_records = Vec::new();
    while let Some(row) = query.next()? {
        let key: i64 = match row.get_ref(0)? { ValueRef::Integer(i) => i, ValueRef::Text(t) => std::str::from_utf8(t)?.parse().context("invalid blueprint key")?, _ => bail!("invalid blueprint key type") };
        if !seen.insert(key) { bail!("duplicate coerced blueprint key {key}"); }
        let raw = match row.get_ref(1)? { ValueRef::Text(t) | ValueRef::Blob(t) => t, _ => bail!("invalid blueprint JSON payload type") };
        let text = std::str::from_utf8(raw)?;
        let exact = diff::parse_json(text).map_err(anyhow::Error::msg)?;
        let ExactValue::Object(mut exact_obj) = exact else { bail!("blueprint payload is not an object") };
        if exact_obj.contains_key("_key") { bail!("blueprint payload contains _key"); }
        if let Some(id) = exact_obj.get("blueprintTypeID") {
            let ExactValue::Number(n) = id else { bail!("blueprintTypeID is not an integer") };
            if n.token.parse::<i64>().ok() != Some(key) { bail!("blueprintTypeID mismatch"); }
        }
        exact_obj.insert("_key".into(), ExactValue::Number(ExactNumber::parse(&key.to_string()).map_err(anyhow::Error::msg)?));
        let exact = ExactValue::Object(exact_obj);
        let value = consumer_value(&exact)?;
        raw_records.push(serde_json::to_string(&exact)?);
        result.push(value);
    }
    check_journals(path)?;
    Ok(DecodedBlueprints { records: result, raw_records })
}
#[cfg(test)]
mod tests;
