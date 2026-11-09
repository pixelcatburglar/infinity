//! Decode observed cFSD dogma attributes, categories, units, effects and type associations.
//! Client `dogma/data.py` and `dogma/units.py` select the corresponding loaders.
//! These projections use the packaged dogma static resources, not runtime effects.
//! Layouts are inferred, not complete native schemas; unsupported presence bits
//! and unproved empty modifier domains fail, while opaque bytes are attributed separately.
use anyhow::{bail, Context, Result};
use serde_json::{json, Map, Value};
use super::cfsd::{self, Reader};

fn pointer(r: &Reader<'_>, pos: usize, field: &str) -> Result<usize> {
    usize::try_from(r.u64(pos).with_context(|| format!("{field} pointer"))?)
        .with_context(|| format!("{field} pointer overflow"))
}

fn text(r: &Reader<'_>, pos: usize, field: &str) -> Result<Value> {
    let ptr = pointer(r, pos, field)?;
    Ok(Value::from(r.string(ptr).with_context(|| field.to_owned())?))
}

fn list(r: &Reader<'_>, pos: usize, stride: usize, field: &str) -> Result<(usize, usize)> {
    let ptr = pointer(r, pos, field)?;
    let header = ptr.checked_sub(8).with_context(|| format!("{field} missing count"))?;
    let count = usize::try_from(r.u64(header).with_context(|| format!("{field} count"))?)
        .with_context(|| format!("{field} count overflow"))?;
    let len = count.checked_mul(stride).with_context(|| format!("{field} size overflow"))?;
    r.slice(ptr, len).with_context(|| format!("{field} entries"))?;
    Ok((ptr, count))
}

fn number(r: &Reader<'_>, row: &mut Map<String, Value>, name: &str, pos: usize) -> Result<()> {
    row.insert(name.into(), r.u32(pos).with_context(|| name.to_owned())?.into());
    Ok(())
}

fn byte(r: &Reader<'_>, row: &mut Map<String, Value>, name: &str, pos: usize) -> Result<()> {
    let value = r.u8(pos).with_context(|| name.to_owned())?;
    if value > 1 { bail!("invalid {name} byte {value}") }
    row.insert(name.into(), value.into());
    Ok(())
}

fn optional_numbers(r: &Reader<'_>, row: &mut Map<String, Value>, base: usize, flags: u64, fields: &[(&str, usize, u32)]) -> Result<()> {
    for &(name, offset, bit) in fields {
        if flags & (1u64 << bit) != 0 { number(r, row, name, base + offset)?; }
    }
    Ok(())
}

fn attributes(r: &Reader<'_>, key: u64, p: usize) -> Result<Value> {
    let flags = u64::from(r.u32(p + 76)?);
    if flags & !0x7ff != 0 { bail!("unsupported attribute flags {flags:#x}") }
    let id = r.u32(p + 24).context("attributeID")?;
    if u64::from(id) != key { bail!("attributeID differs from key") }
    let mut row = Map::new();
    row.insert("_key".into(), key.into());
    row.insert("attributeID".into(), id.into());
    row.insert("name".into(), text(r, p + 16, "name")?);
    number(r, &mut row, "dataType", p + 36)?;
    if flags & 4 != 0 { row.insert("description".into(), text(r, p + 8, "description")?); }
    optional_numbers(r, &mut row, p, flags, &[
        ("categoryID",28,0), ("chargeRechargeTimeID",32,1), ("displayNameID",44,3),
        ("iconID",48,5), ("maxAttributeID",52,6), ("minAttributeID",56,7),
        ("tooltipDescriptionID",60,8), ("tooltipTitleID",64,9), ("unitID",68,10),
    ])?;
    if flags & 16 != 0 {
        row.insert("defaultValue".into(), json!(f64::from(r.f32(p + 40).context("defaultValue")?)));
    }
    for (name, offset) in [("displayWhenZero",72), ("highIsGood",73), ("published",74), ("stackable",75)] {
        byte(r, &mut row, name, p + offset)?;
    }
    Ok(Value::Object(row))
}

fn catalogue(r: &Reader<'_>, key: u64, p: usize, family: &str) -> Result<Value> {
    let flags = r.u64(p + 24)?;
    let mut row = Map::new();
    row.insert("_key".into(), key.into());
    if family == "dogmaattributecategories" {
        if flags & !1 != 0 { bail!("unsupported category flags {flags:#x}") }
        row.insert("name".into(), text(r, p + 16, "name")?);
        if flags & 1 != 0 { row.insert("description".into(), text(r, p + 8, "description")?); }
    } else {
        if flags & !3 != 0 { bail!("unsupported unit flags {flags:#x}") }
        row.insert("name".into(), text(r, p + 8, "name")?);
        optional_numbers(r, &mut row, p, flags, &[("descriptionID",16,0), ("displayNameID",20,1)])?;
    }
    Ok(Value::Object(row))
}

