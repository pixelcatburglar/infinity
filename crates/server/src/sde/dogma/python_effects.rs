//! Python effect modifier data from `eve/common/script/dogma/pythonEffects/`.
//! Only empty SDE modifier lists are supplemented; effect lifecycle is not represented.
//! Weapon disruption includes ship, entity and drone target categories; fighter
//! ability targets are not represented by the current fit nodes.
use super::{Effect, Modifier};
use crate::consts::{ATTR_ACTIVATION_BLOCKED, ATTR_ACTIVATION_BLOCKED_STRENGTH, ATTR_AOE_CLOUD_SIZE, ATTR_ARMOR_DAMAGE_AMOUNT, ATTR_ARMOR_EM_DAMAGE_RESONANCE, ATTR_ARMOR_EXPLOSIVE_DAMAGE_RESONANCE, ATTR_ARMOR_KINETIC_DAMAGE_RESONANCE, ATTR_ARMOR_THERMAL_DAMAGE_RESONANCE, ATTR_DAMAGE_MULTIPLIER, ATTR_EM_DAMAGE, ATTR_EXPLOSION_DELAY, ATTR_EXPLOSIVE_DAMAGE, ATTR_FALLOFF, ATTR_KINETIC_DAMAGE, ATTR_MASS, ATTR_MAX_LOCKED_TARGETS, ATTR_MAX_RANGE, ATTR_MAX_TARGET_RANGE, ATTR_MAX_VELOCITY, ATTR_RATE_OF_FIRE, ATTR_SCAN_RESOLUTION, ATTR_SHIELD_BONUS, ATTR_SHIELD_EM_DAMAGE_RESONANCE, ATTR_SHIELD_EXPLOSIVE_DAMAGE_RESONANCE, ATTR_SHIELD_KINETIC_DAMAGE_RESONANCE, ATTR_SHIELD_THERMAL_DAMAGE_RESONANCE, ATTR_SIGNATURE_RADIUS, ATTR_SPEED_FACTOR, ATTR_SPEED_FACTOR_INTERIM, ATTR_STRUCTURE_EM_DAMAGE_RESONANCE, ATTR_STRUCTURE_EXPLOSIVE_DAMAGE_RESONANCE, ATTR_STRUCTURE_KINETIC_DAMAGE_RESONANCE, ATTR_STRUCTURE_THERMAL_DAMAGE_RESONANCE, ATTR_THERMAL_DAMAGE, ATTR_WARP_SCRAMBLE_STATUS};
use crate::consts::{CATEGORY_DRONE, CATEGORY_ENTITY, CATEGORY_SHIP, ATTR_MISSILE_ENTITY_VELOCITY_MULTIPLIER, ATTR_MISSILE_ENTITY_FLIGHT_TIME_MULTIPLIER, ATTR_MISSILE_ENTITY_AOE_VELOCITY_MULTIPLIER, ATTR_MISSILE_ENTITY_AOE_CLOUD_SIZE_MULTIPLIER, ATTR_TRACKING_SPEED};
use crate::consts::{ATTR_CPU, ATTR_CPU_LOAD, ATTR_POWER, ATTR_POWER_LOAD, ATTR_SLOTS, ATTR_TURRET_SLOTS_LEFT, ATTR_LAUNCHER_SLOTS_LEFT, ATTR_UPGRADE_COST, ATTR_UPGRADE_LOAD, ATTR_UPGRADE_SLOTS_LEFT};

