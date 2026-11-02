//! Bounded JSON/YAML-subset encoding and SHA-256 hashing of shared settings.
//! `sharedSettings/util.py:GetDataFromSettingKey` consumes the top-level pair list;
//! nested dictionaries and sequences retain order. Input is supplied by the service,
//! not read from a database. Non-UTF-8, non-finite and unsupported marshal values
//! cannot be encoded. Depth counts logical values, not JSON pair-list wrappers.
use std::{cell::Cell, fmt::Write as _, io::{self, Write}};
use marshal::Value;
use serde::{Serialize, Serializer, ser::{Error, SerializeMap, SerializeSeq}};
use sha2::{Digest, Sha256};

/// A JSON dictionary entry needs at least `"":0` bytes, excluding separators.
const MIN_ENTRY_BYTES: usize = 4;

/// Two hexadecimal digits encode each byte.
const HEX_DIGITS_PER_BYTE: usize = 2;

/// YAML 1.1 requires a decimal point to recognize floats; JSON exponent forms
/// emitted by the default formatter can otherwise load as strings.
struct YamlFloatFormatter;
impl serde_json::ser::Formatter for YamlFloatFormatter {
    fn write_f64<W: ?Sized + Write>(&mut self, writer: &mut W, value: f64) -> io::Result<()> {
        write!(writer, "{value}")?;
        if value.fract() == 0.0 { writer.write_all(b".0")?; }
        Ok(())
    }
}

pub(super) fn text(value: &Value) -> Option<&str> {
    match value { Value::Bytes(bytes) => std::str::from_utf8(bytes).ok(), Value::Unicode(s) => Some(s), _ => None }
}

struct Adapter<'a> { value: &'a Value, depth: usize, max_depth: usize, entries_left: &'a Cell<usize>, top: bool }
impl<'a> Adapter<'a> {
    fn child(&self, value: &'a Value) -> Adapter<'a> {
        Adapter { value, depth: self.depth + 1, max_depth: self.max_depth, entries_left: self.entries_left, top: false }
    }
}
impl Serialize for Adapter<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if self.depth > self.max_depth { return Err(S::Error::custom("depth limit")); }
        match self.value {
            Value::None => serializer.serialize_none(),
            Value::Bool(v) => serializer.serialize_bool(*v),
            Value::Int(v) => serializer.serialize_i64(*v),
            Value::Float(v) if v.is_finite() => serializer.serialize_f64(*v),
            Value::Bytes(_) | Value::Unicode(_) => serializer.serialize_str(text(self.value).ok_or_else(|| S::Error::custom("invalid UTF-8"))?),
            Value::List(items) | Value::Tuple(items) => {
                let mut seq = serializer.serialize_seq(Some(items.len()))?;
                for item in items { seq.serialize_element(&self.child(item))?; }
                seq.end()
            }
            Value::Dict(items) => {
                let remaining = self.entries_left.get().checked_sub(items.len()).ok_or_else(|| S::Error::custom("size limit"))?;
                self.entries_left.set(remaining);
                let mut entries = Vec::with_capacity(items.len());
                for (key, value) in items {
                    let key = text(key).ok_or_else(|| S::Error::custom("invalid key"))?;
                    entries.push((key, value));
                }
                entries.sort_unstable_by_key(|(key, _)| *key);
                if entries.windows(2).any(|w| w[0].0 == w[1].0) { return Err(S::Error::custom("duplicate key")); }
                if self.top {
                    let mut seq = serializer.serialize_seq(Some(entries.len()))?;
                    for (key, value) in entries { seq.serialize_element(&(key, self.child(value)))?; }
                    seq.end()
                } else {
                    // Preserve the input's dictionary order, after checking duplicate keys.
                    let mut map = serializer.serialize_map(Some(items.len()))?;
                    for (key, value) in items { map.serialize_entry(text(key).ok_or_else(|| S::Error::custom("invalid key"))?, &self.child(value))?; }
                    map.end()
                }
            }
            _ => Err(S::Error::custom("unsupported value")),
        }
    }
}

struct Bounded { bytes: Vec<u8>, limit: usize }
impl Write for Bounded {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > self.limit.saturating_sub(self.bytes.len()) { return Err(io::Error::new(io::ErrorKind::InvalidData, "size limit")); }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> { Ok(()) }
}

pub(super) fn encode(value: &Value, limit: usize, depth: usize) -> Result<(String, String), ()> {
    if !matches!(value, Value::Dict(_)) { return Err(()); }
    let mut writer = Bounded { bytes: Vec::new(), limit };
    let entries_left = Cell::new(limit / MIN_ENTRY_BYTES);
    Adapter { value, depth: 1, max_depth: depth, entries_left: &entries_left, top: true }
        .serialize(&mut serde_json::Serializer::with_formatter(&mut writer, YamlFloatFormatter))
        .map_err(|_| ())?;
    let digest = Sha256::digest(&writer.bytes);
    let mut hash = String::with_capacity(digest.len() * HEX_DIGITS_PER_BYTE);
    for byte in digest {
        write!(&mut hash, "{byte:02x}").expect("writing to a String cannot fail");
    }
    Ok((String::from_utf8(writer.bytes).map_err(|_| ())?, hash))
}

#[cfg(test)]
mod tests;