fn modifiers(r: &Reader<'_>, p: usize) -> Result<Value> {
    let (start, count) = list(r, p, 48, "modifierInfo")?;
    let mut rows = Vec::with_capacity(count);
    for i in 0..count {
        let pos = start + i * 48;
        let mut row = Map::new();
        let domain = text(r, pos, "modifierInfo.domain")?;
        if domain.as_str() == Some("") { bail!("empty modifierInfo.domain has unproved presence semantics") }
        row.insert("domain".into(), domain);
        row.insert("func".into(), text(r, pos + 8, "modifierInfo.func")?);
        let flags = r.u64(pos + 40).context("modifierInfo flags")?;
        if flags & !0x3f != 0 { bail!("unsupported modifierInfo flags {flags:#x}") }
        optional_numbers(r, &mut row, pos, flags, &[
            ("effectID",16,0), ("groupID",20,1), ("modifiedAttributeID",24,2),
            ("modifyingAttributeID",28,3), ("skillTypeID",36,5),
        ])?;
        if flags & 16 != 0 {
            row.insert("operation".into(), r.i32(pos + 32).context("modifierInfo.operation")?.into());
        }
        rows.push(Value::Object(row));
    }
    Ok(Value::Array(rows))
}

fn effects(r: &Reader<'_>, key: u64, p: usize) -> Result<Value> {
    let flags = u64::from(r.u32(p + 108)?);
    if flags & !0xffff != 0 { bail!("unsupported effect flags {flags:#x}") }
    let id = r.u32(p + 64).context("effectID")?;
    if u64::from(id) != key { bail!("effectID differs from key") }
    let mut row = Map::new();
    row.insert("_key".into(), key.into());
    row.insert("effectID".into(), id.into());
    row.insert("effectName".into(), text(r, p + 8, "effectName")?);
    number(r, &mut row, "effectCategory", p + 60)?;
    optional_numbers(r, &mut row, p, flags, &[
        ("descriptionID",40,0), ("dischargeAttributeID",44,1), ("displayNameID",48,2),
        ("distribution",52,3), ("durationAttributeID",56,4), ("falloffAttributeID",68,5),
        ("fittingUsageChanceAttributeID",72,6), ("iconID",76,8),
        ("npcActivationChanceAttributeID",80,10), ("npcUsageChanceAttributeID",84,11),
        ("rangeAttributeID",88,12), ("resistanceAttributeID",92,13), ("trackingSpeedAttributeID",96,15),
    ])?;
    for (name, offset, bit) in [("guid",16,7), ("sfxName",32,14)] {
        if flags & (1 << bit) != 0 { row.insert(name.into(), text(r, p + offset, name)?); }
    }
    for (name, offset) in [("disallowAutoRepeat",100), ("electronicChance",101), ("isAssistance",102),
        ("isOffensive",103), ("isWarpSafe",104), ("propulsionChance",105), ("published",106), ("rangeChance",107)] {
        byte(r, &mut row, name, p + offset)?;
    }
    if flags & 512 != 0 { row.insert("modifierInfo".into(), modifiers(r, p + 24)?); }
    Ok(Value::Object(row))
}

fn type_dogma(r: &Reader<'_>, key: u64, p: usize) -> Result<Value> {
    let (start, count) = list(r, p + 8, 16, "dogmaAttributes")?;
    let mut attributes = Vec::with_capacity(count);
    for i in 0..count {
        let pos = start + i * 16;
        attributes.push(json!({"attributeID": r.u32(pos + 8).context("dogmaAttributes.attributeID")?,
            "value": r.f64(pos).context("dogmaAttributes.value")?}));
    }
    let (start, count) = list(r, p + 16, 8, "dogmaEffects")?;
    let mut effects = Vec::with_capacity(count);
    for i in 0..count {
        let pos = start + i * 8;
        let default = r.u8(pos + 4).context("dogmaEffects.isDefault")?;
        if default > 1 { bail!("invalid dogmaEffects.isDefault byte {default}") }
        effects.push(json!({"effectID": r.u32(pos).context("dogmaEffects.effectID")?, "isDefault": default}));
    }
    Ok(json!({"_key": key, "dogmaAttributes": attributes, "dogmaEffects": effects}))
}

pub(super) fn decode(family: &str, raw: &[u8]) -> Result<Vec<Value>> {
    let stride = match family {
        "dogmaattributes" => 80, "dogmaattributecategories" | "dogmaunits" => 32,
        "dogmaeffects" => 112, "typedogma" => 24,
        _ => bail!("{family}: unsupported dogma family"),
    };
    let r = cfsd::payload(raw).with_context(|| format!("{family}: payload"))?;
    let records = cfsd::bucket_records(&r, stride, 8).with_context(|| format!("{family}: buckets"))?;
    records.into_iter().map(|record| {
        let result = match family {
            "dogmaattributes" => attributes(&r, record.key, record.offset),
            "dogmaattributecategories" | "dogmaunits" => catalogue(&r, record.key, record.offset, family),
            "dogmaeffects" => effects(&r, record.key, record.offset),
            "typedogma" => type_dogma(&r, record.key, record.offset),
            _ => unreachable!(),
        };
        result.with_context(|| format!("{family} key {}", record.key))
    }).collect()
}

#[cfg(test)]
mod tests;