/// Client dogma/const.py: attributeAoeCloudSizeBonus.
const ATTR_AOE_CLOUD_SIZE_BONUS: i32 = 848;
/// Client dogma/const.py: attributeAoeFalloff.
const ATTR_AOE_FALLOFF: i32 = 655;
/// Client dogma/const.py: attributeAoeFalloffBonus.
const ATTR_AOE_FALLOFF_BONUS: i32 = 857;
/// Client dogma/const.py: attributeAoeVelocity.
const ATTR_AOE_VELOCITY: i32 = 653;
/// Client dogma/const.py: attributeAoeVelocityBonus.
const ATTR_AOE_VELOCITY_BONUS: i32 = 847;
/// Client dogma/const.py: attributeArmorDamageAmountBonus.
const ATTR_ARMOR_DAMAGE_AMOUNT_BONUS: i32 = 895;
/// Client dogma/const.py: attributeArmorDamageDurationBonus.
const ATTR_ARMOR_DAMAGE_DURATION_BONUS: i32 = 896;
/// Client dogma/const.py: attributeCloakingTargetingDelay.
const ATTR_CLOAKING_TARGETING_DELAY: i32 = 560;
/// Client dogma/const.py: attributeCloakingTargetingDelayBonus.
const ATTR_CLOAKING_TARGETING_DELAY_BONUS: i32 = 619;
/// Client dogma/const.py: attributeDamageMultiplierBonus.
const ATTR_DAMAGE_MULTIPLIER_BONUS: i32 = 292;
/// Client dogma/const.py: attributeDisallowOffensiveModifiers.
const ATTR_DISALLOW_OFFENSIVE_MODIFIERS: i32 = 872;
/// Client dogma/const.py: attributeDuration.
const ATTR_DURATION: i32 = 73;
/// Client dogma/const.py: attributeExplosionDelayBonus.
const ATTR_EXPLOSION_DELAY_BONUS: i32 = 596;
/// Client dogma/const.py: attributeFalloffBonus.
const ATTR_FALLOFF_BONUS: i32 = 349;
/// Client dogma/const.py: attributeHullEmDamageResonance.
const ATTR_HULL_EM_DAMAGE_RESONANCE: i32 = 974;
/// Client dogma/const.py: attributeHullExplosiveDamageResonance.
const ATTR_HULL_EXPLOSIVE_DAMAGE_RESONANCE: i32 = 975;
/// Client dogma/const.py: attributeHullKineticDamageResonance.
const ATTR_HULL_KINETIC_DAMAGE_RESONANCE: i32 = 976;
/// Client dogma/const.py: attributeHullThermalDamageResonance.
const ATTR_HULL_THERMAL_DAMAGE_RESONANCE: i32 = 977;
/// Client dogma/const.py: attributeMassAddition.
const ATTR_MASS_ADDITION: i32 = 796;
/// Client dogma/const.py: attributeMaxRangeBonus.
const ATTR_MAX_RANGE_BONUS: i32 = 351;
/// Client dogma/const.py: attributeMaxTargetRangeBonusInterim.
const ATTR_MAX_TARGET_RANGE_BONUS_INTERIM: i32 = 2136;
/// Client dogma/const.py: attributeMissileVelocityBonus.
const ATTR_MISSILE_VELOCITY_BONUS: i32 = 547;
/// Client dogma/const.py: attributeRofBonus.
const ATTR_ROF_BONUS: i32 = 293;
/// Client dogma/const.py: attributeScanGravimetricStrength.
const ATTR_SCAN_GRAVIMETRIC_STRENGTH: i32 = 211;
/// Client dogma/const.py: attributeScanGravimetricStrengthPercentInterim.
const ATTR_SCAN_GRAVIMETRIC_STRENGTH_PERCENT_INTERIM: i32 = 2255;
/// Client dogma/const.py: attributeScanLadarStrength.
const ATTR_SCAN_LADAR_STRENGTH: i32 = 209;
/// Client dogma/const.py: attributeScanLadarStrengthPercentInterim.
const ATTR_SCAN_LADAR_STRENGTH_PERCENT_INTERIM: i32 = 2256;
/// Client dogma/const.py: attributeScanMagnetometricStrength.
const ATTR_SCAN_MAGNETOMETRIC_STRENGTH: i32 = 210;
/// Client dogma/const.py: attributeScanMagnetometricStrengthPercentInterim.
const ATTR_SCAN_MAGNETOMETRIC_STRENGTH_PERCENT_INTERIM: i32 = 2257;
/// Client dogma/const.py: attributeScanRadarStrength.
const ATTR_SCAN_RADAR_STRENGTH: i32 = 208;
/// Client dogma/const.py: attributeScanRadarStrengthPercentInterim.
const ATTR_SCAN_RADAR_STRENGTH_PERCENT_INTERIM: i32 = 2258;
/// Client dogma/const.py: attributeScanResolutionBonusInterim.
const ATTR_SCAN_RESOLUTION_BONUS_INTERIM: i32 = 2137;
/// Client dogma/const.py: attributeScanResolutionMultiplier.
const ATTR_SCAN_RESOLUTION_MULTIPLIER: i32 = 565;
/// Client dogma/const.py: attributeShieldBonusDurationBonus.
const ATTR_SHIELD_BONUS_DURATION_BONUS: i32 = 897;
/// Client dogma/const.py: attributeShieldBoostMultiplier.
const ATTR_SHIELD_BOOST_MULTIPLIER: i32 = 548;
/// Client dogma/const.py: attributeSiegeMassMultiplier.
const ATTR_SIEGE_MASS_MULTIPLIER: i32 = 1471;
/// Client dogma/const.py: attributeSiegeModeWarpStatus.
const ATTR_SIEGE_MODE_WARP_STATUS: i32 = 852;
/// Client dogma/const.py: attributeSignatureRadiusBonus.
const ATTR_SIGNATURE_RADIUS_BONUS: i32 = 554;
/// Client dogma/const.py: attributeSignatureRadiusBonusInterim.
const ATTR_SIGNATURE_RADIUS_BONUS_INTERIM: i32 = 2147;
/// Client dogma/const.py: attributeSignatureRadiusBonusPercent.
const ATTR_SIGNATURE_RADIUS_BONUS_PERCENT: i32 = 973;
/// Client dogma/const.py: attributeTrackingSpeedBonus.
const ATTR_TRACKING_SPEED_BONUS: i32 = 767;

/// Client dogma/const.py modifier operations.
const PRE_MUL: i32 = 0;
const MOD_ADD: i32 = 2;
const MOD_SUB: i32 = 3;
const POST_MUL: i32 = 4;
const POST_PERCENT: i32 = 6;
const POST_ASSIGN: i32 = 7;
/// `migratedeffects.py` uses the effect source item type as the required skill.
const SOURCE_ITEM_TYPE: i32 = -1;
/// Client inventorycommon/const.py typeGunnery.
const TYPE_GUNNERY: i32 = 3300;
/// Client inventorycommon/const.py typeMissileLauncherOperation.
const TYPE_MISSILE_LAUNCHER_OPERATION: i32 = 3319;
/// Client bastionMode.py required skill type 3307.
const BASTION_SKILL_3307: i32 = 3307;
/// Client bastionMode.py required skill type 3308.
const BASTION_SKILL_3308: i32 = 3308;
/// Client bastionMode.py required skill type 3309.
const BASTION_SKILL_3309: i32 = 3309;
/// Client bastionMode.py required skill type 3324.
const BASTION_SKILL_3324: i32 = 3324;
/// Client bastionMode.py required skill type 3325.
const BASTION_SKILL_3325: i32 = 3325;
/// Client bastionMode.py required skill type 3326.
const BASTION_SKILL_3326: i32 = 3326;
/// Client bastionMode.py required skill type 3393.
const BASTION_SKILL_3393: i32 = 3393;
/// Client bastionMode.py required skill type 3416.
const BASTION_SKILL_3416: i32 = 3416;
/// Client bastionMode.py required skill type 4385.
const BASTION_SKILL_4385: i32 = 4385;
/// Client bastionMode.py required skill type 20327.
const BASTION_SKILL_20327: i32 = 20327;
/// Client bastionMode.py required skill type 21666.
const BASTION_SKILL_21666: i32 = 21666;
/// Client bastionMode.py required skill type 21667.
const BASTION_SKILL_21667: i32 = 21667;
/// Client bastionMode.py required skill type 21668.
const BASTION_SKILL_21668: i32 = 21668;
/// Client bastionMode.py required skill type 21802.
const BASTION_SKILL_21802: i32 = 21802;
/// Client bastionMode.py required skill type 21803.
const BASTION_SKILL_21803: i32 = 21803;
/// Client bastionMode.py required skill type 32435.
const BASTION_SKILL_32435: i32 = 32435;
/// Client dogma/const.py: raw source attributes for sensor falloff.
const ATTR_MAX_TARGET_RANGE_BONUS: i32 = 309;
const ATTR_SCAN_RESOLUTION_BONUS: i32 = 566;
const ATTR_SCAN_GRAVIMETRIC_STRENGTH_PERCENT: i32 = 1027;
const ATTR_SCAN_LADAR_STRENGTH_PERCENT: i32 = 1028;
const ATTR_SCAN_MAGNETOMETRIC_STRENGTH_PERCENT: i32 = 1029;
const ATTR_SCAN_RADAR_STRENGTH_PERCENT: i32 = 1030;

