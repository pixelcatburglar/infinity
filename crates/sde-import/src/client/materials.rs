//! Decode observed cFSD type-material and required-skill projections.
//! Client `typematerials/data.py` and `evetypes/skills.py` select their loaders.
//! Inputs are `typematerials` and `requiredskillsfortypes` static resources.
//! Layouts are inferred, not complete schemas; unsupported variants fail.
//! Randomized material probability and other opaque bytes are not recovered.
use std::collections::HashSet;

use anyhow::{bail, Context, Result};
use serde_json::{json, Map, Value};

use super::cfsd::{bucket_records, payload, Reader};

fn number(value: u64, field: &str) -> Result<usize> {
    usize::try_from(value).with_context(|| format!("{field} exceeds address space"))
}

fn add(a: usize, b: usize, field: &str) -> Result<usize> {
    a.checked_add(b).with_context(|| format!("{field} offset overflow"))
}

fn size(count: usize, stride: usize, field: &str) -> Result<usize> {
    count.checked_mul(stride).with_context(|| format!("{field} size overflow"))
}

fn region(reader: &Reader<'_>, start: usize, length: usize, low: usize, high: usize, field: &str) -> Result<()> {
    let end = add(start, length, field)?;
    if start < low || end > high { bail!("{field} outside bucket extent") }
    reader.slice(start, length).with_context(|| format!("{field} out of bounds"))?;
    Ok(())
}

fn count_at(reader: &Reader<'_>, ptr: usize, low: usize, high: usize, field: &str) -> Result<usize> {
    let header = ptr.checked_sub(8).with_context(|| format!("{field} pointer has no count"))?;
    region(reader, header, 8, low, high, field)?;
    number(reader.u64(header)?, field)
}

pub(super) fn decode(family: &str, raw: &[u8]) -> Result<Vec<Value>> {
    let stride = match family {
        "typematerials" => 32,
        "requiredskillsfortypes" => 24,
        _ => bail!("unsupported materials family {family}"),
    };
    decode_inner(family, raw, stride).with_context(|| format!("decoding {family}"))
}

fn decode_inner(family: &str, raw: &[u8], stride: usize) -> Result<Vec<Value>> {
    let reader = payload(raw)?;
    let records = bucket_records(&reader, stride, 8)?;
    let table = number(reader.u64(0)?, "bucket table")?;
    let buckets = number(reader.u64(16)?, "bucket count")?;
    let mut output = Vec::with_capacity(records.len());
    for record in records {
        let result = (|| -> Result<Value> {
            let bucket = usize::try_from(record.key % u64::try_from(buckets)?)?;
            let entry = add(table, size(bucket, 8, "bucket")?, "bucket")?;
            let start = number(reader.u64(entry)?, "bucket start")?;
            let high = if bucket + 1 < buckets {
                number(reader.u64(add(entry, 8, "bucket")?)?, "next bucket")?
                    .checked_sub(8).context("next bucket count underflow")?
            } else { reader.data.len() };
            let count = number(reader.u64(start.checked_sub(8).context("bucket count underflow")?)?, "bucket record count")?;
            let low = add(start, size(count, stride, "bucket records")?, "bucket records")?;
            let mut row = Map::new();
            row.insert("_key".into(), json!(record.key));
            let first = number(reader.u64(add(record.offset, 8, "first pointer")?)?, "first pointer")?;
            if family == "typematerials" {
                let second = number(reader.u64(add(record.offset, 16, "random pointer")?)?, "random pointer")?;
                let count = count_at(&reader, first, low, high, "materials")?;
                let random_count = count_at(&reader, second, low, high, "randomizedMaterials")?;
                let flags = reader.u64(add(record.offset, 24, "material flags")?)?;
                if flags & !1 != 0 || (random_count != 0 && flags & 1 == 0) {
                    bail!("unsupported material flags {flags}")
                }
                region(&reader, first, size(count, 8, "materials")?, low, high, "materials")?;
                let mut materials = Vec::with_capacity(count);
                for i in 0..count {
                    let p = add(first, size(i, 8, "materials")?, "materials")?;
                    materials.push(json!({"materialTypeID": reader.u32(p)?, "quantity": reader.u32(add(p, 4, "quantity")?)?}));
                }
                row.insert("materials".into(), Value::Array(materials));
                region(&reader, second, size(random_count, 12, "randomizedMaterials")?, low, high, "randomizedMaterials")?;
                if flags & 1 != 0 {
                    let mut random = Vec::with_capacity(random_count);
                    for i in 0..random_count {
                        let p = add(second, size(i, 12, "randomizedMaterials")?, "randomizedMaterials")?;
                        random.push(json!({"materialTypeID": reader.u32(p)?, "quantityMax": reader.u32(add(p, 4, "quantityMax")?)?, "quantityMin": reader.u32(add(p, 8, "quantityMin")?)?}));
                    }
                    row.insert("randomizedMaterials".into(), Value::Array(random));
                }
            } else {
                let expected = number(reader.u64(add(record.offset, 16, "skill count")?)?, "skill count")?;
                let nested = count_at(&reader, first, low, high, "skill buckets")?;
                if nested == 0 { bail!("zero nested skill bucket count") }
                region(&reader, first, size(nested, 8, "skill buckets")?, low, high, "skill buckets")?;
                let mut seen = HashSet::new();
                for bucket in 0..nested {
                    let entry = add(first, size(bucket, 8, "skill bucket")?, "skill bucket")?;
                    let ptr = number(reader.u64(entry)?, "skill bucket pointer")?;
                    let count = count_at(&reader, ptr, low, high, "skill entries")?;
                    region(&reader, ptr, size(count, 8, "skill entries")?, low, high, "skill entries")?;
                    for i in 0..count {
                        let p = add(ptr, size(i, 8, "skill entry")?, "skill entry")?;
                        let skill = reader.u32(p)?;
                        let level = reader.u32(add(p, 4, "skill level")?)?;
                        if usize::try_from(skill)? % nested != bucket || !seen.insert(skill) {
                            bail!("duplicate or misbucketed skill {skill}")
                        }
                        if level > 5 { bail!("invalid skill level {level} for skill {skill}") }
                        row.insert(skill.to_string(), json!(level));
                    }
                }
                if seen.len() != expected { bail!("nested skill count mismatch") }
            }
            Ok(Value::Object(row))
        })().with_context(|| format!("key {} at offset {}", record.key, record.offset))?;
        output.push(result);
    }
    Ok(output)
}

#[cfg(test)]
mod tests;
