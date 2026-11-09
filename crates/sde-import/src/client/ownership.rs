//! Attribute file-relative intervals in the observed cFSD source resources.
//! This is offline import evidence, not a client service or protocol mirror.
//! Wrapper, bucket, record and pointed storage claims produce ownership intervals.
//! Record bodies remain opaque; unknown and overlapping bytes do not prove a schema.
use anyhow::{bail, Context, Result};
use std::collections::{BTreeMap, BTreeSet};
use super::cfsd::{bucket_bounds, bucket_records, payload, Reader};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind { Owned, Unknown, Unowned }

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Interval {
    pub offset: usize,
    pub length: usize,
    pub kind: Kind,
    pub owner_keys: Vec<u64>,
    pub label: String,
    pub hex: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ownership {
    pub intervals: Vec<Interval>,
    pub source_bytes: usize,
    pub owned_bytes: usize,
    pub unknown_bytes: usize,
    pub unowned_bytes: usize,
}

#[derive(Clone)]
struct Claim { start: usize, end: usize, kind: Kind, key: Option<u64>, label: &'static str }

fn add(a: usize, b: usize) -> Result<usize> { a.checked_add(b).context("ownership offset overflow") }
fn mul(a: usize, b: usize) -> Result<usize> { a.checked_mul(b).context("ownership length overflow") }
fn number(reader: &Reader<'_>, offset: usize) -> Result<usize> { Ok(usize::try_from(reader.u64(offset)?)?) }

fn claim(claims: &mut Vec<Claim>, raw: &[u8], start: usize, length: usize, kind: Kind, key: Option<u64>, label: &'static str) -> Result<()> {
    let end = add(start, length)?;
    raw.get(start..end).context("ownership claim out of bounds")?;
    if length != 0 { claims.push(Claim { start, end, kind, key, label }); }
    Ok(())
}

fn vector(claims: &mut Vec<Claim>, raw: &[u8], r: &Reader<'_>, pointer: usize, width: usize, key: u64, label: &'static str, bounds: Option<(usize, usize)>) -> Result<usize> {
    let start = pointer.checked_sub(8).context("vector pointer has no prefix")?;
    let count = number(r, start)?;
    let length = add(8, mul(count, width)?)?;
    r.slice(start, length)?;
    if let Some((low, high)) = bounds {
        if start < low || start > high || length > high - start { bail!("nested {label} out of bucket for {key}") }
    }
    claim(claims, raw, add(start, 32)?, length, Kind::Unknown, Some(key), label)?;
    Ok(count)
}

fn string(claims: &mut Vec<Claim>, raw: &[u8], r: &Reader<'_>, pointer: usize, key: u64) -> Result<()> {
    r.string(pointer)?;
    let start = pointer.checked_sub(8).context("string pointer has no prefix")?;
    let length = add(number(r, start)?, 9)?;
    claim(claims, raw, add(start, 32)?, length, Kind::Unknown, Some(key), "pointed string")
}

fn allocation(label: &str) -> bool {
    matches!(label.strip_prefix("shared ").unwrap_or(label), "pointed string" | "designer vector" | "material vector" | "skill bucket table" | "skill bucket" | "modifier vector" | "dogma vector")
}

fn finish(raw: &[u8], claims: &[Claim]) -> Ownership {
    // Exact keyed unknown allocations are the only compatible overlaps. Aggregate
    // them once, so deciding each interval never scans all active claims.
    let mut groups: Vec<(Claim, BTreeSet<u64>, usize)> = Vec::new();
    let mut compatible = BTreeMap::new();
    for c in claims {
        if c.kind == Kind::Unknown && c.key.is_some() {
            let identity = (c.start, c.end, c.label);
            if let Some(&index) = compatible.get(&identity) {
                let (_, keys, count): &mut (Claim, BTreeSet<u64>, usize) = &mut groups[index];
                keys.insert(c.key.unwrap());
                *count += 1;
                continue;
            }
            compatible.insert(identity, groups.len());
        }
        groups.push((c.clone(), c.key.into_iter().collect(), 1));
    }
    let mut events: BTreeMap<usize, Vec<(usize, bool)>> = BTreeMap::new();
    events.entry(0).or_default();
    events.entry(raw.len()).or_default();
    for (i, (c, _, _)) in groups.iter().enumerate() {
        events.entry(c.start).or_default().push((i, true));
        events.entry(c.end).or_default().push((i, false));
    }
    let positions: Vec<_> = events.keys().copied().collect();
    let mut active = BTreeSet::new();
    let mut intervals: Vec<Interval> = Vec::new();
    for pair in positions.windows(2) {
        let (pos, end) = (pair[0], pair[1]);
        for &(i, begin) in &events[&pos] { if begin { active.insert(i); } else { active.remove(&i); } }
        let (kind, keys, label) = if active.is_empty() {
            (Kind::Unowned, Vec::new(), "no established owner".to_owned())
        } else if active.len() == 1 {
            let (c, keys, count) = &groups[*active.first().unwrap()];
            let label = if *count > 1 { format!("shared {}", c.label) } else { c.label.to_owned() };
            (c.kind, keys.iter().copied().collect(), label)
        } else {
            (Kind::Unowned, Vec::new(), "overlapping ownership claims".to_owned())
        };
        let mut row = Interval { offset: pos, length: end - pos, kind, owner_keys: keys, label, hex: None };
        if row.kind == Kind::Unowned && row.label == "no established owner" && row.length <= 7 && end % 8 == 0 && raw[pos..end].iter().all(|&b| b == 0) {
            if let Some(previous) = intervals.last() {
                if previous.kind == Kind::Unknown && !previous.owner_keys.is_empty() && allocation(&previous.label) {
                    row.kind = Kind::Unknown;
                    row.owner_keys.clone_from(&previous.owner_keys);
                    row.label = "inferred allocation alignment padding".to_owned();
                }
            }
        }
        if row.kind != Kind::Owned {
            use std::fmt::Write;
            let mut hex = String::with_capacity(row.length.saturating_mul(2));
            for byte in &raw[pos..end] { write!(&mut hex, "{byte:02x}").expect("writing to String cannot fail"); }
            row.hex = Some(hex);
        }
        intervals.push(row);
    }
    let mut result = Ownership { intervals, source_bytes: raw.len(), owned_bytes: 0, unknown_bytes: 0, unowned_bytes: 0 };
    for row in &result.intervals {
        match row.kind { Kind::Owned => result.owned_bytes += row.length, Kind::Unknown => result.unknown_bytes += row.length, Kind::Unowned => result.unowned_bytes += row.length }
    }
    result
}

/// Analyze the twelve observed cFSD layouts. The caller supplies source identity and gates unowned bytes.
pub fn analyze(family: &str, raw: &[u8]) -> Result<Ownership> {
    let (stride, key_width) = match family {
        "types" => (152, 4), "groups" => (32, 4), "categories" => (40, 4),
        "metagroups" => (48, 4), "marketgroups" => (28, 4), "dogmaattributes" => (80, 8),
        "dogmaattributecategories" => (32, 8), "dogmaunits" => (32, 8), "dogmaeffects" => (112, 8),
        "typedogma" => (24, 8), "typematerials" => (32, 8), "requiredskillsfortypes" => (24, 8),
        _ => bail!("unsupported ownership family {family}"),
    };
    let r = payload(raw)?;
    let table = number(&r, 0)?;
    let buckets = number(&r, 16)?;
    let records = bucket_records(&r, stride, key_width)?;
    let mut claims = Vec::new();
    claim(&mut claims, raw, 0, 24, Kind::Unknown, None, "opaque wrapper")?;
    claim(&mut claims, raw, 24, 8, Kind::Owned, None, "payload length")?;
    claim(&mut claims, raw, 32, add(24, mul(buckets, 8)?)?, Kind::Owned, None, "bucket header and table")?;
    for index in 0..buckets {
        let start = number(&r, add(table, mul(index, 8)?)?)?;
        claim(&mut claims, raw, add(start, 24)?, 8, Kind::Owned, None, "bucket count")?;
    }
    for record in records {
        let key = record.key;
        let offset = record.offset;
        claim(&mut claims, raw, add(offset, 32)?, key_width, Kind::Owned, Some(key), "record key")?;
        claim(&mut claims, raw, add(add(offset, 32)?, key_width)?, stride - key_width, Kind::Unknown, Some(key), "record body: semantic ownership not proven")?;
        if matches!(family, "categories" | "metagroups") {
            let p = number(&r, add(offset, 8)?)?;
            if p != 0 { string(&mut claims, raw, &r, p, key)?; }
        }
        if matches!(family, "types" | "typematerials" | "requiredskillsfortypes") {
            let bounds = Some(bucket_bounds(&r, stride, key)?);
            if family == "types" {
                let p = number(&r, add(offset, 24)?)?;
                vector(&mut claims, raw, &r, p, 4, key, "designer vector", bounds)?;
            } else if family == "typematerials" {
                for (pos, width) in [(8, 8), (16, 12)] {
                    vector(&mut claims, raw, &r, number(&r, add(offset, pos)?)?, width, key, "material vector", bounds)?;
                }
            } else {
                let p = number(&r, add(offset, 8)?)?;
                let count = vector(&mut claims, raw, &r, p, 8, key, "skill bucket table", bounds)?;
                for i in 0..count {
                    let child = number(&r, add(p, mul(i, 8)?)?)?;
                    vector(&mut claims, raw, &r, child, 8, key, "skill bucket", bounds)?;
                }
            }
        }
        if matches!(family, "dogmaattributecategories" | "dogmaunits" | "dogmaattributes" | "dogmaeffects") {
            if family == "dogmaattributecategories" && r.u64(add(offset, 24)?)? & !1 != 0 { bail!("unsupported dogma category flags for {key}") }
            if family == "dogmaunits" && r.u64(add(offset, 24)?)? & !3 != 0 { bail!("unsupported dogma unit flags for {key}") }
            let mandatory = if matches!(family, "dogmaattributecategories" | "dogmaattributes") { 16 } else { 8 };
            string(&mut claims, raw, &r, number(&r, add(offset, mandatory)?)?, key)?;
            let optional: &[usize] = match family { "dogmaattributecategories" | "dogmaattributes" => &[8], "dogmaeffects" => &[16, 32], _ => &[] };
            for &pos in optional {
                let p = number(&r, add(offset, pos)?)?;
                if p != 0 { string(&mut claims, raw, &r, p, key)?; }
            }
            if family == "dogmaeffects" {
                let p = number(&r, add(offset, 24)?)?;
                if r.u32(add(offset, 108)?)? & 512 != 0 {
                    let count = vector(&mut claims, raw, &r, p, 48, key, "modifier vector", None)?;
                    for i in 0..count {
                        let base = add(p, mul(i, 48)?)?;
                        let domain = number(&r, base)?;
                        if r.string(domain)?.is_empty() { bail!("empty modifier domain for {key}") }
                        string(&mut claims, raw, &r, domain, key)?;
                        string(&mut claims, raw, &r, number(&r, add(base, 8)?)?, key)?;
                    }
                } else if p != 0 {
                    let prefix = p.checked_sub(8).context("inactive modifier pointer has no prefix")?;
                    if number(&r, prefix)? == 0 { vector(&mut claims, raw, &r, p, 48, key, "modifier vector", None)?; }
                }
            }
        }
        if family == "typedogma" {
            for (pos, width) in [(8, 16), (16, 8)] {
                vector(&mut claims, raw, &r, number(&r, add(offset, pos)?)?, width, key, "dogma vector", None)?;
            }
        }
    }
    Ok(finish(raw, &claims))
}

#[cfg(test)]
mod tests;
