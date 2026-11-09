//! Ship hold capacity and item admission, mirroring `inventoryFlagsCommon.py`
//! and `invCache.py:1310-1318`. Rules use SDE type classifications and fitted
//! dogma; structure-only bays, permissions and persistence are outside this module.

use infinity_core::{consts::*, consts::dogma::*, messages};
use crate::sde::Sde;
use crate::services::{CallError, fit::{Fit, Key}, item_volume::{item_volume, stack_size}, user_error};
use marshal::Value;

pub(crate) struct Hold {
    pub(crate) flag: i32,
    pub(crate) attribute: i32,
    categories: &'static [i32],
    groups: &'static [i32],
    blocked_groups: &'static [i32],
    types: &'static [i32],
    blocked_types: &'static [i32],
    full: &'static str,
}

static HOLDS: &[Hold] = &[
    // inventoryFlagsCommon.py:9-15
    Hold { flag: FLAG_CARGO, attribute: ATTR_CAPACITY, categories: &[], groups: &[], blocked_groups: &[], types: &[], blocked_types: &[], full: messages::MSG_NOT_ENOUGH_CARGO_SPACE },
    // inventoryFlagsCommon.py:16-23
    Hold { flag: FLAG_DRONE_BAY, attribute: ATTR_DRONE_CAPACITY, categories: &[CATEGORY_DRONE], groups: &[], blocked_groups: &[], types: &[], blocked_types: &[], full: messages::MSG_NOT_ENOUGH_DRONE_BAY_SPACE },
    // inventoryFlagsCommon.py:24-31
    Hold { flag: FLAG_FIGHTER_BAY, attribute: ATTR_FIGHTER_CAPACITY, categories: &[CATEGORY_FIGHTER], groups: &[], blocked_groups: &[], types: &[], blocked_types: &[], full: messages::MSG_NOT_ENOUGH_FIGHTER_BAY_SPACE },
    // inventoryFlagsCommon.py:32-43
    Hold { flag: FLAG_FRIGATE_ESCAPE_BAY, attribute: ATTR_FRIGATE_ESCAPE_BAY_CAPACITY, categories: &[], groups: &[GROUP_FRIGATE, GROUP_ASSAULT_FRIGATE, GROUP_LOGISTICS_FRIGATE, GROUP_ELECTRONIC_ATTACK_SHIPS], blocked_groups: &[GROUP_CAPSULE], types: &[], blocked_types: &[], full: messages::MSG_NOT_ENOUGH_SPECIAL_BAY_SPACE },
    // inventoryFlagsCommon.py:44-52
    Hold { flag: FLAG_SHIP_HANGAR, attribute: ATTR_SHIP_MAINTENANCE_BAY_CAPACITY, categories: &[CATEGORY_SHIP], groups: &[], blocked_groups: &[GROUP_CAPSULE], types: &[], blocked_types: &[], full: messages::MSG_NOT_ENOUGH_SPECIAL_BAY_SPACE },
    // inventoryFlagsCommon.py:53-59
    Hold { flag: FLAG_FLEET_HANGAR, attribute: ATTR_FLEET_HANGAR_CAPACITY, categories: &[], groups: &[], blocked_groups: &[], types: &[], blocked_types: &[], full: messages::MSG_NOT_ENOUGH_SPECIAL_BAY_SPACE },
    // inventoryFlagsCommon.py:60-68
    Hold { flag: FLAG_SPECIALIZED_FUEL_BAY, attribute: ATTR_SPECIAL_FUEL_BAY_CAPACITY, categories: &[], groups: &[GROUP_ICE_PRODUCT], blocked_groups: &[], types: &[], blocked_types: &[], full: messages::MSG_NOT_ENOUGH_SPECIAL_BAY_SPACE },
    // inventoryFlagsCommon.py:69-83
    Hold { flag: FLAG_GENERAL_MINING_HOLD, attribute: ATTR_GENERAL_MINING_HOLD_CAPACITY, categories: &[CATEGORY_ASTEROID], groups: &[GROUP_HARVESTABLE_CLOUD, GROUP_COMPRESSED_GAS, GROUP_PRISMATICITE], blocked_groups: &[GROUP_SCALABLE_DECORATIVE_ASTEROID, GROUP_TERRAN_ARTIFACTS, GROUP_NON_INTERACTABLE_ASTEROID], types: &[], blocked_types: &[], full: messages::MSG_NOT_ENOUGH_SPECIAL_BAY_SPACE },
    // inventoryFlagsCommon.py:84-99
    Hold { flag: FLAG_SPECIAL_ASTEROID_HOLD, attribute: ATTR_SPECIAL_ASTEROID_HOLD_CAPACITY, categories: &[CATEGORY_ASTEROID], groups: &[GROUP_PRISMATICITE], blocked_groups: &[GROUP_SCALABLE_DECORATIVE_ASTEROID, GROUP_TERRAN_ARTIFACTS, GROUP_ANCIENT_COMPRESSED_ICE, GROUP_TEMPORAL_RESOURCES, GROUP_ICE, GROUP_NON_INTERACTABLE_ASTEROID], types: &[], blocked_types: &[], full: messages::MSG_NOT_ENOUGH_SPECIAL_BAY_SPACE },
    // inventoryFlagsCommon.py:100-110
    Hold { flag: FLAG_SPECIALIZED_ICE_HOLD, attribute: ATTR_SPECIAL_ICE_HOLD_CAPACITY, categories: &[], groups: &[GROUP_ICE, GROUP_TEMPORAL_RESOURCES, GROUP_ANCIENT_COMPRESSED_ICE], blocked_groups: &[], types: &[], blocked_types: &[], full: messages::MSG_NOT_ENOUGH_SPECIAL_BAY_SPACE },
    // inventoryFlagsCommon.py:111-120
    Hold { flag: FLAG_SPECIALIZED_GAS_HOLD, attribute: ATTR_SPECIAL_GAS_HOLD_CAPACITY, categories: &[], groups: &[GROUP_HARVESTABLE_CLOUD, GROUP_COMPRESSED_GAS], blocked_groups: &[], types: &[], blocked_types: &[], full: messages::MSG_NOT_ENOUGH_SPECIAL_BAY_SPACE },
    // inventoryFlagsCommon.py:121-130
    Hold { flag: FLAG_SPECIALIZED_MINERAL_HOLD, attribute: ATTR_SPECIAL_MINERAL_HOLD_CAPACITY, categories: &[], groups: &[GROUP_MINERAL, GROUP_UNREFINED_MINERALS], blocked_groups: &[], types: &[], blocked_types: &[], full: messages::MSG_NOT_ENOUGH_SPECIAL_BAY_SPACE },
    // inventoryFlagsCommon.py:131-141
    Hold { flag: FLAG_SPECIALIZED_SALVAGE_HOLD, attribute: ATTR_SPECIAL_SALVAGE_HOLD_CAPACITY, categories: &[], groups: &[GROUP_ANCIENT_SALVAGE, GROUP_SALVAGED_MATERIALS, GROUP_REFINABLES], blocked_groups: &[], types: &[], blocked_types: &[], full: messages::MSG_NOT_ENOUGH_SPECIAL_BAY_SPACE },
    // inventoryFlagsCommon.py:142-150
    Hold { flag: FLAG_SPECIALIZED_SHIP_HOLD, attribute: ATTR_SPECIAL_SHIP_HOLD_CAPACITY, categories: &[CATEGORY_SHIP], groups: &[], blocked_groups: &[], types: &[], blocked_types: &[], full: messages::MSG_NOT_ENOUGH_SPECIAL_BAY_SPACE },
    // inventoryFlagsCommon.py:151-170
    Hold { flag: FLAG_SPECIALIZED_SMALL_SHIP_HOLD, attribute: ATTR_SPECIAL_SMALL_SHIP_HOLD_CAPACITY, categories: &[], groups: &[GROUP_FRIGATE, GROUP_ASSAULT_FRIGATE, GROUP_DESTROYER, GROUP_INTERDICTOR, GROUP_INTERCEPTOR, GROUP_COVERT_OPS, GROUP_ELECTRONIC_ATTACK_SHIPS, GROUP_STEALTH_BOMBER, GROUP_EXPEDITION_FRIGATE, GROUP_TACTICAL_DESTROYER, GROUP_LOGISTICS_FRIGATE, GROUP_COMMAND_DESTROYER], blocked_groups: &[], types: &[], blocked_types: &[], full: messages::MSG_NOT_ENOUGH_SPECIAL_BAY_SPACE },
    // inventoryFlagsCommon.py:171-191
    Hold { flag: FLAG_SPECIALIZED_MEDIUM_SHIP_HOLD, attribute: ATTR_SPECIAL_MEDIUM_SHIP_HOLD_CAPACITY, categories: &[], groups: &[GROUP_CRUISER, GROUP_COMBAT_RECON_SHIP, GROUP_COMMAND_SHIP, GROUP_HEAVY_ASSAULT_CRUISER, GROUP_HEAVY_INTERDICTORS, GROUP_LOGISTICS, GROUP_STRATEGIC_CRUISER as i32, GROUP_BATTLECRUISER, GROUP_ATTACK_BATTLECRUISER, GROUP_FORCE_RECON_SHIP, GROUP_FLAG_CRUISER, GROUP_EXPEDITION_COMMAND_SHIP, GROUP_SPECIAL_EDITION_YACHT], blocked_groups: &[], types: &[], blocked_types: &[], full: messages::MSG_NOT_ENOUGH_SPECIAL_BAY_SPACE },
    // inventoryFlagsCommon.py:192-202
    Hold { flag: FLAG_SPECIALIZED_LARGE_SHIP_HOLD, attribute: ATTR_SPECIAL_LARGE_SHIP_HOLD_CAPACITY, categories: &[], groups: &[GROUP_BATTLESHIP, GROUP_BLACK_OPS, GROUP_MARAUDERS], blocked_groups: &[], types: &[], blocked_types: &[], full: messages::MSG_NOT_ENOUGH_SPECIAL_BAY_SPACE },
    // inventoryFlagsCommon.py:203-216
    Hold { flag: FLAG_SPECIALIZED_INDUSTRIAL_SHIP_HOLD, attribute: ATTR_SPECIAL_INDUSTRIAL_SHIP_HOLD_CAPACITY, categories: &[], groups: &[GROUP_BLOCKADE_RUNNER, GROUP_EXHUMER, GROUP_INDUSTRIAL, GROUP_MINING_BARGE, GROUP_TRANSPORT_SHIP, GROUP_EXPEDITION_FRIGATE], blocked_groups: &[], types: &[], blocked_types: &[], full: messages::MSG_NOT_ENOUGH_SPECIAL_BAY_SPACE },
    // inventoryFlagsCommon.py:217-226
    Hold { flag: FLAG_SPECIALIZED_AMMO_HOLD, attribute: ATTR_SPECIAL_AMMO_HOLD_CAPACITY, categories: &[CATEGORY_CHARGE], groups: &[], blocked_groups: &[], types: &[TYPE_NANITE_REPAIR_PASTE], blocked_types: &[], full: messages::MSG_NOT_ENOUGH_SPECIAL_BAY_SPACE },
    // inventoryFlagsCommon.py:227-235
    Hold { flag: FLAG_SPECIALIZED_COMMAND_CENTER_HOLD, attribute: ATTR_SPECIAL_COMMAND_CENTER_HOLD_CAPACITY, categories: &[], groups: &[GROUP_COMMAND_PINS], blocked_groups: &[], types: &[], blocked_types: &[], full: messages::MSG_NOT_ENOUGH_SPECIAL_BAY_SPACE },
    // inventoryFlagsCommon.py:236-247
    Hold { flag: FLAG_SPECIALIZED_PLANETARY_COMMODITIES_HOLD, attribute: ATTR_SPECIAL_PLANETARY_COMMODITIES_HOLD_CAPACITY, categories: &[CATEGORY_PLANETARY_COMMODITIES, CATEGORY_PLANETARY_RESOURCES], groups: &[], blocked_groups: &[], types: &[TYPE_WATER, TYPE_OXYGEN], blocked_types: &[], full: messages::MSG_NOT_ENOUGH_SPECIAL_BAY_SPACE },
    // inventoryFlagsCommon.py:248-258
    Hold { flag: FLAG_SPECIALIZED_MATERIAL_BAY, attribute: ATTR_SPECIAL_MATERIAL_BAY_CAPACITY, categories: &[CATEGORY_PLANETARY_COMMODITIES, CATEGORY_COMMODITY, CATEGORY_MATERIAL], groups: &[], blocked_groups: &[], types: &[], blocked_types: &[], full: messages::MSG_NOT_ENOUGH_SPECIAL_BAY_SPACE },
    // inventoryFlagsCommon.py:259-273
    Hold { flag: FLAG_QUAFE_BAY, attribute: ATTR_SPECIAL_QUAFE_HOLD_CAPACITY, categories: &[], groups: &[], blocked_groups: &[], types: &[TYPE_LARGE_CRATES_OF_QUAFE, TYPE_QUAFE, TYPE_QUAFE_ULTRA, TYPE_QUAFE_ULTRA_SPECIAL_EDITION, TYPE_QUAFE_ZERO, TYPE_SPIKED_QUAFE, TYPE_HYPER_QUAFE], blocked_types: &[], full: messages::MSG_NOT_ENOUGH_SPECIAL_BAY_SPACE },
    // inventoryFlagsCommon.py:274-283
    Hold { flag: FLAG_CORPSE_BAY, attribute: ATTR_SPECIAL_CORPSE_HOLD_CAPACITY, categories: &[], groups: &[], blocked_groups: &[], types: &[TYPE_CORPSE, TYPE_CORPSE_FEMALE], blocked_types: &[], full: messages::MSG_NOT_ENOUGH_SPECIAL_BAY_SPACE },
    // inventoryFlagsCommon.py:284-294
    Hold { flag: FLAG_BOOSTER_BAY, attribute: ATTR_SPECIAL_BOOSTER_HOLD_CAPACITY, categories: &[], groups: &[GROUP_BOOSTER, GROUP_DRUG, GROUP_BIOCHEMICAL_MATERIAL], blocked_groups: &[], types: &[], blocked_types: &[], full: messages::MSG_NOT_ENOUGH_SPECIAL_BAY_SPACE },
    // inventoryFlagsCommon.py:295-303
    Hold { flag: FLAG_SUBSYSTEM_BAY, attribute: ATTR_SPECIAL_SUBSYSTEM_HOLD_CAPACITY, categories: &[CATEGORY_SUBSYSTEM], groups: &[], blocked_groups: &[], types: &[], blocked_types: &[], full: messages::MSG_NOT_ENOUGH_SPECIAL_BAY_SPACE },
    // inventoryFlagsCommon.py:304-312
    Hold { flag: FLAG_MOBILE_DEPOT_HOLD, attribute: ATTR_SPECIAL_MOBILE_DEPOT_HOLD_CAPACITY, categories: &[], groups: &[GROUP_MOBILE_HOMES], blocked_groups: &[], types: &[], blocked_types: &[], full: messages::MSG_NOT_ENOUGH_SPECIAL_BAY_SPACE },
    // inventoryFlagsCommon.py:313-344
    Hold { flag: FLAG_COLONY_RESOURCES_HOLD, attribute: ATTR_SPECIAL_COLONY_RESOURCES_HOLD_CAPACITY, categories: &[CATEGORY_PLANETARY_COMMODITIES, CATEGORY_PLANETARY_RESOURCES, CATEGORY_INFRASTRUCTURE_UPGRADE, CATEGORY_ORBITAL, CATEGORY_SOVEREIGNTY_STRUCTURE, CATEGORY_STRUCTURE, CATEGORY_DEPLOYABLE, CATEGORY_STRUCTURE_MODULE], groups: &[GROUP_COLONY_REAGENTS, GROUP_MOON_MATERIALS, GROUP_FUEL_BLOCK, GROUP_ICE_PRODUCT, GROUP_STRUCTURE_LIGHT_FIGHTER, GROUP_STRUCTURE_SUPPORT_FIGHTER, GROUP_STRUCTURE_HEAVY_FIGHTER, GROUP_STRUCTURE_AREA_MISSILE, GROUP_STRUCTURE_FLAK_MISSILE, GROUP_STRUCTURE_MISSILE, GROUP_STRUCTURE_E_C_M_SCRIPT, GROUP_STRUCTURE_AREA_DENIAL_AMMUNITION, GROUP_COMMAND_PINS, GROUP_STRUCTURE_WARP_DISRUPTOR_SCRIPT], blocked_groups: &[], types: &[TYPE_WATER, TYPE_OXYGEN], blocked_types: &[], full: messages::MSG_NOT_ENOUGH_SPECIAL_BAY_SPACE },
    // inventoryFlagsCommon.py:355-392
    Hold { flag: FLAG_EXPEDITION_HOLD, attribute: ATTR_SPECIAL_EXPEDITION_HOLD_CAPACITY, categories: &[CATEGORY_DEPLOYABLE, CATEGORY_BLUEPRINT, CATEGORY_PERSONALIZATION, CATEGORY_SHIP_SKIN, CATEGORY_COMMODITY, CATEGORY_APPAREL, CATEGORY_SKILL, CATEGORY_SPECIAL_EDITION_ASSETS, CATEGORY_ANCIENT_RELIC], groups: &[GROUP_HARVESTABLE_CLOUD, GROUP_COMPRESSED_GAS, GROUP_DECRYPTORS, GROUP_SALVAGED_MATERIALS, GROUP_ANCIENT_SALVAGE, GROUP_BOOSTER, GROUP_FESTIVAL_MISSILE, GROUP_SCANNER_PROBE, GROUP_SURVEY_PROBE, GROUP_STRUCTURE_FESTIVAL_MISSILE, GROUP_COMMAND_BURST_CHARGE_ARMOR, GROUP_COMMAND_BURST_CHARGE_EXPEDITION, GROUP_COMMAND_BURST_CHARGE_INFO, GROUP_COMMAND_BURST_CHARGE_MINING, GROUP_COMMAND_BURST_CHARGE_SHIELD, GROUP_COMMAND_BURST_CHARGE_SKIRMISH, GROUP_CAPACITOR_BOOSTER_CHARGE, GROUP_REFINABLES, GROUP_ROUGE_DRONE_COMPONENTS, GROUP_NAMED_COMPONENTS], blocked_groups: &[], types: &[TYPE_NANITE_REPAIR_PASTE], blocked_types: &[], full: messages::MSG_NOT_ENOUGH_SPECIAL_BAY_SPACE },
    // inventoryFlagsCommon.py:393-402
    Hold { flag: FLAG_SPECIALIZED_FILAMENT_HOLD, attribute: ATTR_SPECIAL_FILAMENT_HOLD_CAPACITY, categories: &[], groups: &[GROUP_RANDOM_JUMP_KEYS, GROUP_TRIGLAVIAN_JUMP_KEYS], blocked_groups: &[], types: &[], blocked_types: &[], full: messages::MSG_NOT_ENOUGH_SPECIAL_BAY_SPACE },

];

