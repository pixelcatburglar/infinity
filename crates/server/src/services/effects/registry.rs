//! Numeric behavior dispatch and modifier fallback for SDE dogma effects.
//! Client counterparts are named in `dogma/const.py` and `dogma/pythonEffects`.
//! Out-of-scope effects are rejected even if they contain modifier rows;
//! classifying a later-wave kind does not implement its lifecycle behavior.
use infinity_core::{consts::{*, dogma::*}};
use crate::sde::Sde;
use super::{EffectKind, JammingType};

/// Client inventorycommon/const.py: groupMissileLauncherFestival.
const GROUP_MISSILE_LAUNCHER_FESTIVAL: i64 = 501;
/// Client inventorycommon/const.py: groupBubbleProbeLauncher.
const GROUP_BUBBLE_PROBE_LAUNCHER: i64 = 589;
/// Client inventorycommon/const.py: groupMissileLauncherBomb.
const GROUP_BOMB_LAUNCHER: i64 = 862;
/// Client inventorycommon/const.py: groupMissileLauncherXLTorpedo.
const GROUP_XL_TORPEDO_LAUNCHER: i64 = 524;
/// Client inventorycommon/const.py: groupMissileLauncherXLCruise.
const GROUP_XL_CRUISE_LAUNCHER: i64 = 1674;
/// Client inventorycommon/const.py: groupMissileLauncherRapidTorpedo.
const GROUP_RAPID_TORPEDO_LAUNCHER: i64 = 1673;

/// Player activations are module effects, excluding later-wave launcher families.
pub(super) fn activation_in_scope(category: i64, group: i64, kind: EffectKind) -> bool {
    category == i64::from(CATEGORY_MODULE)
        && !(kind == EffectKind::Missile && matches!(group,
            GROUP_MISSILE_LAUNCHER_FESTIVAL | GROUP_BUBBLE_PROBE_LAUNCHER |
            GROUP_BOMB_LAUNCHER | GROUP_XL_TORPEDO_LAUNCHER |
            GROUP_XL_CRUISE_LAUNCHER | GROUP_RAPID_TORPEDO_LAUNCHER))
}

/// SDE dogma effect attribute-ID columns, in descriptor array order.
#[derive(Clone, Copy)]
pub(crate) enum EffectAttribute { Duration, Discharge, Range, Falloff, Tracking }

/// SDE dogma effect boolean columns, in descriptor bit order.
#[derive(Clone, Copy)]
pub(crate) enum EffectFlag { Offensive, Assistance, DisallowAutoRepeat, ElectronicChance, PropulsionChance }

/// Client dogma/const.py: dgmEffActivation, dgmEffTarget, dgmEffOnline.
const ACTIVE_CATEGORIES: [i32; 3] = [1, 2, 4];

/// Resolved behavior and SDE metadata needed by effect consumers.
#[derive(Clone)]
pub(crate) struct Effect {
    pub(crate) id: i64,
    pub(crate) kind: EffectKind,
    pub(crate) guid: Option<String>,
    category: Option<i32>,
    attributes: [Option<i32>; 5],
    flags: u8,
}

impl Effect {
    fn from_row(row: &crate::sde::dogma::Effect, kind: EffectKind) -> Self {
        let mut attributes = [None; 5];
        for (key, value) in [
            (EffectAttribute::Duration, row.duration_attribute_id),
            (EffectAttribute::Discharge, row.discharge_attribute_id),
            (EffectAttribute::Range, row.range_attribute_id),
            (EffectAttribute::Falloff, row.falloff_attribute_id),
            (EffectAttribute::Tracking, row.tracking_attribute_id),
        ] { attributes[key as usize] = value; }
        let mut flags = 0;
        for (key, value) in [
            (EffectFlag::Offensive, row.is_offensive),
            (EffectFlag::Assistance, row.is_assistance),
            (EffectFlag::DisallowAutoRepeat, row.disallow_auto_repeat),
            (EffectFlag::ElectronicChance, row.electronic_chance),
            (EffectFlag::PropulsionChance, row.propulsion_chance),
        ] { if value { flags |= 1 << key as u8; } }
        Self {
            id: i64::from(row.effect_id), kind,
            guid: row.guid.clone().filter(|g| !g.is_empty()),
            category: row.effect_category, attributes, flags,
        }
    }