/// Resolve falloff-effect interim attributes to their raw source attributes.
pub(crate) fn projected_interim_source(attr: i32) -> Option<i32> {
    match attr {
        ATTR_SPEED_FACTOR_INTERIM => Some(ATTR_SPEED_FACTOR),
        ATTR_SIGNATURE_RADIUS_BONUS_INTERIM => Some(ATTR_SIGNATURE_RADIUS_BONUS),
        ATTR_MAX_TARGET_RANGE_BONUS_INTERIM => Some(ATTR_MAX_TARGET_RANGE_BONUS),
        ATTR_SCAN_RESOLUTION_BONUS_INTERIM => Some(ATTR_SCAN_RESOLUTION_BONUS),
        ATTR_SCAN_GRAVIMETRIC_STRENGTH_PERCENT_INTERIM => Some(ATTR_SCAN_GRAVIMETRIC_STRENGTH_PERCENT),
        ATTR_SCAN_LADAR_STRENGTH_PERCENT_INTERIM => Some(ATTR_SCAN_LADAR_STRENGTH_PERCENT),
        ATTR_SCAN_MAGNETOMETRIC_STRENGTH_PERCENT_INTERIM => Some(ATTR_SCAN_MAGNETOMETRIC_STRENGTH_PERCENT),
        ATTR_SCAN_RADAR_STRENGTH_PERCENT_INTERIM => Some(ATTR_SCAN_RADAR_STRENGTH_PERCENT),
        _ => None,
    }
}

#[derive(Clone, Copy)]
struct Row { func: &'static str, domain: &'static str, op: i32, modified: i32, modifying: i32, skill: Option<i32> }