pub(crate) fn descriptor(flag: i32) -> Option<&'static Hold> {
    HOLDS.iter().find(|hold| hold.flag == flag)
}

// `ShouldAllowAdd` gives allowed types a final chance after group rejection;
// blocked groups are evaluated only after category acceptance.
pub(crate) fn accepts_class(hold: &Hold, category: Option<i32>, group: Option<i32>, type_id: i32) -> bool {
    let mut allowed = hold.categories.is_empty() && hold.groups.is_empty() && hold.types.is_empty();
    if !hold.categories.is_empty() { allowed = category.is_some_and(|id| hold.categories.contains(&id)); }
    if !allowed {
        if !hold.groups.is_empty() { allowed = group.is_some_and(|id| hold.groups.contains(&id)); }
    } else if group.is_some_and(|id| hold.blocked_groups.contains(&id)) {
        allowed = false;
    }
    if !allowed {
        if !hold.types.is_empty() { allowed = hold.types.contains(&type_id); }
    } else if hold.blocked_types.contains(&type_id) {
        allowed = false;
    }
    allowed
}

pub(crate) fn accepts(sde: &Sde, flag: i32, type_id: i32) -> bool {
    let Some(hold) = descriptor(flag) else { return false };
    let Some(ty) = sde.types.get(type_id) else { return false };
    accepts_class(hold, ty.category_id, ty.group_id, type_id)
}

