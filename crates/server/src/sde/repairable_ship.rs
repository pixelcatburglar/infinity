//! Immutable repairable-ship recovery configuration from `sde_native`
//! FSD tables, loaded once at startup and validated against typed SDE data.
//! Consumers mirror `repairableShip/fsd` and `repairableShip/common/methods.py`:
//! service-life categories and flags, repair durations, refurbished-component
//! mappings and the unrepairable exclusion lists. Each source type maps to one
//! refurbished component, several sources may share the same component, and
//! that forward mapping feeds both disassembly yield and refurbishment
//! compatibility; it is not a bijection. Component drop rates are validated
//! but not consumed; insurance, module repair pricing and client UI state are
//! outside this dataset. An empty or invalid configuration fails startup.

use std::collections::{HashMap, HashSet};
use anyhow::{anyhow, ensure, Context};
use serde_json::Value;
use infinity_core::consts::{
    CATEGORY_CHARGE, CATEGORY_DRONE, CATEGORY_MODULE, CATEGORY_SHIP, CATEGORY_SUBSYSTEM,
    FLAG_DRONE_BAY, FLAG_FIGHTER_BAY, FLAG_FITTING_FIRST, FLAG_FIXED_SLOT,
    FLAG_RIG_FIRST, FLAG_RIG_LAST, FLAG_SUBSYSTEM_FIRST, FLAG_SUBSYSTEM_SLOT3,
    GROUP_CAPSULE, GROUP_COMPRESSED_GAS, GROUP_STATION,
};
use crate::{dao::sde::repairable_ship as dao, db::Db};
use super::{compression::Compression, industry::Industry, types::Types};

/// repairableShip/common/methods.py `InnerGetRepairTime`: the fixed base added
/// to every branch. It is not a fallback for missing data.
const REPAIR_BASE_SECONDS: f64 = 5.0;
/// repairableShip/common/methods.py `NOTENTER_DELIVERY_SYSTEM_BY_TYPE`.
const DELIVERY_EXCLUDED_TYPES: [i32; 1] = [58745];

/// Types that look reprocessable or compressible to
/// `repairableShip/common/methods.py:IsIndustryItem` at startup.
pub(crate) struct Lookups {
    pub(crate) types: HashSet<i32>,
    pub(crate) groups: HashSet<i32>,
    pub(crate) categories: HashSet<i32>,
    pub(crate) fixed_materials: HashSet<i32>,
    pub(crate) build_times: HashMap<i32, f64>,
    pub(crate) compression_sources: HashSet<i32>,
    pub(crate) compression_outputs: HashSet<i32>,
}

#[cfg_attr(test, derive(Default))]
pub(crate) struct RepairableShip {
    /// Repair times in seconds from
    /// `repairableShip/common/methods.py:InnerGetRepairTime`.
    /// A non-ship, non-industry variation whose parent lacks `metaLevel` has
    /// no formula result and no table entry. Explicit zero remains evaluable;
    /// absent values never receive the fixed base alone.
    repair_seconds: HashMap<i32, f64>,
    components: HashMap<i32, i32>,
    unrepairable_categories: HashSet<i32>,
    unrepairable_groups: HashSet<i32>,
    unrepairable_types: HashSet<i32>,
    max_life: f64,
    max_reduce: f64,
    restore_per_component: f64,
    price_percentage: f64,
}

fn single(rows: &[dao::ConfigRow], table: &str) -> anyhow::Result<serde_json::Map<String, Value>> {
    ensure!(rows.len() == 1, "{table} must hold exactly one row, found {}", rows.len());
    let value: Value = serde_json::from_str(&rows[0].data).with_context(|| format!("invalid {table} JSON"))?;
    value.as_object().cloned().ok_or_else(|| anyhow!("{table} root must be an object"))
}

fn number(object: &serde_json::Map<String, Value>, key: &str, table: &str) -> anyhow::Result<f64> {
    let value = object.get(key).and_then(Value::as_f64)
        .ok_or_else(|| anyhow!("{table}: missing or non-numeric {key}"))?;
    ensure!(value.is_finite(), "{table}: {key} must be finite");
    Ok(value)
}

fn positive(object: &serde_json::Map<String, Value>, key: &str, table: &str) -> anyhow::Result<f64> {
    let value = number(object, key, table)?;
    ensure!(value > 0.0, "{table}: {key} must be positive");
    Ok(value)
}

fn integer(object: &serde_json::Map<String, Value>, key: &str, table: &str) -> anyhow::Result<i64> {
    let value = object.get(key).and_then(Value::as_i64)
        .ok_or_else(|| anyhow!("{table}: missing or non-integer {key}"))?;
    Ok(value)
}