pub(super) fn supplement(effect: &mut Effect) {
    if !effect.modifiers.is_empty() { return; }
    let rows: &[Row] = match effect.name.as_deref() {
        // eve/common/script/dogma/online.py: OnlineEffect.
        Some("online") => &[
            Row { func: "ItemModifier", domain: "shipID", op: MOD_ADD, modified: ATTR_CPU_LOAD, modifying: ATTR_CPU, skill: None },
            Row { func: "ItemModifier", domain: "shipID", op: MOD_ADD, modified: ATTR_POWER_LOAD, modifying: ATTR_POWER, skill: None },
        ],
        // eve/common/script/dogma/fitting.py: LauncherFittedEffect and TurretFittedEffect.
        Some("launcherFitted") => &[
            Row { func: "ItemModifier", domain: "shipID", op: MOD_SUB, modified: ATTR_LAUNCHER_SLOTS_LEFT, modifying: ATTR_SLOTS, skill: None },
        ],
        Some("turretFitted") => &[
            Row { func: "ItemModifier", domain: "shipID", op: MOD_SUB, modified: ATTR_TURRET_SLOTS_LEFT, modifying: ATTR_SLOTS, skill: None },
        ],
        // eve/common/script/dogma/fitting.py: UpgradeFittedEffect.
        Some("rigSlot") => &[
            Row { func: "ItemModifier", domain: "shipID", op: MOD_SUB, modified: ATTR_UPGRADE_SLOTS_LEFT, modifying: ATTR_SLOTS, skill: None },
            Row { func: "ItemModifier", domain: "shipID", op: MOD_ADD, modified: ATTR_UPGRADE_LOAD, modifying: ATTR_UPGRADE_COST, skill: None },
        ],
        // eve/common/script/dogma/pythonEffects/remoteWebifierFalloff.py: BaseRemoteWebifierFalloff.
        Some("remoteWebifierFalloff" | "remoteWebifierEntity" | "npcBehaviorWebifier") => &[
            Row { func: "ItemModifier", domain: "targetID", op: POST_PERCENT, modified: ATTR_MAX_VELOCITY, modifying: ATTR_SPEED_FACTOR_INTERIM, skill: None },
        ],
        // eve/common/script/dogma/pythonEffects/remoteTargetPaintFalloff.py: BaseRemoteTargetPaintFalloff.
        Some("remoteTargetPaintFalloff" | "remoteTargetPaintEntity" | "behaviorTargetPainter") => &[
            Row { func: "ItemModifier", domain: "targetID", op: POST_PERCENT, modified: ATTR_SIGNATURE_RADIUS, modifying: ATTR_SIGNATURE_RADIUS_BONUS_INTERIM, skill: None },
        ],
        // eve/common/script/dogma/pythonEffects/sensorFalloff.py: BaseSensorFalloffEffect.
        Some("remoteSensorDampFalloff" | "remoteSensorDampEntity" | "behaviorSensorDampener") => &[
            Row { func: "ItemModifier", domain: "targetID", op: POST_PERCENT, modified: ATTR_SCAN_GRAVIMETRIC_STRENGTH, modifying: ATTR_SCAN_GRAVIMETRIC_STRENGTH_PERCENT_INTERIM, skill: None },
            Row { func: "ItemModifier", domain: "targetID", op: POST_PERCENT, modified: ATTR_SCAN_LADAR_STRENGTH, modifying: ATTR_SCAN_LADAR_STRENGTH_PERCENT_INTERIM, skill: None },
            Row { func: "ItemModifier", domain: "targetID", op: POST_PERCENT, modified: ATTR_SCAN_MAGNETOMETRIC_STRENGTH, modifying: ATTR_SCAN_MAGNETOMETRIC_STRENGTH_PERCENT_INTERIM, skill: None },
            Row { func: "ItemModifier", domain: "targetID", op: POST_PERCENT, modified: ATTR_SCAN_RADAR_STRENGTH, modifying: ATTR_SCAN_RADAR_STRENGTH_PERCENT_INTERIM, skill: None },
            Row { func: "ItemModifier", domain: "targetID", op: POST_PERCENT, modified: ATTR_SCAN_RESOLUTION, modifying: ATTR_SCAN_RESOLUTION_BONUS_INTERIM, skill: None },
            Row { func: "ItemModifier", domain: "targetID", op: POST_PERCENT, modified: ATTR_MAX_TARGET_RANGE, modifying: ATTR_MAX_TARGET_RANGE_BONUS_INTERIM, skill: None },
        ],
        // eve/common/script/dogma/pythonEffects/sensorFalloff.py: BaseSensorFalloffEffect.
        Some("remoteSensorBoostFalloff") => &[
            Row { func: "ItemModifier", domain: "targetID", op: POST_PERCENT, modified: ATTR_SCAN_GRAVIMETRIC_STRENGTH, modifying: ATTR_SCAN_GRAVIMETRIC_STRENGTH_PERCENT_INTERIM, skill: None },
            Row { func: "ItemModifier", domain: "targetID", op: POST_PERCENT, modified: ATTR_SCAN_LADAR_STRENGTH, modifying: ATTR_SCAN_LADAR_STRENGTH_PERCENT_INTERIM, skill: None },
            Row { func: "ItemModifier", domain: "targetID", op: POST_PERCENT, modified: ATTR_SCAN_MAGNETOMETRIC_STRENGTH, modifying: ATTR_SCAN_MAGNETOMETRIC_STRENGTH_PERCENT_INTERIM, skill: None },
            Row { func: "ItemModifier", domain: "targetID", op: POST_PERCENT, modified: ATTR_SCAN_RADAR_STRENGTH, modifying: ATTR_SCAN_RADAR_STRENGTH_PERCENT_INTERIM, skill: None },
            Row { func: "ItemModifier", domain: "targetID", op: POST_PERCENT, modified: ATTR_SCAN_RESOLUTION, modifying: ATTR_SCAN_RESOLUTION_BONUS_INTERIM, skill: None },
            Row { func: "ItemModifier", domain: "targetID", op: POST_PERCENT, modified: ATTR_MAX_TARGET_RANGE, modifying: ATTR_MAX_TARGET_RANGE_BONUS_INTERIM, skill: None },
        ],
        // eve/common/script/dogma/pythonEffects/adaptiveHardener.py: BaseAdaptiveHardener.
        Some("adaptiveArmorHardener") => &[
            Row { func: "ItemModifier", domain: "shipID", op: PRE_MUL, modified: ATTR_ARMOR_EM_DAMAGE_RESONANCE, modifying: ATTR_ARMOR_EM_DAMAGE_RESONANCE, skill: None },
            Row { func: "ItemModifier", domain: "shipID", op: PRE_MUL, modified: ATTR_ARMOR_EXPLOSIVE_DAMAGE_RESONANCE, modifying: ATTR_ARMOR_EXPLOSIVE_DAMAGE_RESONANCE, skill: None },
            Row { func: "ItemModifier", domain: "shipID", op: PRE_MUL, modified: ATTR_ARMOR_KINETIC_DAMAGE_RESONANCE, modifying: ATTR_ARMOR_KINETIC_DAMAGE_RESONANCE, skill: None },
            Row { func: "ItemModifier", domain: "shipID", op: PRE_MUL, modified: ATTR_ARMOR_THERMAL_DAMAGE_RESONANCE, modifying: ATTR_ARMOR_THERMAL_DAMAGE_RESONANCE, skill: None },
        ],
        // eve/common/script/dogma/pythonEffects/emergencyHullEnergizer.py: EmergencyHullEnergizer.
        Some("emergencyHullEnergizer") => &[
            Row { func: "ItemModifier", domain: "shipID", op: POST_MUL, modified: ATTR_STRUCTURE_EM_DAMAGE_RESONANCE, modifying: ATTR_HULL_EM_DAMAGE_RESONANCE, skill: None },
            Row { func: "ItemModifier", domain: "shipID", op: POST_MUL, modified: ATTR_STRUCTURE_THERMAL_DAMAGE_RESONANCE, modifying: ATTR_HULL_THERMAL_DAMAGE_RESONANCE, skill: None },
            Row { func: "ItemModifier", domain: "shipID", op: POST_MUL, modified: ATTR_STRUCTURE_KINETIC_DAMAGE_RESONANCE, modifying: ATTR_HULL_KINETIC_DAMAGE_RESONANCE, skill: None },
            Row { func: "ItemModifier", domain: "shipID", op: POST_MUL, modified: ATTR_STRUCTURE_EXPLOSIVE_DAMAGE_RESONANCE, modifying: ATTR_HULL_EXPLOSIVE_DAMAGE_RESONANCE, skill: None },
        ],
        // eve/common/script/dogma/pythonEffects/bastionMode.py: BaseBastionMode.
        Some("moduleBonusBastionModule") => &[
            Row { func: "ItemModifier", domain: "shipID", op: POST_ASSIGN, modified: ATTR_MAX_LOCKED_TARGETS, modifying: ATTR_MAX_LOCKED_TARGETS, skill: None },
            Row { func: "ItemModifier", domain: "shipID", op: PRE_MUL, modified: ATTR_SCAN_RESOLUTION, modifying: ATTR_SCAN_RESOLUTION_MULTIPLIER, skill: None },
            Row { func: "ItemModifier", domain: "shipID", op: POST_PERCENT, modified: ATTR_MAX_VELOCITY, modifying: ATTR_SPEED_FACTOR, skill: None },
            Row { func: "ItemModifier", domain: "shipID", op: MOD_ADD, modified: ATTR_WARP_SCRAMBLE_STATUS, modifying: ATTR_SIEGE_MODE_WARP_STATUS, skill: None },
            Row { func: "ItemModifier", domain: "shipID", op: POST_MUL, modified: ATTR_MASS, modifying: ATTR_SIEGE_MASS_MULTIPLIER, skill: None },
            Row { func: "ItemModifier", domain: "shipID", op: POST_ASSIGN, modified: ATTR_DISALLOW_OFFENSIVE_MODIFIERS, modifying: ATTR_DISALLOW_OFFENSIVE_MODIFIERS, skill: None },
            Row { func: "ItemModifier", domain: "shipID", op: PRE_MUL, modified: ATTR_ARMOR_EM_DAMAGE_RESONANCE, modifying: ATTR_ARMOR_EM_DAMAGE_RESONANCE, skill: None },
            Row { func: "ItemModifier", domain: "shipID", op: PRE_MUL, modified: ATTR_ARMOR_EXPLOSIVE_DAMAGE_RESONANCE, modifying: ATTR_ARMOR_EXPLOSIVE_DAMAGE_RESONANCE, skill: None },
            Row { func: "ItemModifier", domain: "shipID", op: PRE_MUL, modified: ATTR_ARMOR_KINETIC_DAMAGE_RESONANCE, modifying: ATTR_ARMOR_KINETIC_DAMAGE_RESONANCE, skill: None },
            Row { func: "ItemModifier", domain: "shipID", op: PRE_MUL, modified: ATTR_ARMOR_THERMAL_DAMAGE_RESONANCE, modifying: ATTR_ARMOR_THERMAL_DAMAGE_RESONANCE, skill: None },
            Row { func: "ItemModifier", domain: "shipID", op: PRE_MUL, modified: ATTR_STRUCTURE_EM_DAMAGE_RESONANCE, modifying: ATTR_HULL_EM_DAMAGE_RESONANCE, skill: None },
            Row { func: "ItemModifier", domain: "shipID", op: PRE_MUL, modified: ATTR_STRUCTURE_EXPLOSIVE_DAMAGE_RESONANCE, modifying: ATTR_HULL_EXPLOSIVE_DAMAGE_RESONANCE, skill: None },
            Row { func: "ItemModifier", domain: "shipID", op: PRE_MUL, modified: ATTR_STRUCTURE_KINETIC_DAMAGE_RESONANCE, modifying: ATTR_HULL_KINETIC_DAMAGE_RESONANCE, skill: None },
            Row { func: "ItemModifier", domain: "shipID", op: PRE_MUL, modified: ATTR_STRUCTURE_THERMAL_DAMAGE_RESONANCE, modifying: ATTR_HULL_THERMAL_DAMAGE_RESONANCE, skill: None },
            Row { func: "ItemModifier", domain: "shipID", op: PRE_MUL, modified: ATTR_SHIELD_EM_DAMAGE_RESONANCE, modifying: ATTR_SHIELD_EM_DAMAGE_RESONANCE, skill: None },
            Row { func: "ItemModifier", domain: "shipID", op: PRE_MUL, modified: ATTR_SHIELD_EXPLOSIVE_DAMAGE_RESONANCE, modifying: ATTR_SHIELD_EXPLOSIVE_DAMAGE_RESONANCE, skill: None },
            Row { func: "ItemModifier", domain: "shipID", op: PRE_MUL, modified: ATTR_SHIELD_KINETIC_DAMAGE_RESONANCE, modifying: ATTR_SHIELD_KINETIC_DAMAGE_RESONANCE, skill: None },
            Row { func: "ItemModifier", domain: "shipID", op: PRE_MUL, modified: ATTR_SHIELD_THERMAL_DAMAGE_RESONANCE, modifying: ATTR_SHIELD_THERMAL_DAMAGE_RESONANCE, skill: None },
            Row { func: "LocationRequiredSkillModifier", domain: "shipID", op: POST_PERCENT, modified: ATTR_ARMOR_DAMAGE_AMOUNT, modifying: ATTR_ARMOR_DAMAGE_AMOUNT_BONUS, skill: Some(BASTION_SKILL_21803) },
            Row { func: "LocationRequiredSkillModifier", domain: "shipID", op: POST_PERCENT, modified: ATTR_DURATION, modifying: ATTR_ARMOR_DAMAGE_DURATION_BONUS, skill: Some(BASTION_SKILL_21803) },
            Row { func: "LocationRequiredSkillModifier", domain: "shipID", op: POST_PERCENT, modified: ATTR_SHIELD_BONUS, modifying: ATTR_SHIELD_BOOST_MULTIPLIER, skill: Some(BASTION_SKILL_21802) },
            Row { func: "LocationRequiredSkillModifier", domain: "shipID", op: POST_PERCENT, modified: ATTR_DURATION, modifying: ATTR_SHIELD_BONUS_DURATION_BONUS, skill: Some(BASTION_SKILL_21802) },
            Row { func: "LocationRequiredSkillModifier", domain: "shipID", op: POST_PERCENT, modified: ATTR_DAMAGE_MULTIPLIER, modifying: ATTR_DAMAGE_MULTIPLIER_BONUS, skill: Some(BASTION_SKILL_21666) },
            Row { func: "LocationRequiredSkillModifier", domain: "shipID", op: POST_PERCENT, modified: ATTR_TRACKING_SPEED, modifying: ATTR_TRACKING_SPEED_BONUS, skill: Some(BASTION_SKILL_21666) },
            Row { func: "LocationRequiredSkillModifier", domain: "shipID", op: POST_PERCENT, modified: ATTR_DAMAGE_MULTIPLIER, modifying: ATTR_DAMAGE_MULTIPLIER_BONUS, skill: Some(BASTION_SKILL_20327) },
            Row { func: "LocationRequiredSkillModifier", domain: "shipID", op: POST_PERCENT, modified: ATTR_TRACKING_SPEED, modifying: ATTR_TRACKING_SPEED_BONUS, skill: Some(BASTION_SKILL_20327) },
            Row { func: "LocationRequiredSkillModifier", domain: "shipID", op: POST_PERCENT, modified: ATTR_DAMAGE_MULTIPLIER, modifying: ATTR_DAMAGE_MULTIPLIER_BONUS, skill: Some(BASTION_SKILL_21667) },
            Row { func: "LocationRequiredSkillModifier", domain: "shipID", op: POST_PERCENT, modified: ATTR_TRACKING_SPEED, modifying: ATTR_TRACKING_SPEED_BONUS, skill: Some(BASTION_SKILL_21667) },
            Row { func: "LocationRequiredSkillModifier", domain: "shipID", op: POST_PERCENT, modified: ATTR_MAX_RANGE, modifying: ATTR_MAX_RANGE_BONUS, skill: Some(BASTION_SKILL_3309) },
            Row { func: "LocationRequiredSkillModifier", domain: "shipID", op: POST_PERCENT, modified: ATTR_MAX_RANGE, modifying: ATTR_MAX_RANGE_BONUS, skill: Some(BASTION_SKILL_3307) },
            Row { func: "LocationRequiredSkillModifier", domain: "shipID", op: POST_PERCENT, modified: ATTR_MAX_RANGE, modifying: ATTR_MAX_RANGE_BONUS, skill: Some(BASTION_SKILL_3308) },
            Row { func: "LocationRequiredSkillModifier", domain: "shipID", op: POST_PERCENT, modified: ATTR_FALLOFF, modifying: ATTR_FALLOFF_BONUS, skill: Some(BASTION_SKILL_3309) },
            Row { func: "LocationRequiredSkillModifier", domain: "shipID", op: POST_PERCENT, modified: ATTR_FALLOFF, modifying: ATTR_FALLOFF_BONUS, skill: Some(BASTION_SKILL_3307) },
            Row { func: "LocationRequiredSkillModifier", domain: "shipID", op: POST_PERCENT, modified: ATTR_FALLOFF, modifying: ATTR_FALLOFF_BONUS, skill: Some(BASTION_SKILL_3308) },
            Row { func: "LocationRequiredSkillModifier", domain: "shipID", op: POST_PERCENT, modified: ATTR_ARMOR_DAMAGE_AMOUNT, modifying: ATTR_ARMOR_DAMAGE_AMOUNT_BONUS, skill: Some(BASTION_SKILL_3393) },
            Row { func: "LocationRequiredSkillModifier", domain: "shipID", op: POST_PERCENT, modified: ATTR_SHIELD_BONUS, modifying: ATTR_SHIELD_BOOST_MULTIPLIER, skill: Some(BASTION_SKILL_3416) },
            Row { func: "LocationRequiredSkillModifier", domain: "shipID", op: MOD_ADD, modified: ATTR_ACTIVATION_BLOCKED, modifying: ATTR_ACTIVATION_BLOCKED_STRENGTH, skill: Some(BASTION_SKILL_4385) },
            Row { func: "OwnerRequiredSkillModifier", domain: "charID", op: POST_PERCENT, modified: ATTR_MAX_VELOCITY, modifying: ATTR_MISSILE_VELOCITY_BONUS, skill: Some(BASTION_SKILL_3324) },
            Row { func: "OwnerRequiredSkillModifier", domain: "charID", op: POST_PERCENT, modified: ATTR_AOE_VELOCITY, modifying: ATTR_AOE_VELOCITY_BONUS, skill: Some(BASTION_SKILL_32435) },
            Row { func: "OwnerRequiredSkillModifier", domain: "charID", op: POST_PERCENT, modified: ATTR_KINETIC_DAMAGE, modifying: ATTR_DAMAGE_MULTIPLIER_BONUS, skill: Some(BASTION_SKILL_32435) },
            Row { func: "OwnerRequiredSkillModifier", domain: "charID", op: POST_PERCENT, modified: ATTR_EM_DAMAGE, modifying: ATTR_DAMAGE_MULTIPLIER_BONUS, skill: Some(BASTION_SKILL_32435) },
            Row { func: "OwnerRequiredSkillModifier", domain: "charID", op: POST_PERCENT, modified: ATTR_EXPLOSIVE_DAMAGE, modifying: ATTR_DAMAGE_MULTIPLIER_BONUS, skill: Some(BASTION_SKILL_32435) },
            Row { func: "OwnerRequiredSkillModifier", domain: "charID", op: POST_PERCENT, modified: ATTR_EM_DAMAGE, modifying: ATTR_DAMAGE_MULTIPLIER_BONUS, skill: Some(BASTION_SKILL_21668) },
            Row { func: "OwnerRequiredSkillModifier", domain: "charID", op: POST_PERCENT, modified: ATTR_EXPLOSIVE_DAMAGE, modifying: ATTR_DAMAGE_MULTIPLIER_BONUS, skill: Some(BASTION_SKILL_21668) },
            Row { func: "OwnerRequiredSkillModifier", domain: "charID", op: POST_PERCENT, modified: ATTR_KINETIC_DAMAGE, modifying: ATTR_DAMAGE_MULTIPLIER_BONUS, skill: Some(BASTION_SKILL_21668) },
            Row { func: "OwnerRequiredSkillModifier", domain: "charID", op: POST_PERCENT, modified: ATTR_THERMAL_DAMAGE, modifying: ATTR_DAMAGE_MULTIPLIER_BONUS, skill: Some(BASTION_SKILL_21668) },
            Row { func: "OwnerRequiredSkillModifier", domain: "charID", op: POST_PERCENT, modified: ATTR_AOE_VELOCITY, modifying: ATTR_AOE_VELOCITY_BONUS, skill: Some(BASTION_SKILL_21668) },
            Row { func: "OwnerRequiredSkillModifier", domain: "charID", op: POST_PERCENT, modified: ATTR_AOE_FALLOFF, modifying: ATTR_AOE_FALLOFF_BONUS, skill: Some(BASTION_SKILL_21668) },
            Row { func: "OwnerRequiredSkillModifier", domain: "charID", op: POST_PERCENT, modified: ATTR_MAX_VELOCITY, modifying: ATTR_MISSILE_VELOCITY_BONUS, skill: Some(BASTION_SKILL_3325) },
            Row { func: "OwnerRequiredSkillModifier", domain: "charID", op: POST_PERCENT, modified: ATTR_MAX_VELOCITY, modifying: ATTR_MISSILE_VELOCITY_BONUS, skill: Some(BASTION_SKILL_3326) },
        ],
        // eve/common/script/dogma/pythonEffects/microJumpDrive.py: BaseMicroJumpDrive.
        Some("microJumpDrive") => &[
            Row { func: "ItemModifier", domain: "shipID", op: POST_PERCENT, modified: ATTR_SIGNATURE_RADIUS, modifying: ATTR_SIGNATURE_RADIUS_BONUS_PERCENT, skill: None },
        ],
        // eve/common/script/dogma/pythonEffects/propulsionModules.py: Afterburner.
        Some("moduleBonusAfterburner") => &[
            Row { func: "ItemModifier", domain: "shipID", op: MOD_ADD, modified: ATTR_MASS, modifying: ATTR_MASS_ADDITION, skill: None },
        ],
        // eve/common/script/dogma/pythonEffects/propulsionModules.py: Microwarpdrive.
        Some("moduleBonusMicrowarpdrive") => &[
            Row { func: "ItemModifier", domain: "shipID", op: MOD_ADD, modified: ATTR_MASS, modifying: ATTR_MASS_ADDITION, skill: None },
            Row { func: "ItemModifier", domain: "shipID", op: POST_PERCENT, modified: ATTR_SIGNATURE_RADIUS, modifying: ATTR_SIGNATURE_RADIUS_BONUS, skill: None },
        ],
        // eve/common/script/dogma/pythonEffects/remoteTrackingComputer.py: BaseRemoteTrackingComputer.
        Some("shipModuleRemoteTrackingComputer") => &[
            Row { func: "LocationRequiredSkillModifier", domain: "targetID", op: POST_PERCENT, modified: ATTR_MAX_RANGE, modifying: ATTR_MAX_RANGE_BONUS, skill: Some(TYPE_GUNNERY) },
            Row { func: "LocationRequiredSkillModifier", domain: "targetID", op: POST_PERCENT, modified: ATTR_FALLOFF, modifying: ATTR_FALLOFF_BONUS, skill: Some(TYPE_GUNNERY) },
            Row { func: "LocationRequiredSkillModifier", domain: "targetID", op: POST_PERCENT, modified: ATTR_TRACKING_SPEED, modifying: ATTR_TRACKING_SPEED_BONUS, skill: Some(TYPE_GUNNERY) },
        ],
        // eve/common/script/dogma/pythonEffects/weaponDisruptionUtil.py: GetTrackingDisruptionAttributes.
        Some("shipModuleTrackingDisruptor" | "npcBehaviorTrackingDisruptor" | "npcEntityTrackingDisruptor") => &[
            Row { func: "LocationRequiredSkillModifier", domain: "targetID", op: POST_PERCENT, modified: ATTR_MAX_RANGE, modifying: ATTR_MAX_RANGE_BONUS, skill: Some(TYPE_GUNNERY) },
            Row { func: "LocationRequiredSkillModifier", domain: "targetID", op: POST_PERCENT, modified: ATTR_FALLOFF, modifying: ATTR_FALLOFF_BONUS, skill: Some(TYPE_GUNNERY) },
            Row { func: "LocationRequiredSkillModifier", domain: "targetID", op: POST_PERCENT, modified: ATTR_TRACKING_SPEED, modifying: ATTR_TRACKING_SPEED_BONUS, skill: Some(TYPE_GUNNERY) },
        ],
        // eve/common/script/dogma/pythonEffects/weaponDisruptionUtil.py: GetGuidanceDisruptionAttributes.
        Some("shipModuleGuidanceDisruptor" | "npcBehaviorGuidanceDisruptor") => &[
            Row { func: "LocationRequiredSkillModifier", domain: "targetID", op: POST_PERCENT, modified: ATTR_MAX_VELOCITY, modifying: ATTR_MISSILE_VELOCITY_BONUS, skill: Some(TYPE_MISSILE_LAUNCHER_OPERATION) },
            Row { func: "LocationRequiredSkillModifier", domain: "targetID", op: POST_PERCENT, modified: ATTR_EXPLOSION_DELAY, modifying: ATTR_EXPLOSION_DELAY_BONUS, skill: Some(TYPE_MISSILE_LAUNCHER_OPERATION) },
            Row { func: "LocationRequiredSkillModifier", domain: "targetID", op: POST_PERCENT, modified: ATTR_AOE_VELOCITY, modifying: ATTR_AOE_VELOCITY_BONUS, skill: Some(TYPE_MISSILE_LAUNCHER_OPERATION) },
            Row { func: "LocationRequiredSkillModifier", domain: "targetID", op: POST_PERCENT, modified: ATTR_AOE_CLOUD_SIZE, modifying: ATTR_AOE_CLOUD_SIZE_BONUS, skill: Some(TYPE_MISSILE_LAUNCHER_OPERATION) },
        ],
        // weaponDisruptionUtil.py: GetWeaponDisruptionAttributes (ship target).
        Some("npcEntityWeaponDisruptor") => &[
            Row { func: "LocationRequiredSkillModifier", domain: "targetID", op: POST_PERCENT, modified: ATTR_MAX_RANGE, modifying: ATTR_MAX_RANGE_BONUS, skill: Some(TYPE_GUNNERY) },
            Row { func: "LocationRequiredSkillModifier", domain: "targetID", op: POST_PERCENT, modified: ATTR_FALLOFF, modifying: ATTR_FALLOFF_BONUS, skill: Some(TYPE_GUNNERY) },
            Row { func: "LocationRequiredSkillModifier", domain: "targetID", op: POST_PERCENT, modified: ATTR_TRACKING_SPEED, modifying: ATTR_TRACKING_SPEED_BONUS, skill: Some(TYPE_GUNNERY) },
            Row { func: "LocationRequiredSkillModifier", domain: "targetID", op: POST_PERCENT, modified: ATTR_MAX_VELOCITY, modifying: ATTR_MISSILE_VELOCITY_BONUS, skill: Some(TYPE_MISSILE_LAUNCHER_OPERATION) },
            Row { func: "LocationRequiredSkillModifier", domain: "targetID", op: POST_PERCENT, modified: ATTR_EXPLOSION_DELAY, modifying: ATTR_EXPLOSION_DELAY_BONUS, skill: Some(TYPE_MISSILE_LAUNCHER_OPERATION) },
            Row { func: "LocationRequiredSkillModifier", domain: "targetID", op: POST_PERCENT, modified: ATTR_AOE_VELOCITY, modifying: ATTR_AOE_VELOCITY_BONUS, skill: Some(TYPE_MISSILE_LAUNCHER_OPERATION) },
            Row { func: "LocationRequiredSkillModifier", domain: "targetID", op: POST_PERCENT, modified: ATTR_AOE_CLOUD_SIZE, modifying: ATTR_AOE_CLOUD_SIZE_BONUS, skill: Some(TYPE_MISSILE_LAUNCHER_OPERATION) },
        ],
        // eve/common/script/dogma/pythonEffects/migratedeffects.py: MissileEMDmgBonus.
        Some("missileEMDmgBonus") => &[
            Row { func: "OwnerRequiredSkillModifier", domain: "charID", op: POST_PERCENT, modified: ATTR_EM_DAMAGE, modifying: ATTR_DAMAGE_MULTIPLIER_BONUS, skill: Some(SOURCE_ITEM_TYPE) },
        ],
        // eve/common/script/dogma/pythonEffects/migratedeffects.py: MissileExplosiveDmgBonus.
        Some("missileExplosiveDmgBonus") => &[
            Row { func: "OwnerRequiredSkillModifier", domain: "charID", op: POST_PERCENT, modified: ATTR_EXPLOSIVE_DAMAGE, modifying: ATTR_DAMAGE_MULTIPLIER_BONUS, skill: Some(SOURCE_ITEM_TYPE) },
        ],
        // eve/common/script/dogma/pythonEffects/migratedeffects.py: MissileKineticDmgBonus2.
        Some("missileKineticDmgBonus2") => &[
            Row { func: "OwnerRequiredSkillModifier", domain: "charID", op: POST_PERCENT, modified: ATTR_KINETIC_DAMAGE, modifying: ATTR_DAMAGE_MULTIPLIER_BONUS, skill: Some(SOURCE_ITEM_TYPE) },
        ],
        // eve/common/script/dogma/pythonEffects/migratedeffects.py: MissileThermalDmgBonus.
        Some("missileThermalDmgBonus") => &[
            Row { func: "OwnerRequiredSkillModifier", domain: "charID", op: POST_PERCENT, modified: ATTR_THERMAL_DAMAGE, modifying: ATTR_DAMAGE_MULTIPLIER_BONUS, skill: Some(SOURCE_ITEM_TYPE) },
        ],
        // eve/common/script/dogma/pythonEffects/migratedeffects.py: DroneDmgBonus.
        Some("droneDmgBonus") => &[
            Row { func: "OwnerRequiredSkillModifier", domain: "charID", op: POST_PERCENT, modified: ATTR_DAMAGE_MULTIPLIER, modifying: ATTR_DAMAGE_MULTIPLIER_BONUS, skill: Some(SOURCE_ITEM_TYPE) },
        ],
        // eve/common/script/dogma/pythonEffects/migratedeffects.py: CloakingTargetingDelayBonusPostPercentCloakingTargetingDelayBonusForShipModulesRequiringCloaking.
        Some("cloakingTargetingDelayBonusPostPercentCloakingTargetingDelayBonusForShipModulesRequiringCloaking") => &[
            Row { func: "LocationRequiredSkillModifier", domain: "shipID", op: POST_PERCENT, modified: ATTR_CLOAKING_TARGETING_DELAY, modifying: ATTR_CLOAKING_TARGETING_DELAY_BONUS, skill: Some(SOURCE_ITEM_TYPE) },
        ],
        // eve/common/script/dogma/pythonEffects/migratedeffects.py: SelfRof.
        Some("selfRof") => &[
            Row { func: "LocationRequiredSkillModifier", domain: "shipID", op: POST_PERCENT, modified: ATTR_RATE_OF_FIRE, modifying: ATTR_ROF_BONUS, skill: Some(SOURCE_ITEM_TYPE) },
        ],
        _ => &[],
    };
    let disruption = matches!(effect.name.as_deref(), Some("shipModuleTrackingDisruptor" | "npcBehaviorTrackingDisruptor" | "npcEntityTrackingDisruptor" | "shipModuleGuidanceDisruptor" | "npcBehaviorGuidanceDisruptor" | "npcEntityWeaponDisruptor"));
    for r in rows {
        effect.modifiers.push(Modifier {
            func: r.func.into(), domain: r.domain.into(), op: r.op, modified: r.modified,
            modifying: r.modifying, group: None, skill: r.skill,
            target_category: disruption.then_some(CATEGORY_SHIP),
        });
        if disruption {
            let modified = match r.modifying {
                ATTR_MISSILE_VELOCITY_BONUS => ATTR_MISSILE_ENTITY_VELOCITY_MULTIPLIER,
                ATTR_EXPLOSION_DELAY_BONUS => ATTR_MISSILE_ENTITY_FLIGHT_TIME_MULTIPLIER,
                ATTR_AOE_VELOCITY_BONUS => ATTR_MISSILE_ENTITY_AOE_VELOCITY_MULTIPLIER,
                ATTR_AOE_CLOUD_SIZE_BONUS => ATTR_MISSILE_ENTITY_AOE_CLOUD_SIZE_MULTIPLIER,
                _ => r.modified,
            };
            for category in [CATEGORY_ENTITY, CATEGORY_DRONE] {
                effect.modifiers.push(Modifier {
                    func: "ItemModifier".into(), domain: "targetID".into(), op: r.op,
                    modified, modifying: r.modifying, group: None, skill: None,
                    target_category: Some(category),
                });
            }
        }
    }
}

#[cfg(test)] mod tests;