pub(crate) fn effective_capacity(fit: &Fit, flag: i32) -> f64 {
    let Some(hold) = descriptor(flag) else { return 0.0 };
    if !fit.type_has_attribute(Key::Ship, hold.attribute) { return 0.0; }
    let value = fit.value(Key::Ship, hold.attribute);
    if !value.is_finite() || value <= 0.0 { return 0.0; }
    if flag == FLAG_FRIGATE_ESCAPE_BAY && value > 0.0 { 1.0 } else { value }
}

pub(super) fn admission(sde: &Sde, flag: i32, type_id: i32, singleton: bool, quantity: i64) -> Result<f64, CallError> {
    if !accepts(sde, flag, type_id) { return Err(user_error(messages::MSG_CANNOT_STORE_DESTINATION_RESTRICTED, vec![])); }
    if stack_size(singleton, quantity) <= 0 {
        return Err(user_error(messages::MSG_CANNOT_STORE_DESTINATION_RESTRICTED, vec![]));
    }
    let volume = if flag == FLAG_FRIGATE_ESCAPE_BAY {
        stack_size(singleton, quantity) as f64
    } else {
        item_volume(sde, type_id, singleton, quantity, None)
            .map_err(|_| user_error(messages::MSG_CANNOT_STORE_DESTINATION_RESTRICTED, vec![]))?
    };
    if !volume.is_finite() || volume < 0.0 {
        return Err(user_error(messages::MSG_CANNOT_STORE_DESTINATION_RESTRICTED, vec![]));
    }
    Ok(volume)
}