    pub(crate) fn attribute(&self, key: EffectAttribute) -> Option<i32> { self.attributes[key as usize] }
    pub(crate) fn has(&self, key: EffectFlag) -> bool { self.flags & (1 << key as u8) != 0 }
    pub(super) fn manually_activatable(&self) -> bool {
        self.kind != EffectKind::Modifiers || self.category.is_some_and(|category| ACTIVE_CATEGORIES.contains(&category))
    }
}

pub(crate) fn resolve(sde: &Sde, effect_id: i32) -> Option<Effect> {
    let row = sde.dogma.effect(effect_id)?;
    Some(Effect::from_row(row, classify(effect_id, !row.modifiers().is_empty())?))
}


pub(crate) fn kind(sde: &Sde, effect_id: i32) -> Option<EffectKind> {
    classify(effect_id, sde.dogma.effect(effect_id).is_some_and(|effect| !effect.modifiers().is_empty()))
}

fn classify(effect_id: i32, has_modifiers: bool) -> Option<EffectKind> {
    use EffectKind::*;
    Some(match effect_id {
        EFFECT_TARGET_ATTACK => Turret,
        EFFECT_PROJECTILE_FIRED => Turret,
        EFFECT_TARGET_DISINTEGRATOR_ATTACK => Turret,
        EFFECT_USE_MISSILES => Missile,
        EFFECT_SHIELD_BOOSTING => LocalRepair { layer: 0, amount_attr: ATTR_SHIELD_BONUS },
        EFFECT_ARMOR_REPAIR => LocalRepair { layer: 1, amount_attr: ATTR_ARMOR_DAMAGE_AMOUNT },
        EFFECT_STRUCTURE_REPAIR => LocalRepair { layer: 2, amount_attr: ATTR_STRUCTURE_DAMAGE_AMOUNT },
        EFFECT_MINING_LASER => Mining,
        EFFECT_MINING_CLOUDS => Mining,
        EFFECT_MINING => Mining,
        EFFECT_HACKING => Hacking,
        EFFECT_PROBE_LAUNCHING => ProbeLaunch,
        EFFECT_EMP_WAVE => Smartbomb,
        EFFECT_CHAIN_LIGHTNING => Vorton,
        EFFECT_FUELED_ARMOR_REPAIR => FueledLocalRepair { layer: 1, amount_attr: ATTR_ARMOR_DAMAGE_AMOUNT, charge_bonus_attr: Some(ATTR_CHARGED_ARMOR_DAMAGE_MULTIPLIER) },
        EFFECT_FUELED_SHIELD_BOOSTING => FueledLocalRepair { layer: 0, amount_attr: ATTR_SHIELD_BONUS, charge_bonus_attr: None },
        EFFECT_POWER_BOOSTER => CapBooster,
        EFFECT_ENERGY_NEUTRALIZER_FALLOFF => Neutralizer,
        EFFECT_ENTITY_ENERGY_NEUTRALIZER_FALLOFF => Neutralizer,
        EFFECT_ENERGY_NOSFERATU_FALLOFF => Nosferatu,
        EFFECT_REMOTE_CAPACITOR_TRANSMITTER => CapTransfer,
        EFFECT_REMOTE_ARMOR_REPAIRER => RemoteRepair { layer: 1 },
        EFFECT_REMOTE_HULL_REPAIRER => RemoteRepair { layer: 2 },
        EFFECT_REMOTE_SHIELD_BOOSTER => RemoteRepair { layer: 0 },
        EFFECT_NPC_REMOTE_ARMOR_REPAIRER => RemoteRepair { layer: 1 },
        EFFECT_NPC_REMOTE_HULL_REPAIRER => RemoteRepair { layer: 2 },
        EFFECT_NPC_REMOTE_SHIELD_BOOSTER => RemoteRepair { layer: 0 },
        EFFECT_ANCILLARY_REMOTE_ARMOR_REPAIRER => RemoteRepair { layer: 1 },
        EFFECT_ANCILLARY_REMOTE_SHIELD_BOOSTER => RemoteRepair { layer: 0 },
        EFFECT_MUTADAPTIVE_REMOTE_ARMOR_REPAIRER => RemoteRepair { layer: 1 },
        EFFECT_REMOTE_ECM_FALLOFF => Ecm,
        EFFECT_ENTITY_ECM_FALLOFF => Ecm,
        EFFECT_ECM_BURST_JAMMER => BurstJammer,
        EFFECT_CLOAKING => Cloak,
        EFFECT_CLOAKING_PROTOTYPE => Cloak,
        EFFECT_CLOAKING_WARP_SAFE => Cloak,
        EFFECT_MICRO_JUMP_DRIVE => MicroJumpDrive,
        EFFECT_SALVAGING => Salvage,
        EFFECT_SALVAGE_DRONE => Salvage,
        EFFECT_TRACTOR_BEAM => Tractor,
        EFFECT_CARGO_SCAN => CargoScan,
        EFFECT_SHIP_SCAN => ShipScan,
        EFFECT_SURVEY_SCAN => SurveyScan,
        EFFECT_TARGET_HOSTILES => AutoTarget,
        EFFECT_TARGET_PASSIVELY => PassiveTarget,
        EFFECT_BOMB_LAUNCHING => Bomb,
        EFFECT_REMOTE_WEBIFIER_FALLOFF => Projected { jamming: JammingType::Webify },
        EFFECT_REMOTE_WEBIFIER_ENTITY => Projected { jamming: JammingType::Webify },
        EFFECT_WARP_DISRUPT => Projected { jamming: JammingType::Disrupt },
        EFFECT_WARP_SCRAMBLE => Projected { jamming: JammingType::Scramble },
        EFFECT_REMOTE_TARGET_PAINT_FALLOFF => Projected { jamming: JammingType::Paint },
        EFFECT_REMOTE_TARGET_PAINT_ENTITY => Projected { jamming: JammingType::Paint },
        EFFECT_REMOTE_SENSOR_DAMP_FALLOFF => Projected { jamming: JammingType::Damp },
        EFFECT_REMOTE_SENSOR_DAMP_ENTITY => Projected { jamming: JammingType::Damp },
        EFFECT_TRACKING_DISRUPTOR => Projected { jamming: JammingType::TrackingDisrupt },
        EFFECT_NPC_WEAPON_DISRUPTOR => Projected { jamming: JammingType::TrackingDisrupt },
        EFFECT_GUIDANCE_DISRUPTOR => Projected { jamming: JammingType::GuidanceDisrupt },
        EFFECT_REMOTE_SENSOR_BOOST => Projected { jamming: JammingType::SensorBoost },
        EFFECT_REMOTE_TRACKING_COMPUTER => Projected { jamming: JammingType::TrackingBoost },
        EFFECT_EXCLUDED_649 => return None,
        EFFECT_EXCLUDED_650 => return None,
        EFFECT_EXCLUDED_11691 => return None,
        EFFECT_EXCLUDED_6482 => return None,
        EFFECT_EXCLUDED_6481 => return None,
        EFFECT_EXCLUDED_6513 => return None,
        EFFECT_EXCLUDED_6477 => return None,
        EFFECT_EXCLUDED_6478 => return None,
        EFFECT_EXCLUDED_6479 => return None,
        EFFECT_EXCLUDED_6476 => return None,
        EFFECT_EXCLUDED_6472 => return None,
        EFFECT_EXCLUDED_6473 => return None,
        EFFECT_EXCLUDED_6474 => return None,
        EFFECT_EXCLUDED_6201 => return None,
        EFFECT_EXCLUDED_6440 => return None,
        EFFECT_EXCLUDED_6439 => return None,
        EFFECT_EXCLUDED_6485 => return None,
        EFFECT_EXCLUDED_6442 => return None,
        EFFECT_EXCLUDED_6441 => return None,
        EFFECT_EXCLUDED_6719 => return None,
        EFFECT_EXCLUDED_6465 => return None,
        EFFECT_EXCLUDED_6437 => return None,
        EFFECT_EXCLUDED_6434 => return None,
        EFFECT_EXCLUDED_6554 => return None,
        EFFECT_EXCLUDED_6431 => return None,
        EFFECT_EXCLUDED_6435 => return None,
        EFFECT_EXCLUDED_6464 => return None,
        EFFECT_EXCLUDED_6436 => return None,
        EFFECT_EXCLUDED_4489 => return None,
        EFFECT_EXCLUDED_4490 => return None,
        EFFECT_EXCLUDED_4491 => return None,
        EFFECT_EXCLUDED_4492 => return None,
        EFFECT_NPC_BEHAVIOR_REMOTE_ARMOR => RemoteRepair { layer: 1 },
        EFFECT_NPC_BEHAVIOR_REMOTE_SHIELD => RemoteRepair { layer: 0 },
        EFFECT_NPC_BEHAVIOR_REMOTE_CAPACITOR => CapTransfer,
        EFFECT_NPC_BEHAVIOR_NEUTRALIZER => Neutralizer,
        EFFECT_NPC_BEHAVIOR_NOSFERATU => Nosferatu,
        EFFECT_NPC_BEHAVIOR_ECM => Ecm,
        EFFECT_NPC_BEHAVIOR_MWD => Propulsion,
        EFFECT_NPC_BEHAVIOR_MINING => Mining,
        EFFECT_NPC_BEHAVIOR_SMARTBOMB => Smartbomb,
        EFFECT_NPC_BEHAVIOR_ARMOR_REPAIR => LocalRepair { layer: 1, amount_attr: ATTR_ENTITY_ARMOR_REPAIR_AMOUNT },
        EFFECT_NPC_BEHAVIOR_SHIELD_BOOST => LocalRepair { layer: 0, amount_attr: ATTR_ENTITY_SHIELD_BOOST_AMOUNT },
        EFFECT_NPC_BEHAVIOR_WEB => Projected { jamming: JammingType::Webify },
        EFFECT_NPC_BEHAVIOR_GUIDANCE_DISRUPTOR => Projected { jamming: JammingType::GuidanceDisrupt },
        EFFECT_NPC_BEHAVIOR_TRACKING_DISRUPTOR => Projected { jamming: JammingType::TrackingDisrupt },
        EFFECT_NPC_ENTITY_TRACKING_DISRUPTOR => Projected { jamming: JammingType::TrackingDisrupt },
        EFFECT_NPC_BEHAVIOR_WARP_DISRUPT => Projected { jamming: JammingType::Disrupt },
        EFFECT_NPC_BEHAVIOR_WARP_SCRAMBLE => Projected { jamming: JammingType::Scramble },
        EFFECT_NPC_BEHAVIOR_PAINTER => Projected { jamming: JammingType::Paint },
        EFFECT_NPC_BEHAVIOR_DAMP => Projected { jamming: JammingType::Damp },
        EFFECT_NPC_WARP_SCRAMBLE => Projected { jamming: JammingType::Scramble },
        EFFECT_NPC_WARP_SCRAMBLE_MWD => Projected { jamming: JammingType::Scramble },
        EFFECT_WARP_DISRUPT_SPHERE => return None,
        EFFECT_ENTOSIS_LINK => Entosis,
        EFFECT_SNOWBALL_LAUNCHING => return None,
        EFFECT_INDUSTRIAL_ITEM_COMPRESSION => Compression,
        EFFECT_CLONE_RESPAWN_BAY => return None,
        EFFECT_MICRO_JUMP_PORTAL => return None,
        EFFECT_MICRO_JUMP_PORTAL_CAPITAL => return None,
        EFFECT_COMMAND_BURST_ARMOR => return None,
        EFFECT_COMMAND_BURST_SHIELD => return None,
        EFFECT_COMMAND_BURST_SKIRMISH => return None,
        EFFECT_COMMAND_BURST_INFO => return None,
        EFFECT_COMMAND_BURST_MINING => return None,
        EFFECT_TITAN_EFFECT_GENERATOR => return None,
        EFFECT_SIEGE_MODE => return None,
        EFFECT_TRIAGE_MODE => return None,
        EFFECT_TRIAGE_MODE_7 => return None,
        EFFECT_AFTERBURNER | EFFECT_MICROWARPDRIVE => Propulsion,
        id if id == EFFECT_ONLINE as i32 => Online,
        EFFECT_CYNO_GENERATION |
        EFFECT_CLONE_JUMP_ACCEPTING |
        EFFECT_INDUSTRIAL_COMPACT_CORE |
        EFFECT_INDUSTRIAL_CORE |
        EFFECT_JUMP_PORTAL |
        EFFECT_JUMP_PORTAL_BO |
        EFFECT_BASTION |
        EFFECT_SACRIBIS_BASTION |
        EFFECT_SIEGE |
        EFFECT_TRIAGE |
        EFFECT_STELLIAN_TRIAGE |
        EFFECT_FOCUSED_WARP_DISRUPTION_SCRIPT |
        EFFECT_FOCUSED_WARP_SCRAMBLING_SCRIPT => return None,
        _ if has_modifiers => Modifiers,
        _ => return None,
    })
}

#[cfg(test)]
mod tests;