/// `{repairDuration}` maps keyed by type or group ID, per the native tables.
fn durations(root: &serde_json::Map<String, Value>, key: &str, table: &str) -> anyhow::Result<HashMap<i32, f64>> {
    let entries = root.get(key).and_then(Value::as_object)
        .ok_or_else(|| anyhow!("{table}: missing {key} object"))?;
    let mut map = HashMap::with_capacity(entries.len());
    for (id, entry) in entries {
        let id: i32 = id.parse().with_context(|| format!("{table}: invalid {key} id {id:?}"))?;
        let entry = entry.as_object().ok_or_else(|| anyhow!("{table}: {key}.{id} must be an object"))?;
        let duration = positive(entry, "repairDuration", table)?;
        map.insert(id, duration);
    }
    Ok(map)
}

fn id_list(root: &serde_json::Map<String, Value>, key: &str, table: &str) -> anyhow::Result<HashSet<i32>> {
    let entries = root.get(key).and_then(Value::as_array)
        .ok_or_else(|| anyhow!("{table}: missing {key} array"))?;
    let mut set = HashSet::with_capacity(entries.len());
    for entry in entries {
        let id = entry.as_i64().and_then(|id| i32::try_from(id).ok())
            .ok_or_else(|| anyhow!("{table}: invalid {key} entry {entry}"))?;
        set.insert(id);
    }
    Ok(set)
}

impl RepairableShip {
    pub(crate) async fn load(db: &Db, types: &Types, industry: &Industry, compression: &Compression) -> anyhow::Result<Self> {
        let (types_rows, component_rows, unrepairable_rows, variation_rows, randomized_rows, catalogue_rows) = tokio::try_join!(
            dao::repairable_types(db), dao::refurbished_components(db), dao::unrepairable_items(db),
            dao::variations(db), dao::randomized_materials(db), dao::catalogue(db),
        )?;
        let mut build_times = HashMap::new();
        for (product, time) in industry.manufacturing_products() {
            ensure!(time.is_finite() && time >= 0.0, "manufacturing build time for {product} must be finite and non-negative");
            if let Some(previous) = build_times.insert(product, time) {
                ensure!(previous == time, "conflicting manufacturing build times for product {product}");
            }
        }
        let lookups = Lookups {
            types: variation_rows.iter().map(|row| row.type_id).collect(),
            groups: catalogue_rows.iter().filter_map(|row| row.group_id).collect(),
            categories: catalogue_rows.iter().filter_map(|row| row.category_id).collect(),
            fixed_materials: types.iter().filter(|ty| !ty.materials().is_empty()).map(|ty| ty.type_id).collect(),
            build_times,
            compression_sources: compression.pairs().map(|(source, _)| source).collect(),
            compression_outputs: compression.pairs().map(|(_, output)| output).collect(),
        };
        Self::from_rows(&types_rows, &component_rows, &unrepairable_rows, &variation_rows, &randomized_rows, &lookups)
    }