pub(super) fn full_error(flag: i32, type_id: i32, free: f64, volume: f64) -> CallError {
    let Some(hold) = descriptor(flag) else { return user_error(messages::MSG_CANNOT_STORE_DESTINATION_RESTRICTED, vec![]) };
    // plexVault.py:101-103 uses type/free/required; invControllers.py catches
    // cargo space refusals and recomputes available/volume.
    let dict = if hold.full == messages::MSG_NOT_ENOUGH_CARGO_SPACE {
        vec![("available".into(), Value::Float(free)), ("volume".into(), Value::Float(volume))]
    } else {
        vec![("type".into(), Value::Int(type_id as i64)), ("free".into(), Value::Float(free)), ("required".into(), Value::Float(volume))]
    };
    user_error(hold.full, dict)
}

pub(crate) fn check(sde: &Sde, flag: i32, type_id: i32, singleton: bool, quantity: i64, capacity: f64, used: f64) -> Result<(), CallError> {
    let volume = admission(sde, flag, type_id, singleton, quantity)?;
    if !capacity.is_finite() || capacity < 0.0 || !used.is_finite() || used < 0.0 {
        return Err(user_error(messages::MSG_CANNOT_STORE_DESTINATION_RESTRICTED, vec![]));
    }
    let free = (capacity - used).max(0.0);
    if capacity <= 0.0 || used > capacity || volume > free {
        return Err(full_error(flag, type_id, free, volume));
    }
    Ok(())
}

pub(crate) fn mining_flag(sde: &Sde, fit: &Fit, type_id: i32) -> i32 {
    let Some(ty) = sde.types.get(type_id) else { return FLAG_CARGO };
    let candidates: &[i32] = if ty.group_id.is_some_and(|g| [GROUP_ICE, GROUP_TEMPORAL_RESOURCES, GROUP_ANCIENT_COMPRESSED_ICE].contains(&g)) {
        &[FLAG_SPECIALIZED_ICE_HOLD]
    } else if ty.group_id.is_some_and(|g| [GROUP_HARVESTABLE_CLOUD, GROUP_COMPRESSED_GAS].contains(&g)) {
        &[FLAG_SPECIALIZED_GAS_HOLD]
    } else {
        &[FLAG_GENERAL_MINING_HOLD, FLAG_SPECIAL_ASTEROID_HOLD]
    };
    candidates.iter().copied().find(|&flag| effective_capacity(fit, flag) > 0.0 && accepts(sde, flag, type_id)).unwrap_or(FLAG_CARGO)
}

#[cfg(test)] mod tests;
