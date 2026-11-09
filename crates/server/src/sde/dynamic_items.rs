//! Startup mutaplasmid ranges and source-to-result mappings from native `dynamicitemattributes`.
//! The client loads static data through `dynamicitemattributes/loader.py` and uses
//! `eve/client/script/dogma/dynamicItemSvc.py`. No client mutation algorithm is mirrored here.
//! Extra-input variants, malformed rows and references absent from loaded SDE are unavailable.
//! `highIsGood` is not loaded: the server samples uniformly and the client reads polarity from its own static data.
use std::collections::{BTreeMap, HashMap, HashSet};

use serde_json::Value;
use crate::dao::sde::dynamic_items::DynamicItemRow;
use super::{dogma::Dogma, types::Types};

pub(crate) struct AttributeRange {
    pub(crate) min: f64,
    pub(crate) max: f64,
}

pub(crate) struct Mutation {
    pub(crate) mappings: BTreeMap<i32, i32>,
    pub(crate) attributes: BTreeMap<i32, AttributeRange>,
}

impl Mutation {
    pub(crate) fn result_type(&self, source: i32) -> Option<i32> { self.mappings.get(&source).copied() }
}

#[cfg_attr(test, derive(Default))]
pub(crate) struct DynamicItems(HashMap<i32, Mutation>);

#[derive(Debug, PartialEq, Eq)]
enum Exclusion { ExtraInput, Malformed }

fn positive_id(value: &Value) -> Option<i32> {
    value.as_i64().and_then(|n| i32::try_from(n).ok()).filter(|&n| n > 0)
}

fn flag(value: &Value) -> Option<bool> {
    value.as_bool().or_else(|| match value.as_i64() { Some(0) => Some(false), Some(1) => Some(true), _ => None })
}

fn parse_row(key: Option<&str>, data: &str) -> Result<(i32, Mutation), Exclusion> {
    let bad = || Exclusion::Malformed;
    let value: Value = serde_json::from_str(data).map_err(|_| bad())?;
    let object = value.as_object().ok_or_else(bad)?;
    let mutator = object.get("_key").and_then(positive_id).ok_or_else(bad)?;
    if key.and_then(|k| k.parse::<i32>().ok()) != Some(mutator) { return Err(bad()); }

    let mut extra = false;
    if let Some(input) = object.get("inputItemTypes") {
        extra |= !input.as_array().ok_or_else(bad)?.is_empty();
    }
    for name in ["badMutatorSkillTypeID", "goodMutatorSkillTypeID", "inputItemMetaLevelSum", "canEnsureGoodAttributeCount"] {
        if let Some(value) = object.get(name) {
            let number = value.as_u64().ok_or_else(bad)?;
            extra |= number != 0;
        }
    }
    if extra { return Err(Exclusion::ExtraInput); }

    let mut attributes = BTreeMap::new();
    for (key, value) in object.get("attributeIDs").and_then(Value::as_object).ok_or_else(bad)? {
        let id = key.parse::<i32>().ok().filter(|&id| id > 0).ok_or_else(bad)?;
        let range = value.as_object().ok_or_else(bad)?;
        if let Some(nested) = range.get("_key") {
            if positive_id(nested) != Some(id) { return Err(bad()); }
        }
        let min = range.get("min").and_then(Value::as_f64).filter(|n| n.is_finite() && *n > 0.0).ok_or_else(bad)?;
        let max = range.get("max").and_then(Value::as_f64).filter(|n| n.is_finite() && *n > 0.0).ok_or_else(bad)?;
        if min > max { return Err(bad()); }
        if let Some(value) = range.get("highIsGood") { flag(value).ok_or_else(bad)?; }
        if let Some(value) = range.get("canEnsureGood") { flag(value).ok_or_else(bad)?; }
        if attributes.insert(id, AttributeRange { min, max }).is_some() { return Err(bad()); }
    }
    if attributes.is_empty() { return Err(bad()); }

    let mut mappings = BTreeMap::new();
    for mapping in object.get("inputOutputMapping").and_then(Value::as_array).ok_or_else(bad)? {
        let mapping = mapping.as_object().ok_or_else(bad)?;
        let result = mapping.get("resultingType").and_then(positive_id).ok_or_else(bad)?;
        let sources = mapping.get("applicableTypes").and_then(Value::as_array).ok_or_else(bad)?;
        if sources.is_empty() { return Err(bad()); }
        for source in sources {
            let source = positive_id(source).ok_or_else(bad)?;
            if mappings.insert(source, result).is_some() { return Err(bad()); }
        }
    }
    if mappings.is_empty() { return Err(bad()); }
    Ok((mutator, Mutation { mappings, attributes }))
}

impl DynamicItems {
    pub(super) fn from_rows(rows: Vec<DynamicItemRow>, types: &Types, dogma: &Dogma) -> Self {
        Self::index(rows, |id| types.get(id).is_some(), |id| dogma.attribute(id).is_some())
    }

    fn index(rows: Vec<DynamicItemRow>, has_type: impl Fn(i32) -> bool, has_attribute: impl Fn(i32) -> bool) -> Self {
        let mut items = HashMap::new();
        let mut duplicates = HashSet::new();
        let (mut loaded, mut excluded_extra_input, mut excluded_malformed) = (0, 0, 0);
        for row in rows {
            match parse_row(row.key.as_deref(), &row.data) {
                Ok((id, mutation)) if has_type(id)
                    && mutation.mappings.iter().all(|(&source, &result)| has_type(source) && has_type(result))
                    && mutation.attributes.keys().all(|&id| has_attribute(id)) => {
                        if duplicates.contains(&id) { excluded_malformed += 1; }
                        else if items.remove(&id).is_some() {
                            duplicates.insert(id);
                            loaded -= 1;
                            excluded_malformed += 2;
                        } else {
                            items.insert(id, mutation);
                            loaded += 1;
                        }
                    }
                Err(Exclusion::ExtraInput) => excluded_extra_input += 1,
                _ => excluded_malformed += 1,
            }
        }
        tracing::info!(loaded, excluded_extra_input, excluded_malformed, "dynamic item definitions loaded");
        Self(items)
    }

    pub(crate) fn get(&self, mutator: i32) -> Option<&Mutation> { self.0.get(&mutator) }
}

#[cfg(test)]
mod tests;