    fn from_rows(
        types_rows: &[dao::ConfigRow], component_rows: &[dao::ConfigRow], unrepairable_rows: &[dao::ConfigRow],
        variation_rows: &[dao::VariationRow], randomized_rows: &[dao::RandomizedMaterialRow], lookups: &Lookups,
    ) -> anyhow::Result<Self> {
        const TYPES_TABLE: &str = "sde_native.repairabletypesconfig";
        const COMPONENTS_TABLE: &str = "sde_native.typerefurbishedcomponentconfig";
        const UNREPAIRABLE_TABLE: &str = "sde_native.unrepairableitemsconfig";

        let root = single(types_rows, TYPES_TABLE)?;
        let group_durations = durations(&root, "repairableShipGroups", TYPES_TABLE)?;
        for id in group_durations.keys() {
            ensure!(lookups.groups.contains(id), "{TYPES_TABLE}: unknown repairableShipGroups id {id}");
        }
        let type_durations = durations(&root, "repairableShipTypes", TYPES_TABLE)?;
        for id in type_durations.keys() {
            ensure!(lookups.types.contains(id), "{TYPES_TABLE}: unknown repairableShipTypes id {id}");
        }
        let defaults = root.get("defaultRepairableConfig").and_then(Value::as_object)
            .ok_or_else(|| anyhow!("{TYPES_TABLE}: missing defaultRepairableConfig"))?;
        let default_ship_repair = positive(defaults, "defaultShipRepairDuration", TYPES_TABLE)?;
        let max_life = positive(defaults, "defaultServiceLife", TYPES_TABLE)?;
        let max_reduce = positive(defaults, "defaultMaxReduceServiceLife", TYPES_TABLE)?;
        let restore_per_component = positive(defaults, "defaultRefurbishedComponentAddServiceLifeValue", TYPES_TABLE)?;
        let price_percentage = positive(defaults, "estPricePercentage", TYPES_TABLE)?;
        let manufacturing_percentage = positive(defaults, "manufacturingTimePercentage", TYPES_TABLE)?;
        positive(defaults, "repairDuration", TYPES_TABLE)?;
        let drop_chance = number(defaults, "defaultRefurbishedComponentDropChance", TYPES_TABLE)?;
        ensure!(drop_chance > 0.0 && drop_chance <= 1.0, "{TYPES_TABLE}: defaultRefurbishedComponentDropChance must be in (0, 1]");
        let drop_min = integer(defaults, "defaultRefurbishedComponentDropMinNum", TYPES_TABLE)?;
        let drop_max = integer(defaults, "defaultRefurbishedComponentDropMaxNum", TYPES_TABLE)?;
        ensure!(drop_min > 0 && drop_max >= drop_min, "{TYPES_TABLE}: invalid refurbished component drop range {drop_min}..{drop_max}");
        let root = single(component_rows, COMPONENTS_TABLE)?;
        let components_raw = root.get("typeRefurbishedComponents").and_then(Value::as_object)
            .ok_or_else(|| anyhow!("{COMPONENTS_TABLE}: missing typeRefurbishedComponents object"))?;
        ensure!(!components_raw.is_empty(), "{COMPONENTS_TABLE}: typeRefurbishedComponents is empty");
        let mut components = HashMap::with_capacity(components_raw.len());
        for (id, component) in components_raw {
            let source: i32 = id.parse().with_context(|| format!("{COMPONENTS_TABLE}: invalid source id {id:?}"))?;
            let component = component.as_i64().and_then(|id| i32::try_from(id).ok())
                .ok_or_else(|| anyhow!("{COMPONENTS_TABLE}: invalid component id for {id}"))?;
            ensure!(source != component && lookups.types.contains(&source) && lookups.types.contains(&component),
                "{COMPONENTS_TABLE}: invalid mapping {source} -> {component}");
            ensure!(components.insert(source, component).is_none(), "{COMPONENTS_TABLE}: duplicate source {source}");
        }

        let root = single(unrepairable_rows, UNREPAIRABLE_TABLE)?;
        let unrepairable_categories = id_list(&root, "unrepairableCategories", UNREPAIRABLE_TABLE)?;
        for id in &unrepairable_categories {
            ensure!(lookups.categories.contains(id), "{UNREPAIRABLE_TABLE}: unknown unrepairableCategories id {id}");
        }
        let unrepairable_groups = id_list(&root, "unrepairableGroups", UNREPAIRABLE_TABLE)?;
        for id in &unrepairable_groups {
            ensure!(lookups.groups.contains(id), "{UNREPAIRABLE_TABLE}: unknown unrepairableGroups id {id}");
        }
        let unrepairable_types = id_list(&root, "unrepairableTypes", UNREPAIRABLE_TABLE)?;
        for id in &unrepairable_types {
            ensure!(lookups.types.contains(id), "{UNREPAIRABLE_TABLE}: unknown unrepairableTypes id {id}");
        }

        let randomized: HashSet<i32> = randomized_rows.iter().map(|row| row.type_id).collect();
        let meta_levels: HashMap<i32, f64> = variation_rows.iter()
            .filter_map(|row| row.meta_level.map(|meta| (row.type_id, meta))).collect();
        for (id, meta) in &meta_levels {
            ensure!(meta.is_finite() && *meta >= 0.0, "invalid metaLevel {meta} for type {id}");
        }
        let industry_item = |type_id: i32, group_id: Option<i32>| -> bool {
            if group_id == Some(GROUP_STATION as i32) { return false; }
            let fixed = lookups.fixed_materials.contains(&type_id);
            if (fixed || randomized.contains(&type_id))
                && !(fixed && group_id == Some(GROUP_COMPRESSED_GAS)) {
                return true;
            }
            lookups.compression_sources.contains(&type_id) || lookups.compression_outputs.contains(&type_id)
        };
        let mut repair_seconds = HashMap::with_capacity(variation_rows.len());
        for row in variation_rows {
            let extra = if row.category_id == Some(CATEGORY_SHIP) {
                row.group_id.and_then(|group| group_durations.get(&group))
                    .or_else(|| type_durations.get(&row.type_id))
                    .copied().unwrap_or(default_ship_repair)
            } else if industry_item(row.type_id, row.group_id) {
                lookups.build_times.get(&row.type_id).copied().unwrap_or(0.0) * manufacturing_percentage
            } else if let Some(parent) = row.parent {
                let Some(meta) = meta_levels.get(&parent) else { continue; };
                lookups.build_times.get(&parent).copied().unwrap_or(0.0) * manufacturing_percentage * meta * meta
            } else {
                0.0
            };
            ensure!(extra.is_finite() && extra >= 0.0, "invalid repair time contribution for type {}", row.type_id);
            repair_seconds.insert(row.type_id, REPAIR_BASE_SECONDS + extra);
        }

        Ok(Self {
            repair_seconds, components,
            unrepairable_categories, unrepairable_groups, unrepairable_types,
            max_life, max_reduce, restore_per_component, price_percentage,
        })
    }

