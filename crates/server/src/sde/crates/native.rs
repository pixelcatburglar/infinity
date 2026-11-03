//! Parses native crate presentation, rewards and references at startup.
//! Mirrors the client's item-pack metadata without implementing opening or rolls.
//! Input is the `sde_native.crates` JSONB catalogue; policy is loaded separately.
//! Conflicting or malformed records cannot supply preview rewards.

use std::collections::BTreeMap;

use serde_json::{Map, Value};

use crate::{dao::sde::crates::NativeRow, sde::types::Types};

pub(super) struct NativeCatalogue {
    pub definitions: Vec<NativeDefinition>,
    pub untyped_rows: Vec<String>,
}

pub(super) struct NativeDefinition {
    pub type_id: i32,
    pub source_keys: Vec<String>,
    pub splash: String,
    pub ordinary_present: bool,
    pub custom_present: bool,
    pub ordinary: Vec<NativeReward>,
    pub custom: Vec<NativeReward>,
    pub special: bool,
    pub ambiguous: bool,
    pub malformed: bool,
    pub reference_count: usize,
    pub missing_fields: usize,
    pub empty_fields: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct NativeReward {
    pub type_id: i32,
    pub quantity: i64,
    pub soulbound: bool,
}

// Native crate JSON fields that affect presentation, rewards or references.
const FIELDS: &[&str] = &[
    "lootPresentationType", "lootTableType", "staticSplash", "animatedSplash",
    "fixedLootItems", "fixedCustomLootItems", "lootTableByRace", "skillBundleID", "fittings",
];

/// Presentation modes in the native crate metadata consumed by the item-pack client.
const PRESENTATION_MODES: &[&str] = &["fixed", "hacking"];
/// Loot table selection modes in the native crate metadata.
const TABLE_MODES: &[&str] = &["byType", "byCharacterRace"];

pub(super) fn parse(rows: Vec<NativeRow>, types: &Types) -> NativeCatalogue {
    parse_validated(rows, |id| types.get(id).is_some())
}

fn parse_validated(rows: Vec<NativeRow>, exists: impl Fn(i32) -> bool) -> NativeCatalogue {
    let mut grouped: BTreeMap<i32, Vec<NativeRow>> = BTreeMap::new();
    let mut untyped_rows = Vec::new();
    for row in rows {
        match row.data.get("typeID").and_then(positive_id) {
            Some(id) => grouped.entry(id).or_default().push(row),
            None => untyped_rows.push(row.key),
        }
    }
    untyped_rows.sort();
    let definitions = grouped.into_iter().map(|(type_id, mut rows)| {
        rows.sort_by(|a, b| a.key.cmp(&b.key));
        let source_keys = rows.iter().map(|row| row.key.clone()).collect();
        let projection = semantic(&rows[0].data);
        let ambiguous = rows.iter().skip(1).any(|row| semantic(&row.data) != projection);
        let mut definition = NativeDefinition {
            type_id, source_keys, splash: String::new(), ordinary: Vec::new(), custom: Vec::new(),
            special: false, ambiguous, malformed: !exists(type_id), reference_count: 0,
            ordinary_present: false, custom_present: false, missing_fields: 0, empty_fields: 0,
        };
        if ambiguous {
            for row in &rows {
                let data = &row.data;
                definition.ordinary_present |= data.get("fixedLootItems").is_some();
                definition.custom_present |= data.get("fixedCustomLootItems").is_some();
                for field in FIELDS {
                    match data.get(*field) {
                        None => definition.missing_fields += 1,
                        Some(value) if empty(value) => definition.empty_fields += 1,
                        _ => {}
                    }
                }
                match parse_contents(data, &exists) {
                    ContentsParse { parsed: Some(parsed), .. } => {
                        definition.special |= parsed.special;
                        definition.reference_count += parsed.reference_count;
                    }
                    ContentsParse { special, reference_count, .. } => {
                        definition.special |= special;
                        definition.reference_count += reference_count;
                        definition.malformed = true;
                    }
                }
            }
            return definition;
        }
        let data = &rows[0].data;
        definition.ordinary_present = data.get("fixedLootItems").is_some();
        definition.custom_present = data.get("fixedCustomLootItems").is_some();
        for field in FIELDS {
            match data.get(*field) {
                None => definition.missing_fields += 1,
                Some(value) if empty(value) => definition.empty_fields += 1,
                _ => {}
            }
        }
        if let Some(splash) = data.pointer("/staticSplash/resPath").and_then(Value::as_str) {
            definition.splash = splash.to_owned();
        }
        match parse_contents(data, &exists) {
            ContentsParse { parsed: Some(parsed), .. } if !definition.malformed => {
                definition.ordinary = parsed.ordinary;
                definition.custom = parsed.custom;
                definition.special = parsed.special;
                definition.reference_count = parsed.reference_count;
            }
            ContentsParse { special, reference_count, .. } => {
                definition.special = special;
                definition.reference_count = reference_count;
                definition.malformed = true;
            }
        }
        definition
    }).collect();
    NativeCatalogue { definitions, untyped_rows }
}

fn empty(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::Array(items) => items.is_empty(),
        Value::Object(items) => items.is_empty(),
        Value::String(text) => text.is_empty(),
        _ => false,
    }
}

