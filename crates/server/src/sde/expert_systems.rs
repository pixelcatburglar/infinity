//! Native expert-system definitions imported from `sde_native.expertsystems`.
//! This mirrors `expertSystems/client/service.py` and the static definitions used
//! by `expertSystems/client/ui/character_sheet/home/controller.py`. The imported
//! native rows are authoritative; exact fsdbinary comparison with client resources
//! is unavailable. Invalid JSON, IDs, flags and references fail startup rather than
//! dropping a definition. Lifecycle and acquisition behavior are not implemented here.
use std::collections::{BTreeMap, HashSet};

use anyhow::{anyhow, bail, Result};
use serde_json::Value;

use crate::{client::filetime_ticks_from_hours, consts::{dogma::MAX_SKILL_LEVEL, CATEGORY_SHIP, CATEGORY_SKILL, FILETIME_DAY}, dao::sde::expert_systems::ExpertSystemRow, db::Db};
use super::types::Types;

/// Client `inventorycommon/const.py:357` expert-system inventory category.
const CATEGORY_EXPERT_SYSTEM: i32 = 2100;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ExpertSystem {
    pub(crate) type_id: i32,
    pub(crate) duration_days: i32,
    pub(crate) duration_filetime: i64,
    pub(crate) skills: Vec<(i32, i32)>,
    pub(crate) associated_ship_types: Vec<i32>,
    pub(crate) hidden: bool,
    pub(crate) retired: bool,
}

#[derive(Default)]
pub(crate) struct ExpertSystems {
    definitions: BTreeMap<i32, ExpertSystem>,
}

impl ExpertSystems {
    pub(crate) async fn load(db: &Db, types: &Types) -> Result<Self> {
        Self::from_rows(
            crate::dao::sde::expert_systems::load(db).await?,
            |type_id| types.get(type_id).and_then(|ty| ty.category_id),
        )
    }

    fn from_rows(rows: Vec<ExpertSystemRow>, category_of: impl Fn(i32) -> Option<i32>) -> Result<Self> {
        let mut definitions = BTreeMap::new();
        for row in rows {
            let definition = parse_row(&row.key, &row.data, &category_of)?;
            if definitions.insert(definition.type_id, definition).is_some() {
                bail!("duplicate expert-system type ID")
            }
        }
        Ok(Self { definitions })
    }

    pub(crate) fn get(&self, type_id: i32) -> Option<&ExpertSystem> {
        self.definitions.get(&type_id)
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = &ExpertSystem> {
        self.definitions.values()
    }
}

fn positive_id(value: &Value) -> Option<i32> {
    value.as_i64().and_then(|value| i32::try_from(value).ok()).filter(|value| *value > 0)
}

fn flag(value: &Value) -> Option<bool> {
    value.as_bool().or_else(|| match value.as_i64() {
        Some(0) => Some(false),
        Some(1) => Some(true),
        _ => None,
    })
}

fn parse_row(key: &Option<String>, data: &str, category_of: &impl Fn(i32) -> Option<i32>) -> Result<ExpertSystem> {
    let value: Value = serde_json::from_str(data).map_err(|error| anyhow!("invalid expert-system JSON: {error}"))?;
    let object = value.as_object().ok_or_else(|| anyhow!("expert-system data must be an object"))?;
    let type_id = object.get("_key").and_then(positive_id).ok_or_else(|| anyhow!("expert-system _key must be a positive integer"))?;
    let row_key = key.as_deref().ok_or_else(|| anyhow!("expert-system row key is missing"))?.parse::<i32>()?;
    if row_key != type_id { bail!("expert-system row key does not match _key {type_id}") }
    match category_of(type_id) {
        Some(CATEGORY_EXPERT_SYSTEM) => {}
        _ => bail!("expert-system {type_id} is missing category {CATEGORY_EXPERT_SYSTEM}"),
    }

    let days = object.get("durationDays").and_then(Value::as_f64).filter(|days| days.is_finite() && *days > 0.0 && days.fract() == 0.0).ok_or_else(|| anyhow!("expert-system {type_id} durationDays must be a positive integer"))?;
    let duration_days = i32::try_from(days as i64).map_err(|_| anyhow!("expert-system {type_id} durationDays is out of range"))?;
    let duration_filetime = filetime_ticks_from_hours(days * (FILETIME_DAY / crate::consts::FILETIME_HOUR) as f64).ok_or_else(|| anyhow!("expert-system {type_id} duration overflows FILETIME"))?;

    let grants = object.get("skillsGranted").and_then(Value::as_object).ok_or_else(|| anyhow!("expert-system {type_id} skillsGranted must be an object"))?;
    let mut skills = Vec::with_capacity(grants.len());
    let mut skill_ids = HashSet::with_capacity(grants.len());
    for (skill_key, level) in grants {
        let skill_id = skill_key.parse::<i32>().ok().filter(|id| *id > 0).ok_or_else(|| anyhow!("expert-system {type_id} has invalid skill ID"))?;
        if !skill_ids.insert(skill_id) {
            bail!("expert-system {type_id} has duplicate skill ID {skill_id}");
        }
        let level = positive_id(level).filter(|level| *level <= MAX_SKILL_LEVEL).ok_or_else(|| anyhow!("expert-system {type_id} has invalid skill level"))?;
        if category_of(skill_id) != Some(CATEGORY_SKILL) { bail!("expert-system {type_id} references missing skill {skill_id}") }
        skills.push((skill_id, level));
    }
    skills.sort_unstable_by_key(|(skill_id, _)| *skill_id);

    let mut associated_ship_types = Vec::new();
    if let Some(ships) = object.get("associatedShipTypes") {
        let ships = ships.as_array().ok_or_else(|| anyhow!("expert-system {type_id} associatedShipTypes must be an array"))?;
        associated_ship_types.reserve(ships.len());
        for ship in ships {
            let ship_id = positive_id(ship).ok_or_else(|| anyhow!("expert-system {type_id} has invalid ship ID"))?;
            if category_of(ship_id) != Some(CATEGORY_SHIP) { bail!("expert-system {type_id} references missing ship {ship_id}") }
            associated_ship_types.push(ship_id);
        }
        associated_ship_types.sort_unstable();
        if associated_ship_types.windows(2).any(|pair| pair[0] == pair[1]) { bail!("expert-system {type_id} has duplicate associated ship IDs") }
    }
    let hidden = object.get("esHidden").and_then(flag).ok_or_else(|| anyhow!("expert-system {type_id} esHidden must be bool or 0/1"))?;
    let retired = object.get("esRetired").and_then(flag).ok_or_else(|| anyhow!("expert-system {type_id} esRetired must be bool or 0/1"))?;
    Ok(ExpertSystem { type_id, duration_days, duration_filetime, skills, associated_ship_types, hidden, retired })
}
#[cfg(test)]
mod tests;
