//! Asteroid generation policy and immutable random inputs. The game client consumes
//! the resulting balls; field composition comes from SDE types and server configuration.
//! Dynamic asteroid rows are persisted by the parent module; no client formulas are inferred.
use super::*;
use infinity_core::consts::CATEGORY_ASTEROID;
use crate::services::{rand01, rand_dir};

/// Inherited server field composition security thresholds.
const LOW_SECURITY: f64 = 0.45;
/// Inherited server ore-composition security threshold.
const MID_SECURITY: f64 = 0.7;
/// Inherited server fallback for invalid or missing SDE resource volume.
const MIN_UNIT_VOLUME: f64 = 0.01;
/// Inherited server geometry policy for ore radius per cube root of volume.
const ORE_RADIUS_SCALE: f64 = 150.0;
/// Inherited server geometry policy for ice radius.
const ICE_RADIUS_BASE: f64 = 6_000.0;
/// Inherited server geometry policy for ice radius per cube root of volume.
const ICE_RADIUS_SCALE: f64 = 30.0;
/// Inherited server geometry policy for gas cloud radius.
const GAS_RADIUS_BASE: f64 = 12_000.0;
/// Inherited server geometry policy for randomized gas cloud radius.
const GAS_RADIUS_SPREAD: f64 = 10_000.0;
/// Inherited server minimum asteroid radius.
const MIN_RADIUS: f64 = 800.0;
/// Inherited server maximum asteroid radius.
const MAX_RADIUS: f64 = 25_000.0;
/// Inherited server field geometry for gas.
const GAS_SPREAD: f64 = 60_000.0;
/// Inherited server field geometry for ore and ice.
const SOLID_SPREAD: f64 = 40_000.0;
/// Inherited server minimum displacement from field center.
const MIN_DISPLACEMENT: f64 = 3_000.0;
/// Inherited server vertical compression of field geometry.
const VERTICAL_SCALE: f64 = 0.3;

pub(crate) fn field_groups(field: Field, security: f64, jspace: bool) -> (&'static [&'static str], bool) {
    match field {
        Field::Ore if jspace || security <= 0.0 => (&["Gneiss", "Dark Ochre", "Spodumain", "Crokite", "Bistot", "Arkonor", "Hemorphite", "Hedbergite"], true),
        Field::Ore if security < LOW_SECURITY => (&["Kernite", "Omber", "Jaspet", "Hemorphite", "Hedbergite", "Plagioclase"], true),
        Field::Ore if security < MID_SECURITY => (&["Veldspar", "Scordite", "Pyroxeres", "Plagioclase", "Omber", "Kernite"], true),
        Field::Ore => (&["Veldspar", "Scordite", "Pyroxeres"], true),
        Field::Ice if security >= LOW_SECURITY => (&["Clear Icicle", "Glacial Mass", "Blue Ice", "White Glaze"], false),
        Field::Ice if security > 0.0 => (&["Glare Crust", "Dark Glitter", "Gelidus", "Blue Ice", "Clear Icicle"], false),
        Field::Ice => (&["Krystallos", "Glare Crust", "Dark Glitter", "Gelidus"], false),
        Field::Gas if jspace => (&["Fullerite-C50", "Fullerite-C60", "Fullerite-C70", "Fullerite-C72", "Fullerite-C84", "Fullerite-C28", "Fullerite-C32", "Fullerite-C320", "Fullerite-C540"], false),
        Field::Gas if security >= LOW_SECURITY => (&["Amber Mykoserocin", "Golden Mykoserocin", "Viridian Mykoserocin", "Celadon Mykoserocin", "Lime Mykoserocin"], false),
        Field::Gas => (&["Amber Cytoserocin", "Golden Cytoserocin", "Viridian Cytoserocin", "Celadon Cytoserocin", "Lime Cytoserocin", "Vermillion Cytoserocin", "Azure Cytoserocin"], false),
    }
}
fn ore_candidate(name: &str, group: &str, category: Option<i32>, names: &[&str]) -> bool {
    category == Some(CATEGORY_ASTEROID) && names.contains(&group)
        && (name == group || name.strip_prefix(group).is_some_and(|suffix| suffix == " II-Grade" || suffix == " III-Grade"))
}
pub(crate) fn field_types(sde: &Sde, field: Field, security: f64, jspace: bool) -> Vec<(i32, f64)> {
    let (names, groups) = field_groups(field, security, jspace);
    sde.types.iter().filter(|t| {
        if t.published != Some(true) { return false; }
        let Some(name) = t.name.as_deref() else { return false };
        if !groups { return names.contains(&name); }
        let Some(group) = t.group_id.and_then(|id| sde.types.group(id)) else { return false };
        let Some(group_name) = group.name.as_deref() else { return false };
        ore_candidate(name, group_name, group.category_id, names)
    }).map(|t| (t.type_id, t.volume.unwrap_or(1.0).max(MIN_UNIT_VOLUME))).collect()
}
fn sample_range(range: [f64; 2], random: f64) -> f64 { range[0] + random * (range[1] - range[0]) }
fn rock_radius(field: Field, volume: f64) -> f32 {
    let radius = match field {
        Field::Ore => volume.cbrt() * ORE_RADIUS_SCALE,
        Field::Ice => ICE_RADIUS_BASE + volume.cbrt() * ICE_RADIUS_SCALE,
        Field::Gas => GAS_RADIUS_BASE + rand01() * GAS_RADIUS_SPREAD,
    };
    radius.clamp(MIN_RADIUS, MAX_RADIUS) as f32
}

#[derive(Clone)]
pub(super) struct Candidate { pub type_id: i32, pub quantity: i64, pub radius: f32, pub pos: V3 }

/// Draw all random values once, before opening a retriable transaction.
pub(crate) fn generation_inputs(sde: &Sde, center: V3, field: Field, security: f64, jspace: bool) -> GenerationInputs {
    let types = field_types(sde, field, security, jspace);
    let mut rocks = Vec::new();
    if !types.is_empty() {
        let policy = &crate::config::game().mining;
        let range = match field { Field::Ore => policy.ore_rocks, Field::Ice => policy.ice_rocks, Field::Gas => policy.gas_clouds };
        let count = range[0] + (rand01() * (range[1] - range[0]) as f64) as i64;
        for _ in 0..count {
            let (type_id, unit_volume) = types[(rand01() * types.len() as f64) as usize % types.len()];
            let volume = match field {
                Field::Ore => sample_range(policy.ore_rock_m3, rand01()),
                Field::Ice => sample_range([policy.ice_rock_units[0] as f64, policy.ice_rock_units[1] as f64], rand01()) * unit_volume,
                Field::Gas => sample_range(policy.gas_cloud_m3, rand01()),
            };
            let quantity = (volume / unit_volume).round().max(1.0) as i64;
            let radius = rock_radius(field, volume);
            let spread = if field == Field::Gas { GAS_SPREAD } else { SOLID_SPREAD };
            let v = rand_dir();
            let d = MIN_DISPLACEMENT + rand01() * spread;
            rocks.push(Candidate { type_id, quantity, radius, pos: [center[0] + v[0] * d, center[1] + v[1] * d * VERTICAL_SCALE, center[2] + v[2] * d] });
        }
    }
    GenerationInputs { rocks, center }
}


#[cfg(test)] mod tests;