    /// The precomputed repair time, when available.
    pub(crate) fn repair_seconds(&self, type_id: i32) -> Option<f64> {
        self.repair_seconds.get(&type_id).copied()
    }

    /// `GetTypeRefurbishedComponent`: the refurbished component a source type
    /// produces. Several sources may map to the same component; this forward
    /// mapping decides both disassembly yield and refurbishment compatibility.
    pub(crate) fn component(&self, source_type: i32) -> Option<i32> {
        self.components.get(&source_type).copied()
    }

    /// `defaultRepairableConfig.defaultServiceLife`.
    pub(crate) fn max_life(&self) -> f64 { self.max_life }
    /// `defaultRepairableConfig.defaultMaxReduceServiceLife`.
    pub(crate) fn max_reduce(&self) -> f64 { self.max_reduce }
    /// `defaultRepairableConfig.defaultRefurbishedComponentAddServiceLifeValue`.
    pub(crate) fn restore_per_component(&self) -> f64 { self.restore_per_component }
    /// `defaultRepairableConfig.estPricePercentage`.
    pub(crate) fn price_percentage(&self) -> f64 { self.price_percentage }

    /// `_IsHaveServiceLife`: module, drone, subsystem and non-capsule ship
    /// singletons carry service life; charges never do.
    pub(crate) fn has_service_life(&self, group_id: i32, category_id: i32) -> bool {
        category_id != CATEGORY_CHARGE
            && (category_id == CATEGORY_MODULE || category_id == CATEGORY_DRONE
                || category_id == CATEGORY_SUBSYSTEM
                || (category_id == CATEGORY_SHIP && group_id != GROUP_CAPSULE))
    }

    /// `IsInRepairSystemByFlag`: fitted slots, rigs, subsystem slots and the
    /// fighter and drone bays enter the recovery system.
    pub(crate) fn repair_flag(&self, flag: i32) -> bool {
        (FLAG_FITTING_FIRST..=FLAG_FIXED_SLOT).contains(&flag)
            || (FLAG_RIG_FIRST..=FLAG_RIG_LAST).contains(&flag)
            || (FLAG_SUBSYSTEM_FIRST..=FLAG_SUBSYSTEM_SLOT3).contains(&flag)
            || flag == FLAG_FIGHTER_BAY || flag == FLAG_DRONE_BAY
    }

    /// `IsReduceServiceLifeByFlag`: the same flags minus the drone bay.
    pub(crate) fn reduce_flag(&self, flag: i32) -> bool {
        (FLAG_FITTING_FIRST..=FLAG_FIXED_SLOT).contains(&flag)
            || (FLAG_RIG_FIRST..=FLAG_RIG_LAST).contains(&flag)
            || (FLAG_SUBSYSTEM_FIRST..=FLAG_SUBSYSTEM_SLOT3).contains(&flag)
            || flag == FLAG_FIGHTER_BAY
    }

    /// `IsInUnableRepairSystemTable`: category, group or type exclusions.
    pub(crate) fn unrepairable(&self, category_id: i32, group_id: i32, type_id: i32) -> bool {
        self.unrepairable_categories.contains(&category_id)
            || self.unrepairable_groups.contains(&group_id)
            || self.unrepairable_types.contains(&type_id)
    }

    /// `IsInRepairSystemByItem` minus `NOTENTER_DELIVERY_SYSTEM_BY_TYPE`; the
    /// client also requires a non-NPC owner, which is a runtime check.
    pub(crate) fn enters_recovery(&self, type_id: i32, group_id: i32, category_id: i32) -> bool {
        self.has_service_life(group_id, category_id) && !DELIVERY_EXCLUDED_TYPES.contains(&type_id)
    }
}

#[cfg(all(test, feature = "tests-sde"))] mod tests;
