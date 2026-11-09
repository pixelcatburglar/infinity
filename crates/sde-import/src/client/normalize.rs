//! Project decoded native families into compatibility rows for offline import.
//! This is importer plumbing, not a client service or protocol mirror.
//! Names and descriptions resolve from `localization_fsd_en_us`; source IDs remain.
//! Other language rows are never used as fallback, and missing required text fails.
use std::collections::HashMap;

use anyhow::{bail, Context, Result};
use serde_json::{Map, Value};

use super::FamilyRows;

pub(super) fn normalize(native: &FamilyRows) -> Result<FamilyRows> {
    let english_rows = native.get("localization_fsd_en_us")
        .context("missing localization_fsd_en_us family")?;
    let mut english = HashMap::with_capacity(english_rows.len());
    for row in english_rows {
        let key = row.get("_key").and_then(Value::as_u64)
            .context("invalid English localization message key")?;
        let text = row.get("text").and_then(Value::as_str)
            .with_context(|| format!("invalid English text for message {key}"))?;
        if english.insert(key, text).is_some() {
            bail!("duplicate English localization message {key}");
        }
    }

    for family in ["marketgroups", "metagroups"] {
        for row in native.get(family).into_iter().flatten() {
            let object = row.as_object().with_context(|| format!("{family} row is not an object"))?;
            resolve(object, &english, family, "nameID", true)?;
            resolve(object, &english, family, "descriptionID", false)?;
        }
    }
    let mut compat = FamilyRows::new();
    for family in super::families::FAMILIES.iter().filter(|f| f.compat_table.is_some()) {
        let rows = native.get(family.name).with_context(|| format!("missing {} family", family.name))?;
        compat.insert(family.name.to_owned(), rows.clone());
    }
    for (family, rows) in &mut compat {
        for row in rows {
            let object = row.as_object_mut()
                .with_context(|| format!("{family} row is not an object"))?;
            match family.as_str() {
                "types" => {
                    localized(object, &english, family, "typeNameID", "name", true)?;
                    localized(object, &english, family, "descriptionID", "description", false)?;
                }
                "groups" => localized(object, &english, family, "groupNameID", "name", true)?,
                "categories" => localized(object, &english, family, "categoryNameID", "name", true)?,
                "dogmaattributes" => {
                    rename(object, "categoryID", "attributeCategoryID");
                    localized(object, &english, family, "displayNameID", "displayName", false)?;
                }
                "dogmaeffects" => {
                    rename(object, "effectName", "name");
                    rename(object, "effectCategory", "effectCategoryID");
                }
                _ => {}
            }
        }
    }
    Ok(compat)
}

// Source fields stay present alongside their compatibility aliases. In
// particular, null, zero, false and ordered arrays are not normalized away.
fn rename(row: &mut Map<String, Value>, source: &str, target: &str) {
    if let Some(value) = row.get(source).cloned() {
        row.insert(target.to_owned(), value);
    }
}

fn localized(
    row: &mut Map<String, Value>, english: &HashMap<u64, &str>, family: &str,
    source: &str, target: &str, required: bool,
) -> Result<()> {
    if let Some(text) = resolve(row, english, family, source, required)? {
        row.insert(target.to_owned(), text.map_or(Value::Null, |s| Value::String(s.to_owned())));
    }
    Ok(())
}

fn resolve<'a>(row: &Map<String, Value>, english: &HashMap<u64, &'a str>, family: &str, source: &str, required: bool) -> Result<Option<Option<&'a str>>> {
    let Some(id) = row.get(source) else {
        if required { bail!("{family} {:?} missing required {source}", row.get("_key")); }
        return Ok(None);
    };
    if id.is_null() && !required { return Ok(Some(None)); }
    let id = id.as_u64().with_context(|| format!("{family} {:?} invalid {source}", row.get("_key")))?;
    let text = english.get(&id).with_context(|| format!("{family} {:?} {source} message {id} missing English text", row.get("_key")))?;
    Ok(Some(Some(*text)))
}

#[cfg(test)]
mod tests;
