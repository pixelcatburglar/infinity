//! Decode observed category, meta-group and market-group cFSD projections.
//! Client loaders are selected by `evetypes/data.py`, `metaGroups/data.py`
//! and `marketgroups/data.py` for their respective static resources.
//! Layouts are inferred from observed variants, not complete native schemas;
//! unsupported flags fail, and unassigned bytes are attributed separately.
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use super::cfsd::{bucket_records, payload, Reader};

pub(super) fn decode(family: &str, raw: &[u8]) -> Result<Vec<Value>> {
    let stride = match family { "categories" => 40, "metagroups" => 48, "marketgroups" => 28, _ => bail!("unsupported family {family}") };
    let p = payload(raw).with_context(|| format!("{family} wrapper"))?;
    let records = bucket_records(&p, stride, 4).with_context(|| format!("{family} buckets"))?;
    let mut rows = Vec::with_capacity(records.len());
    for record in records {
        rows.push(row(&p, family, record.key, record.offset).with_context(|| format!("{family} key {}", record.key))?);
    }
    rows.sort_by_key(|v| v["_key"].as_u64().unwrap_or_default());
    Ok(rows)
}

fn row(p: &Reader<'_>, family: &str, key: u64, o: usize) -> Result<Value> {
    let mut row = json!({"_key":key});
    match family {
        "marketgroups" => {
            let flags = p.u32(o+24)?;
            let has_types = p.u32(o+20)?;
            if flags & !7 != 0 { bail!("unsupported marketgroups flags {flags:#x}") }
            if has_types > 1 { bail!("invalid hasTypes boolean {has_types}") }
            row["nameID"] = json!(p.u32(o+12)?);
            row["hasTypes"] = json!(has_types);
            for (bit, name, pos) in [(1,"descriptionID",4),(2,"iconID",8),(4,"parentGroupID",16)] {
                if flags & bit != 0 { row[name] = json!(p.u32(o+pos).with_context(|| name.to_owned())?); }
            }
        }
        "categories" => {
            let flags = p.u32(o+32)?;
            let id = p.u32(o+16)?;
            let published = p.u8(o+28)?;
            if flags & !3 != 0 { bail!("unsupported categories flags {flags:#x}") }
            if u64::from(id) != key { bail!("categoryID identity mismatch") }
            if published > 1 { bail!("invalid published boolean {published}") }
            row["categoryID"] = json!(id);
            row["categoryNameID"] = json!(p.u32(o+20)?);
            row["published"] = json!(published);
            if flags & 1 != 0 { row["iconID"] = json!(p.u32(o+24)?); }
            if flags & 2 != 0 {
                let ptr = usize::try_from(p.u64(o+8)?).context("sofBuildClass pointer overflow")?;
                row["sofBuildClass"] = json!(p.string(ptr).context("sofBuildClass")?);
            }
        }
        "metagroups" => {
            let flags = p.u32(o+44)?;
            if flags & !15 != 0 { bail!("unsupported metagroups flags {flags:#x}") }
            row["nameID"] = json!(p.u32(o+40)?);
            if flags & 1 != 0 {
                let mut color = Vec::with_capacity(4);
                for i in 0..4 { color.push(f64::from(p.f32(o+16+i*4).context("color")?)); }
                row["color"] = json!(color);
            }
            for (bit, name, pos) in [(2,"descriptionID",32),(4,"iconID",36)] {
                if flags & bit != 0 { row[name] = json!(p.u32(o+pos).with_context(|| name.to_owned())?); }
            }
            if flags & 8 != 0 {
                let ptr = usize::try_from(p.u64(o+8)?).context("iconSuffix pointer overflow")?;
                row["iconSuffix"] = json!(p.string(ptr).context("iconSuffix")?);
            }
        }
        _ => bail!("unsupported family {family}"),
    }
    Ok(row)
}

#[cfg(test)]
mod tests;