fn semantic(data: &Value) -> Map<String, Value> {
    let mut result = Map::new();
    for &field in FIELDS {
        if let Some(value) = data.get(field) {
            let mut value = value.clone();
            if matches!(field, "fixedLootItems" | "fixedCustomLootItems" | "fittings") {
                if let Value::Array(items) = &mut value {
                    items.sort_by_key(Value::to_string);
                }
            }
            result.insert(field.to_owned(), value);
        }
    }
    result
}

fn positive_id(value: &Value) -> Option<i32> {
    let id = i32::try_from(value.as_i64()?).ok()?;
    (id > 0).then_some(id)
}

struct ParsedContents {
    ordinary: Vec<NativeReward>,
    custom: Vec<NativeReward>,
    special: bool,
    reference_count: usize,
}

struct ContentsParse {
    parsed: Option<ParsedContents>,
    special: bool,
    reference_count: usize,
}

fn parse_contents(data: &Value, exists: &impl Fn(i32) -> bool) -> ContentsParse {
    let special = data.get("skillBundleID").is_some_and(|value| !empty(value))
        || data.get("fittings").is_some_and(|value| !empty(value));
    let parsed = (|| {
        for (field, allowed) in [("lootPresentationType", PRESENTATION_MODES), ("lootTableType", TABLE_MODES)] {
            if let Some(value) = data.get(field).filter(|value| !value.is_null()) {
                if !value.as_str().is_some_and(|mode| allowed.contains(&mode)) {
                    return None;
                }
            }
        }
        let mut ordinary = Vec::new();
        let mut custom = Vec::new();
        for item in array_field(data, "fixedLootItems")? {
            let type_id = positive_id(item)?;
            if !exists(type_id) { return None; }
            ordinary.push(NativeReward { type_id, quantity: 1, soulbound: false });
        }
        for item in array_field(data, "fixedCustomLootItems")? {
            let type_id = positive_id(item.get("loottypeid")?)?;
            let quantity = item.get("lootcount")?.as_i64()?;
            if !exists(type_id) || quantity <= 0 || quantity > i64::from(i32::MAX) { return None; }
            let soulbound = match item.get("lootsoulbound")? {
                Value::Bool(value) => *value,
                value if value.as_i64() == Some(0) => false,
                value if value.as_i64() == Some(1) => true,
                _ => return None,
            };
            custom.push(NativeReward { type_id, quantity, soulbound });
        }
        ordinary.sort();
        custom.sort();
        let mut references = 0;
        if let Some(value) = data.get("lootTableByRace").filter(|v| !v.is_null()) {
            for value in value.as_object()?.values() {
                positive_id(value)?;
                references += 1;
            }
        }
        let bundle = match data.get("skillBundleID") {
            None | Some(Value::Null) => false,
            Some(value) if empty(value) => false,
            Some(value) => { positive_id(value)?; references += 1; true }
        };
        let fittings = array_field(data, "fittings")?;
        for fitting in fittings {
            positive_id(fitting)?;
            references += 1;
        }
        Some(ParsedContents { ordinary, custom, special: bundle || !fittings.is_empty(), reference_count: references })
    })();
    let reference_count = parsed.as_ref().map_or(0, |parsed| parsed.reference_count);
    ContentsParse { parsed, special, reference_count }
}

fn array_field<'a>(data: &'a Value, field: &str) -> Option<&'a [Value]> {
    match data.get(field) {
        None | Some(Value::Null) => Some(&[]),
        Some(value) => value.as_array().map(Vec::as_slice),
    }
}

#[cfg(test)]
mod tests;
