//! Decode observed type and group projections from their cFSD static resources.
//! Client `evetypes/data.py` selects distinct type and group loaders.
//! Layouts are inferred from observed variants, not a general native schema.
//! Unsupported presence masks fail; unassigned bytes are attributed separately.
use anyhow::{bail, Context, Result};
use serde_json::{json, Map, Value};
use super::cfsd::{bucket_bounds, bucket_records, payload, Reader};

const OPTIONALS: &[(&str, usize, u32)] = &[
    ("certificateTemplate", 64, 2), ("descriptionID", 68, 3),
    ("factionID", 72, 5), ("graphicID", 80, 6), ("iconID", 88, 7),
    ("isisGroupID", 92, 9), ("marketGroupID", 96, 10),
    ("metaGroupID", 100, 12), ("metaLevel", 104, 13),
    ("raceID", 116, 18), ("soundID", 120, 20), ("techLevel", 124, 21),
    ("variationParentTypeID", 136, 22), ("wreckTypeID", 140, 24),
];

pub(super) fn decode(family: &str, raw: &[u8]) -> Result<Vec<Value>> {
    let stride = match family { "types" => 152, "groups" => 32, _ => bail!("unsupported family {family}") };
    let p = payload(raw).with_context(|| format!("{family} wrapper"))?;
    let records = bucket_records(&p, stride, 4).with_context(|| format!("{family} buckets"))?;
    let mut rows = Vec::with_capacity(records.len());
    for record in records {
        let row = if family == "types" { type_row(&p, record.key, record.offset) } else { group_row(&p, record.key, record.offset) }
            .with_context(|| format!("{family} key {}", record.key))?;
        rows.push(row);
    }
    rows.sort_by_key(|v| v["_key"].as_u64().unwrap_or_default());
    Ok(rows)
}

fn boolean(p: &Reader<'_>, offset: usize, name: &str) -> Result<u8> {
    let v = p.u8(offset).with_context(|| name.to_owned())?;
    if v > 1 { bail!("invalid {name} boolean {v}") }
    Ok(v)
}

fn group_row(p: &Reader<'_>, key: u64, o: usize) -> Result<Value> {
    let flags = p.u32(o + 28)?;
    if flags & 0x37 != 0x37 || flags & !0x3f != 0 { bail!("unsupported group flags {flags:#x}") }
    let group = p.u32(o + 8)?;
    if u64::from(group) != key { bail!("groupID identity mismatch") }
    let mut row = json!({"_key":key,"categoryID":p.u32(o+4)?,"groupID":group,
        "groupNameID":p.u32(o+12)?,"anchorable":boolean(p,o+20,"anchorable")?,
        "anchored":boolean(p,o+21,"anchored")?,"fittableNonSingleton":boolean(p,o+22,"fittableNonSingleton")?,
        "published":boolean(p,o+23,"published")?,"useBasePrice":boolean(p,o+24,"useBasePrice")?});
    if flags & 8 != 0 { row["iconID"] = json!(p.u32(o+16)?); }
    Ok(row)
}

fn type_row(p: &Reader<'_>, key: u64, o: usize) -> Result<Value> {
    let flags = p.u32(o + 148)?;
    const REQUIRED: u32 = (1<<0)|(1<<1)|(1<<11)|(1<<14)|(1<<15)|(1<<19)|(1<<23);
    if flags & REQUIRED != REQUIRED || flags & !0x1ff_ffff != 0 { bail!("unsupported type flags {flags:#x}") }
    // The two quote presence bits were observed only together; individual meanings are unproved.
    if (flags >> 16) & 3 == 1 || (flags >> 16) & 3 == 2 { bail!("unsupported independent quote presence flags") }
    let id = p.u32(o + 128)?;
    if u64::from(id) != key { bail!("typeID identity mismatch") }
    let mut row = Map::new();
    row.insert("_key".into(), json!(key));
    row.insert("typeID".into(), json!(id));
    for (name, pos) in [("basePrice",8),("capacity",16),("mass",32),("radius",48),("volume",56)] {
        row.insert(name.into(), json!(p.f64(o+pos).with_context(|| name.to_owned())?));
    }
    row.insert("portionSize".into(), json!(p.u64(o+40)?));
    row.insert("groupID".into(), json!(p.u32(o+84)?));
    row.insert("typeNameID".into(), json!(p.u32(o+132)?));
    row.insert("published".into(), json!(boolean(p,o+145,"published")?));
    for &(name, pos, bit) in OPTIONALS {
        if flags & (1 << bit) != 0 { row.insert(name.into(), json!(p.u32(o+pos).with_context(|| name.to_owned())?)); }
    }
    if flags & (3 << 16) != 0 {
        row.insert("quoteAuthorID".into(), json!(p.u32(o+108)?));
        row.insert("quoteID".into(), json!(p.u32(o+112)?));
    }
    if flags & (1 << 8) != 0 { row.insert("isDynamicType".into(), json!(boolean(p,o+144,"isDynamicType")?)); }
    let (records_end, end) = bucket_bounds(p, 152, key).context("designerIDs bucket bounds")?;
    let ptr = usize::try_from(p.u64(o+24)?).context("designerIDs pointer overflow")?;
    if ptr < records_end.checked_add(8).context("designerIDs pointer overflow")? || ptr > end { bail!("invalid designerIDs pointer") }
    let len = usize::try_from(p.u64(ptr-8)?).context("designerIDs count overflow")?;
    if len > (end-ptr)/4 { bail!("invalid designerIDs count") }
    if flags & (1<<4) != 0 {
        let mut values = Vec::with_capacity(len);
        for i in 0..len { values.push(p.u32(ptr + i*4)?); }
        row.insert("designerIDs".into(), json!(values));
    }
    Ok(Value::Object(row))
}

#[cfg(test)]
mod tests;
