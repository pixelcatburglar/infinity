//! Game-data constants shared by server modules, grouped by source.
//!
//! IDs follow the SDE and the client's inventorycommon, dogma and corporation
//! constants. FILETIME durations use Windows 100 ns ticks; AU is in metres.
//! Runtime configuration, formulas and module-owned wire protocol constants do
//! not belong here. Shared gateway protocol units are included. This module
//! provides no data lookup or runtime state.

/// `dogma/const.py` dgmGroupableGroupIDs: weapon groups permitted in banks.
pub const GROUPABLE_GROUP_IDS: [i64; 19] = [53, 55, 56, 74, 506, 507, 508, 509, 510, 511, 512, 524, 771, 1245, 1327, 1328, 1562, 1673, 1674];

// FILETIME durations and timestamps.
/// Windows FILETIME epoch offset: 100 ns ticks from 1601-01-01 to 1970-01-01.
pub const FILETIME_UNIX_EPOCH: i64 = 116_444_736_000_000_000;
/// Windows FILETIME ticks per second (100 ns per tick).
pub const FILETIME_SECOND: i64 = 10_000_000;
/// Client blue time convention: FILETIME ticks per minute.
pub const FILETIME_MINUTE: i64 = 60 * FILETIME_SECOND;
/// Client blue time convention: FILETIME ticks per hour.
pub const FILETIME_HOUR: i64 = 60 * FILETIME_MINUTE;
/// Client blue time convention: FILETIME ticks per day.
pub const FILETIME_DAY: i64 = 24 * FILETIME_HOUR;
/// Server subscription policy: expiry at 2100-12-31, expressed as FILETIME.
pub const FAR_FUTURE: i64 = 157_766_976_000_000_000;
// Standard time units.
/// Standard time unit: seconds in a minute.
pub const SECONDS_PER_MINUTE: i64 = 60;
/// Standard time unit: seconds in an hour.
pub const SECONDS_PER_HOUR: i64 = 3600;

// Existing damage representation.
/// SDE/dogma damage attribute grouping: EM, thermal, kinetic and explosive.
pub const DAMAGE_TYPE_COUNT: usize = 4;
/// Ship damage layer model: shield, armor and structure.
pub const DAMAGE_LAYER_COUNT: usize = 3;


// Distances.
/// Server distance convention: the IAU astronomical unit, in metres.
pub const AU: f64 = 149_597_870_700.0;

// SDE inventory type, group and category IDs.
/// SDE invTypes: Character (Gallente), used for player owners.
pub const TYPE_CHARACTER: i32 = 1373;
/// SDE invTypes: Solar System.
pub const TYPE_SOLAR_SYSTEM: i32 = 5;
/// SDE invTypes: Capsule.
pub const TYPE_CAPSULE: i32 = 670;
/// SDE invGroups: Planet.
pub const GROUP_PLANET: i32 = 7;
/// SDE invGroups: Asteroid Belt.
pub const GROUP_ASTEROID_BELT: i32 = 9;
/// SDE invGroups: Wreck.
pub const GROUP_WRECK: i32 = 186;
/// SDE invGroups: Cargo Container (groupCargoContainer).
pub const GROUP_CARGO_CONTAINER: i32 = 12;
/// SDE invGroups: Secure Cargo Container (groupSecureCargoContainer).
pub const GROUP_SECURE_CARGO_CONTAINER: i32 = 340;
/// SDE invGroups: Audit Log Secure Container (groupAuditLogSecureContainer).
pub const GROUP_AUDIT_LOG_SECURE_CONTAINER: i32 = 448;
/// SDE invGroups: Freight Container (groupFreightContainer).
pub const GROUP_FREIGHT_CONTAINER: i32 = 649;
/// Cargo-container groups recognized by inventory and repair.
pub const CARGO_CONTAINER_GROUPS: [i32; 4] = [GROUP_CARGO_CONTAINER, GROUP_SECURE_CARGO_CONTAINER, GROUP_AUDIT_LOG_SECURE_CONTAINER, GROUP_FREIGHT_CONTAINER];
/// SDE invCategories: Owner.
pub const CATEGORY_OWNER: i32 = 1;
/// SDE invCategories: Ship.
pub const CATEGORY_SHIP: i32 = 6;
/// SDE invCategories: Module.
pub const CATEGORY_MODULE: i32 = 7;
/// SDE invCategories: Charge.
pub const CATEGORY_CHARGE: i32 = 8;
/// SDE invCategories: Entity.
pub const CATEGORY_ENTITY: i32 = 11;
/// SDE invCategories: Skill.
pub const CATEGORY_SKILL: i32 = 16;
/// SDE invCategories: Drone.
pub const CATEGORY_DRONE: i32 = 18;
/// SDE invCategories: Subsystem.
pub const CATEGORY_SUBSYSTEM: i32 = 32;
/// SDE invTypes: Hacking skill.
pub const SKILL_HACKING: i32 = 21718;
/// SDE invTypes: Archaeology skill.
pub const SKILL_ARCHAEOLOGY: i32 = 13278;

// SDE faction IDs.
/// SDE chrFactions: Caldari State.
pub const FACTION_CALDARI: i64 = 500001;
/// SDE chrFactions: Minmatar Republic.
pub const FACTION_MINMATAR: i64 = 500002;
/// SDE chrFactions: Amarr Empire.
pub const FACTION_AMARR: i64 = 500003;
/// SDE chrFactions: Gallente Federation.
pub const FACTION_GALLENTE: i64 = 500004;
/// SDE chrFactions: Guristas Pirates.
pub const FACTION_GURISTAS: i64 = 500010;
/// SDE chrFactions: Angel Cartel.
pub const FACTION_ANGEL_CARTEL: i64 = 500011;

// SDE dogma attribute and effect IDs.
/// SDE dgmEffects: skillEffect, skill-point bookkeeping rather than a modifier.
pub const EFFECT_SKILL: i32 = 132;
/// SDE dgmAttributeTypes: skillTimeConstant.
pub const ATTR_SKILL_TIME_CONSTANT: i32 = 275;
/// SDE dgmAttributeTypes: skillPoints.
pub const ATTR_SKILL_POINTS: i32 = 276;
/// SDE dgmAttributeTypes: skillLevel.
pub const ATTR_SKILL_LEVEL: i32 = 280;
/// Client dogma/const.py: requiredSkill1 through requiredSkill6.
pub const ATTR_REQUIRED_SKILLS: [i32; 6] = [ATTR_REQUIRED_SKILL_1, 183, 184, 1285, 1289, 1290];
/// Client dogma/const.py: requiredSkill1Level through requiredSkill6Level.
pub const ATTR_REQUIRED_SKILL_LEVELS: [i32; 6] = [277, 278, 279, 1286, 1287, 1288];
/// Client dogma/const.py: attributeCpu.
pub const ATTR_CPU: i32 = 50;
/// Client dogma/const.py: attributeCpuLoad.
pub const ATTR_CPU_LOAD: i32 = 49;
/// Client dogma/const.py: attributeCpuOutput.
pub const ATTR_CPU_OUTPUT: i32 = 48;
/// Client dogma/const.py: attributePower.
pub const ATTR_POWER: i32 = 30;
/// Client dogma/const.py: attributePowerLoad.
pub const ATTR_POWER_LOAD: i32 = 15;
/// Client dogma/const.py: attributePowerOutput.
pub const ATTR_POWER_OUTPUT: i32 = 11;
/// Client dogma/const.py: attributeTurretSlotsLeft.
pub const ATTR_TURRET_SLOTS_LEFT: i32 = 102;
/// Client dogma/const.py: attributeLauncherSlotsLeft.
pub const ATTR_LAUNCHER_SLOTS_LEFT: i32 = 101;
/// Client dogma/const.py: attributeUpgradeLoad.
pub const ATTR_UPGRADE_LOAD: i32 = 1152;
/// Client dogma/const.py: attributeUpgradeCost.
pub const ATTR_UPGRADE_COST: i32 = 1153;
/// Client dogma/const.py: attributeUpgradeCapacity.
pub const ATTR_UPGRADE_CAPACITY: i32 = 1132;
/// Client dogma/const.py: attributeUpgradeSlotsLeft.
pub const ATTR_UPGRADE_SLOTS_LEFT: i32 = 1154;
/// Client dogma/const.py: attributeSlots.
pub const ATTR_SLOTS: i32 = 47;
/// Client dogma/const.py: effectTurretFitted.
pub const EFFECT_TURRET_FITTED: i32 = 42;
/// Client dogma/const.py: effectLauncherFitted.
pub const EFFECT_LAUNCHER_FITTED: i32 = 40;
/// Client dogma/const.py: effectRigSlot.
pub const EFFECT_RIG_SLOT: i32 = 2663;

/// SDE dgmAttributeTypes: damageMultiplier.
pub const ATTR_DAMAGE_MULTIPLIER: i32 = 64;
/// SDE dgmAttributeTypes: rateOfFire.
pub const ATTR_RATE_OF_FIRE: i32 = 51;
/// SDE dgmAttributeTypes: emDamage.
pub const ATTR_EM_DAMAGE: i32 = 114;
/// SDE dgmAttributeTypes: thermalDamage.
pub const ATTR_THERMAL_DAMAGE: i32 = 118;
/// SDE dgmAttributeTypes: kineticDamage.
pub const ATTR_KINETIC_DAMAGE: i32 = 117;
/// SDE dgmAttributeTypes: explosiveDamage.
pub const ATTR_EXPLOSIVE_DAMAGE: i32 = 116;
/// SDE dgmAttributeTypes: mass.
pub const ATTR_MASS: i32 = 4;
/// SDE dgmAttributeTypes: hp (structure hitpoint capacity).
pub const ATTR_STRUCTURE_HP: i32 = 9;
/// SDE dgmAttributeTypes: capacity.
pub const ATTR_CAPACITY: i32 = 38;
/// SDE dgmAttributeTypes: volume.
pub const ATTR_VOLUME: i32 = 161;
/// SDE dgmAttributeTypes: radius.
pub const ATTR_RADIUS: i32 = 162;
/// SDE dgmAttributeTypes: maxVelocity (also used for missiles).
pub const ATTR_MAX_VELOCITY: i32 = 37;
/// SDE dgmAttributeTypes: speedFactor.
pub const ATTR_SPEED_FACTOR: i32 = 20;
/// Client dogma/const.py: attributeSpeedFactorInterim.
pub const ATTR_SPEED_FACTOR_INTERIM: i32 = 2148;
/// Client dogma/const.py: attributeWarpScrambleStatus.
pub const ATTR_WARP_SCRAMBLE_STATUS: i32 = 104;
/// Client dogma/const.py: attributeActivationBlocked.
pub const ATTR_ACTIVATION_BLOCKED: i32 = 1349;
/// Client dogma/const.py: attributeActivationBlockedStrenght (client spelling).
pub const ATTR_ACTIVATION_BLOCKED_STRENGTH: i32 = 1350;
/// SDE dgmAttributeTypes: agility.
pub const ATTR_AGILITY: i32 = 70;
/// SDE dgmAttributeTypes: signatureRadius.
pub const ATTR_SIGNATURE_RADIUS: i32 = 552;
/// SDE dgmAttributeTypes: maxRange.
pub const ATTR_MAX_RANGE: i32 = 54;
/// SDE dgmAttributeTypes: quantity.
pub const ATTR_QUANTITY: i32 = 805;
/// SDE dgmAttributeTypes: isOnline.
pub const ATTR_IS_ONLINE: i32 = 2;
/// SDE dgmAttributeTypes: explosionDelay (also used for probe lifetime).
pub const ATTR_EXPLOSION_DELAY: i32 = 281;
/// SDE dgmAttributeTypes: capacitorCapacity.
pub const ATTR_CAPACITOR_CAPACITY: i32 = 482;
/// Client dogma/const.py: attributeCapacitorBonus.
pub const ATTR_CAPACITOR_BONUS: i32 = 67;
/// Client dogma/const.py: attributeEnergyNeutralizerAmount.
pub const ATTR_ENERGY_NEUTRALIZER_AMOUNT: i32 = 97;
/// Client dogma/const.py: attributePowerTransferAmount.
pub const ATTR_POWER_TRANSFER_AMOUNT: i32 = 90;
/// Client dogma/const.py: attributeNosOverride.
pub const ATTR_NOS_OVERRIDE: i32 = 1945;
/// SDE dgmAttributeTypes: shieldCapacity.
pub const ATTR_SHIELD_CAPACITY: i32 = 263;
/// SDE dgmAttributeTypes: charge.
pub const ATTR_CHARGE: i32 = 18;
/// SDE dgmAttributeTypes: shieldCharge.
pub const ATTR_SHIELD_CHARGE: i32 = 264;
/// SDE dgmAttributeTypes: armorDamage.
pub const ATTR_ARMOR_DAMAGE: i32 = 266;
/// SDE dgmAttributeTypes: damage (hull damage), not hitpoint capacity hp (9).
pub const ATTR_HULL_DAMAGE: i32 = 3;
/// SDE dgmAttributeTypes: requiredSkill1.
pub const ATTR_REQUIRED_SKILL_1: i32 = 182;
/// SDE dgmEffects: online; an effect ID, not an effect category.
pub const EFFECT_ONLINE: i64 = 16;
/// SDE dgmEffects: moduleBonusMicrowarpdrive.
pub const EFFECT_MICROWARPDRIVE: i32 = 6730;
/// SDE dgmEffects: moduleBonusAfterburner.
pub const EFFECT_AFTERBURNER: i32 = 6731;
/// SDE dgmEffects: surveyScan; fittingScreen/pythonEffects.py: FakeSurveyScan.
pub const EFFECT_SURVEY_SCAN: i32 = 81;

/// SDE dgmEffects: targetAttack.
pub const EFFECT_TARGET_ATTACK: i32 = 10;
/// SDE dgmEffects: projectileFired.
pub const EFFECT_PROJECTILE_FIRED: i32 = 34;
/// SDE dgmEffects: targetDisintegratorAttack.
pub const EFFECT_TARGET_DISINTEGRATOR_ATTACK: i32 = 6995;
/// SDE dgmEffects: useMissiles.
pub const EFFECT_USE_MISSILES: i32 = 101;
/// SDE dgmEffects: shieldBoosting.
pub const EFFECT_SHIELD_BOOSTING: i32 = 4;
/// SDE dgmEffects: armorRepair.
pub const EFFECT_ARMOR_REPAIR: i32 = 27;
/// SDE dgmEffects: structureRepair.
pub const EFFECT_STRUCTURE_REPAIR: i32 = 26;
/// SDE dgmEffects: miningLaser.
pub const EFFECT_MINING_LASER: i32 = 67;
/// SDE dgmEffects: miningClouds.
pub const EFFECT_MINING_CLOUDS: i32 = 2726;
/// SDE dgmEffects: mining.
pub const EFFECT_MINING: i32 = 17;
/// SDE dgmEffects: doHacking.
pub const EFFECT_HACKING: i32 = 1738;
/// SDE dgmEffects: probeLaunching.
pub const EFFECT_PROBE_LAUNCHING: i32 = 3793;
/// SDE dgmEffects: empWave.
pub const EFFECT_EMP_WAVE: i32 = 38;
/// SDE dgmEffects: ChainLightning.
pub const EFFECT_CHAIN_LIGHTNING: i32 = 8037;
/// SDE dgmEffects: fueledArmorRepair.
pub const EFFECT_FUELED_ARMOR_REPAIR: i32 = 5275;
/// SDE dgmEffects: fueledShieldBoosting.
pub const EFFECT_FUELED_SHIELD_BOOSTING: i32 = 4936;
/// SDE dgmEffects: powerBooster.
pub const EFFECT_POWER_BOOSTER: i32 = 48;
/// SDE dgmEffects: energyNeutralizerFalloff.
pub const EFFECT_ENERGY_NEUTRALIZER_FALLOFF: i32 = 6187;
/// SDE dgmEffects: entityEnergyNeutralizerFalloff.
pub const EFFECT_ENTITY_ENERGY_NEUTRALIZER_FALLOFF: i32 = 6691;
/// SDE dgmEffects: energyNosferatuFalloff.
pub const EFFECT_ENERGY_NOSFERATU_FALLOFF: i32 = 6197;
/// SDE dgmEffects: shipModuleRemoteCapacitorTransmitter.
pub const EFFECT_REMOTE_CAPACITOR_TRANSMITTER: i32 = 6184;
/// SDE dgmEffects: shipModuleRemoteArmorRepairer.
pub const EFFECT_REMOTE_ARMOR_REPAIRER: i32 = 6188;
/// SDE dgmEffects: shipModuleRemoteHullRepairer.
pub const EFFECT_REMOTE_HULL_REPAIRER: i32 = 6185;
/// SDE dgmEffects: shipModuleRemoteShieldBooster.
pub const EFFECT_REMOTE_SHIELD_BOOSTER: i32 = 6186;
/// SDE dgmEffects: npcEntityRemoteArmorRepairer.
pub const EFFECT_NPC_REMOTE_ARMOR_REPAIRER: i32 = 6687;
/// SDE dgmEffects: npcEntityRemoteHullRepairer.
pub const EFFECT_NPC_REMOTE_HULL_REPAIRER: i32 = 6689;
/// SDE dgmEffects: npcEntityRemoteShieldBooster.
pub const EFFECT_NPC_REMOTE_SHIELD_BOOSTER: i32 = 6688;
/// SDE dgmEffects: shipModuleAncillaryRemoteArmorRepairer.
pub const EFFECT_ANCILLARY_REMOTE_ARMOR_REPAIRER: i32 = 6651;
/// SDE dgmEffects: shipModuleAncillaryRemoteShieldBooster.
pub const EFFECT_ANCILLARY_REMOTE_SHIELD_BOOSTER: i32 = 6652;
/// SDE dgmEffects: ShipModuleRemoteArmorMutadaptiveRepairer.
pub const EFFECT_MUTADAPTIVE_REMOTE_ARMOR_REPAIRER: i32 = 7166;
/// SDE dgmEffects: remoteECMFalloff.
pub const EFFECT_REMOTE_ECM_FALLOFF: i32 = 6470;
/// SDE dgmEffects: entityECMFalloff.
pub const EFFECT_ENTITY_ECM_FALLOFF: i32 = 6695;
/// SDE dgmEffects: ECMBurstJammer.
pub const EFFECT_ECM_BURST_JAMMER: i32 = 6714;
/// SDE dgmEffects: cloaking.
pub const EFFECT_CLOAKING: i32 = 607;
/// SDE dgmEffects: cloakingPrototype.
pub const EFFECT_CLOAKING_PROTOTYPE: i32 = 5945;
/// SDE dgmEffects: cloakingWarpSafe.
pub const EFFECT_CLOAKING_WARP_SAFE: i32 = 980;
/// SDE dgmEffects: microJumpDrive.
pub const EFFECT_MICRO_JUMP_DRIVE: i32 = 4921;
/// SDE dgmEffects: salvaging.
pub const EFFECT_SALVAGING: i32 = 2757;
/// SDE dgmEffects: salvageDroneEffect.
pub const EFFECT_SALVAGE_DRONE: i32 = 5163;
/// SDE dgmEffects: tractorBeamCan.
pub const EFFECT_TRACTOR_BEAM: i32 = 2255;
/// SDE dgmEffects: cargoScan.
pub const EFFECT_CARGO_SCAN: i32 = 47;
/// SDE dgmEffects: shipScan.
pub const EFFECT_SHIP_SCAN: i32 = 46;
/// SDE dgmEffects: targetHostiles.
pub const EFFECT_TARGET_HOSTILES: i32 = 55;
/// SDE dgmEffects: targetPassively.
pub const EFFECT_TARGET_PASSIVELY: i32 = 54;
/// SDE dgmEffects: bombLaunching.
pub const EFFECT_BOMB_LAUNCHING: i32 = 2971;
/// SDE dgmEffects: remoteWebifierFalloff.
pub const EFFECT_REMOTE_WEBIFIER_FALLOFF: i32 = 6426;
/// SDE dgmEffects: remoteWebifierEntity.
pub const EFFECT_REMOTE_WEBIFIER_ENTITY: i32 = 6690;
/// SDE dgmEffects: warpDisrupt.
pub const EFFECT_WARP_DISRUPT: i32 = 39;
/// SDE dgmEffects: warpScrambleBlockMWDWithNPCEffect.
pub const EFFECT_WARP_SCRAMBLE: i32 = 5934;
/// SDE dgmEffects: remoteTargetPaintFalloff.
pub const EFFECT_REMOTE_TARGET_PAINT_FALLOFF: i32 = 6425;
/// SDE dgmEffects: remoteTargetPaintEntity.
pub const EFFECT_REMOTE_TARGET_PAINT_ENTITY: i32 = 6692;
/// SDE dgmEffects: remoteSensorDampFalloff.
pub const EFFECT_REMOTE_SENSOR_DAMP_FALLOFF: i32 = 6422;
/// SDE dgmEffects: remoteSensorDampEntity.
pub const EFFECT_REMOTE_SENSOR_DAMP_ENTITY: i32 = 6693;
/// SDE dgmEffects: shipModuleTrackingDisruptor.
pub const EFFECT_TRACKING_DISRUPTOR: i32 = 6424;
/// SDE dgmEffects: npcEntityWeaponDisruptor.
pub const EFFECT_NPC_WEAPON_DISRUPTOR: i32 = 6694;
/// SDE dgmEffects: shipModuleGuidanceDisruptor.
pub const EFFECT_GUIDANCE_DISRUPTOR: i32 = 6423;
/// SDE dgmEffects: remoteSensorBoostFalloff.
pub const EFFECT_REMOTE_SENSOR_BOOST: i32 = 6427;
/// SDE dgmEffects: shipModuleRemoteTrackingComputer.
pub const EFFECT_REMOTE_TRACKING_COMPUTER: i32 = 6428;
/// SDE dgmEffects: cynosuralGeneration.
pub const EFFECT_CYNO_GENERATION: i32 = 2857;
/// SDE dgmEffects: cloneJumpAccepting.
pub const EFFECT_CLONE_JUMP_ACCEPTING: i32 = 2858;
/// SDE dgmEffects: industrialCompactCoreEffect2.
pub const EFFECT_INDUSTRIAL_COMPACT_CORE: i32 = 8119;
/// SDE dgmEffects: industrialCoreEffect2.
pub const EFFECT_INDUSTRIAL_CORE: i32 = 4575;
/// SDE dgmEffects: jumpPortalGeneration.
pub const EFFECT_JUMP_PORTAL: i32 = 2152;
/// SDE dgmEffects: jumpPortalGenerationBO.
pub const EFFECT_JUMP_PORTAL_BO: i32 = 3674;
/// SDE dgmEffects: moduleBonusBastionModule.
pub const EFFECT_BASTION: i32 = 6658;
/// SDE dgmEffects: moduleBonusSacribisBastion.
pub const EFFECT_SACRIBIS_BASTION: i32 = 11692;
/// SDE dgmEffects: moduleBonusSiegeModule.
pub const EFFECT_SIEGE: i32 = 6582;
/// SDE dgmEffects: moduleBonusTriageModule.
pub const EFFECT_TRIAGE: i32 = 6581;
/// SDE dgmEffects: moduleBonusStellianTriageModule.
pub const EFFECT_STELLIAN_TRIAGE: i32 = 12248;
/// SDE dgmEffects: shipModuleFocusedWarpDisruptionScript.
pub const EFFECT_FOCUSED_WARP_DISRUPTION_SCRIPT: i32 = 6849;
/// SDE dgmEffects: shipModuleFocusedWarpScramblingScript.
pub const EFFECT_FOCUSED_WARP_SCRAMBLING_SCRIPT: i32 = 6848;
/// SDE dgmAttributeTypes: falloff.
pub const ATTR_FALLOFF: i32 = 158;
/// SDE dgmAttributeTypes: armor_hp.
pub const ATTR_ARMOR_HP: i32 = 265;
/// SDE dgmAttributeTypes: shield_em_damage_resonance.
pub const ATTR_SHIELD_EM_DAMAGE_RESONANCE: i32 = 271;
/// SDE dgmAttributeTypes: shield_thermal_damage_resonance.
pub const ATTR_SHIELD_THERMAL_DAMAGE_RESONANCE: i32 = 274;
/// SDE dgmAttributeTypes: shield_kinetic_damage_resonance.
pub const ATTR_SHIELD_KINETIC_DAMAGE_RESONANCE: i32 = 273;
/// SDE dgmAttributeTypes: shield_explosive_damage_resonance.
pub const ATTR_SHIELD_EXPLOSIVE_DAMAGE_RESONANCE: i32 = 272;
/// SDE dgmAttributeTypes: armor_em_damage_resonance.
pub const ATTR_ARMOR_EM_DAMAGE_RESONANCE: i32 = 267;
/// SDE dgmAttributeTypes: armor_thermal_damage_resonance.
pub const ATTR_ARMOR_THERMAL_DAMAGE_RESONANCE: i32 = 270;
/// SDE dgmAttributeTypes: armor_kinetic_damage_resonance.
pub const ATTR_ARMOR_KINETIC_DAMAGE_RESONANCE: i32 = 269;
/// SDE dgmAttributeTypes: armor_explosive_damage_resonance.
pub const ATTR_ARMOR_EXPLOSIVE_DAMAGE_RESONANCE: i32 = 268;
/// SDE dgmAttributeTypes: structure_em_damage_resonance.
pub const ATTR_STRUCTURE_EM_DAMAGE_RESONANCE: i32 = 113;
/// SDE dgmAttributeTypes: structure_thermal_damage_resonance.
pub const ATTR_STRUCTURE_THERMAL_DAMAGE_RESONANCE: i32 = 110;
/// SDE dgmAttributeTypes: structure_kinetic_damage_resonance.
pub const ATTR_STRUCTURE_KINETIC_DAMAGE_RESONANCE: i32 = 109;
/// SDE dgmAttributeTypes: structure_explosive_damage_resonance.
pub const ATTR_STRUCTURE_EXPLOSIVE_DAMAGE_RESONANCE: i32 = 111;
/// SDE dgmAttributeTypes: scan_resolution.
pub const ATTR_SCAN_RESOLUTION: i32 = 564;
/// SDE dgmAttributeTypes: max_target_range.
pub const ATTR_MAX_TARGET_RANGE: i32 = 76;
/// SDE dgmAttributeTypes: max_locked_targets.
pub const ATTR_MAX_LOCKED_TARGETS: i32 = 192;
/// SDE dgmAttributeTypes: aoe_cloud_size.
pub const ATTR_AOE_CLOUD_SIZE: i32 = 654;
/// SDE dgmAttributeTypes: aoeVelocity.
pub const ATTR_AOE_VELOCITY: i32 = 653;
/// SDE dgmAttributeTypes: aoeDamageReductionFactor.
pub const ATTR_AOE_DAMAGE_REDUCTION_FACTOR: i32 = 1353;
/// SDE dgmAttributeTypes: optimalSigRadius.
pub const ATTR_OPTIMAL_SIG_RADIUS: i32 = 620;
/// SDE dgmAttributeTypes: recharge_rate.
pub const ATTR_RECHARGE_RATE: i32 = 55;
/// SDE dgmAttributeTypes: capacitor_need.
pub const ATTR_CAPACITOR_NEED: i32 = 6;
/// SDE dgmAttributeTypes: shield_recharge_rate.
pub const ATTR_SHIELD_RECHARGE_RATE: i32 = 479;
/// SDE dgmAttributeTypes: reload_time.
pub const ATTR_RELOAD_TIME: i32 = 1795;
/// SDE dgmAttributeTypes: next_activation_time.
pub const ATTR_NEXT_ACTIVATION_TIME: i32 = 2729;
/// SDE dgmAttributeTypes: missile_damage_multiplier.
pub const ATTR_MISSILE_DAMAGE_MULTIPLIER: i32 = 212;
/// Client dogma/const.py: attributeMissileEntityVelocityMultiplier.
pub const ATTR_MISSILE_ENTITY_VELOCITY_MULTIPLIER: i32 = 645;
/// Client dogma/const.py: attributeMissileEntityFlightTimeMultiplier.
pub const ATTR_MISSILE_ENTITY_FLIGHT_TIME_MULTIPLIER: i32 = 646;
/// Client dogma/const.py: attributeMissileEntityAoeVelocityMultiplier.
pub const ATTR_MISSILE_ENTITY_AOE_VELOCITY_MULTIPLIER: i32 = 859;
/// Client dogma/const.py: attributeMissileEntityAoeCloudSizeMultiplier.
pub const ATTR_MISSILE_ENTITY_AOE_CLOUD_SIZE_MULTIPLIER: i32 = 858;
/// Client dogma/const.py: attributeTrackingSpeed.
pub const ATTR_TRACKING_SPEED: i32 = 160;
/// SDE dgmAttributeTypes: entity_fly_range.
pub const ATTR_ENTITY_FLY_RANGE: i32 = 416;
/// SDE dgmAttributeTypes: entity_attack_range.
pub const ATTR_ENTITY_ATTACK_RANGE: i32 = 247;
/// SDE dgmAttributeTypes: entity_missile_type.
pub const ATTR_ENTITY_MISSILE_TYPE: i32 = 507;
/// SDE dgmAttributeTypes: entity_kill_bounty.
pub const ATTR_ENTITY_KILL_BOUNTY: i32 = 481;
/// SDE dgmAttributeTypes: entity_shield_boost_amount.
pub const ATTR_ENTITY_SHIELD_BOOST_AMOUNT: i32 = 637;
/// SDE dgmAttributeTypes: entity_shield_boost_duration.
pub const ATTR_ENTITY_SHIELD_BOOST_DURATION: i32 = 636;
/// SDE dgmAttributeTypes: entity_armor_repair_amount.
pub const ATTR_ENTITY_ARMOR_REPAIR_AMOUNT: i32 = 631;
/// SDE dgmAttributeTypes: entity_armor_repair_duration.
pub const ATTR_ENTITY_ARMOR_REPAIR_DURATION: i32 = 630;
/// SDE dgmAttributeTypes: shield_bonus.
pub const ATTR_SHIELD_BONUS: i32 = 68;
/// SDE dgmAttributeTypes: armor_damage_amount.
pub const ATTR_ARMOR_DAMAGE_AMOUNT: i32 = 84;
/// SDE dgmAttributeTypes: structure_damage_amount.
pub const ATTR_STRUCTURE_DAMAGE_AMOUNT: i32 = 83;
/// SDE dgmAttributeTypes: charged_armor_damage_multiplier.
pub const ATTR_CHARGED_ARMOR_DAMAGE_MULTIPLIER: i32 = 1886;
/// SDE dgmEffects: npcBehaviorRemoteArmorRepairer.
pub const EFFECT_NPC_BEHAVIOR_REMOTE_ARMOR: i32 = 6741;
/// SDE dgmEffects: npcBehaviorRemoteShieldBooster.
pub const EFFECT_NPC_BEHAVIOR_REMOTE_SHIELD: i32 = 6742;
/// SDE dgmEffects: npcBehaviorRemoteCapacitorTransmitter.
pub const EFFECT_NPC_BEHAVIOR_REMOTE_CAPACITOR: i32 = 12073;
/// SDE dgmEffects: npcBehaviorEnergyNeutralizer.
pub const EFFECT_NPC_BEHAVIOR_NEUTRALIZER: i32 = 6756;
/// SDE dgmEffects: npcBehaviorEnergyNosferatu.
pub const EFFECT_NPC_BEHAVIOR_NOSFERATU: i32 = 6882;
/// SDE dgmEffects: behaviorECM.
pub const EFFECT_NPC_BEHAVIOR_ECM: i32 = 6757;
/// SDE dgmEffects: npcBehaviorMicroWarpDrive.
pub const EFFECT_NPC_BEHAVIOR_MWD: i32 = 6864;
/// SDE dgmEffects: npcBehaviorMiningLaser.
pub const EFFECT_NPC_BEHAVIOR_MINING: i32 = 6901;
/// SDE dgmEffects: npcBehaviorSmartBomb.
pub const EFFECT_NPC_BEHAVIOR_SMARTBOMB: i32 = 7188;
/// SDE dgmEffects: npcBehaviorArmorRepairer.
pub const EFFECT_NPC_BEHAVIOR_ARMOR_REPAIR: i32 = 6884;
/// SDE dgmEffects: npcBehaviorShieldBooster.
pub const EFFECT_NPC_BEHAVIOR_SHIELD_BOOST: i32 = 6990;
/// SDE dgmEffects: npcBehaviorWebifier.
pub const EFFECT_NPC_BEHAVIOR_WEB: i32 = 6743;
/// SDE dgmEffects: npcBehaviorGuidanceDisruptor.
pub const EFFECT_NPC_BEHAVIOR_GUIDANCE_DISRUPTOR: i32 = 6746;
/// SDE dgmEffects: npcBehaviorTrackingDisruptor.
pub const EFFECT_NPC_BEHAVIOR_TRACKING_DISRUPTOR: i32 = 6747;
/// SDE dgmEffects: npcEntityTrackingDisruptor.
pub const EFFECT_NPC_ENTITY_TRACKING_DISRUPTOR: i32 = 6846;
/// SDE dgmEffects: behaviorWarpDisrupt.
pub const EFFECT_NPC_BEHAVIOR_WARP_DISRUPT: i32 = 6744;
/// SDE dgmEffects: behaviorWarpScramble.
pub const EFFECT_NPC_BEHAVIOR_WARP_SCRAMBLE: i32 = 6745;
/// SDE dgmEffects: behaviorTargetPainter.
pub const EFFECT_NPC_BEHAVIOR_PAINTER: i32 = 6754;
/// SDE dgmEffects: behaviorSensorDampener.
pub const EFFECT_NPC_BEHAVIOR_DAMP: i32 = 6755;
/// SDE dgmEffects: warpScrambleForEntity.
pub const EFFECT_NPC_WARP_SCRAMBLE: i32 = 563;
/// SDE dgmEffects: warpScrambleTargetMWDBlockActivationForEntity.
pub const EFFECT_NPC_WARP_SCRAMBLE_MWD: i32 = 5928;
/// SDE dgmEffects: warpDisruptSphere.
pub const EFFECT_WARP_DISRUPT_SPHERE: i32 = 3380;
/// SDE dgmEffects: entosisLink.
pub const EFFECT_ENTOSIS_LINK: i32 = 6063;
/// SDE dgmEffects: snowBallLaunching.
pub const EFFECT_SNOWBALL_LAUNCHING: i32 = 2413;
/// SDE dgmEffects: industrialItemCompression.
pub const EFFECT_INDUSTRIAL_ITEM_COMPRESSION: i32 = 8364;
/// SDE dgmEffects: cloneRespawnBay.
pub const EFFECT_CLONE_RESPAWN_BAY: i32 = 8093;
/// SDE dgmEffects: microJumpPortalDrive.
pub const EFFECT_MICRO_JUMP_PORTAL: i32 = 6208;
/// SDE dgmEffects: microJumpPortalDriveCapital.
pub const EFFECT_MICRO_JUMP_PORTAL_CAPITAL: i32 = 12126;
/// SDE dgmEffects: moduleBonusWarfareLinkArmor.
pub const EFFECT_COMMAND_BURST_ARMOR: i32 = 6732;
/// SDE dgmEffects: moduleBonusWarfareLinkShield.
pub const EFFECT_COMMAND_BURST_SHIELD: i32 = 6733;
/// SDE dgmEffects: moduleBonusWarfareLinkSkirmish.
pub const EFFECT_COMMAND_BURST_SKIRMISH: i32 = 6734;
/// SDE dgmEffects: moduleBonusWarfareLinkInfo.
pub const EFFECT_COMMAND_BURST_INFO: i32 = 6735;
/// SDE dgmEffects: moduleBonusWarfareLinkMining.
pub const EFFECT_COMMAND_BURST_MINING: i32 = 6736;
/// SDE dgmEffects: moduleTitanEffectGenerator.
pub const EFFECT_TITAN_EFFECT_GENERATOR: i32 = 6753;
/// SDE dgmEffects: siegeModeEffect.
pub const EFFECT_SIEGE_MODE: i32 = 4877;
/// SDE dgmEffects: triageMode.
pub const EFFECT_TRIAGE_MODE: i32 = 4839;
/// SDE dgmEffects: triageMode7.
pub const EFFECT_TRIAGE_MODE_7: i32 = 4893;
/// SDE dgmEffects: anchorDrop.
pub const EFFECT_EXCLUDED_649: i32 = 649;
/// SDE dgmEffects: anchorLift.
pub const EFFECT_EXCLUDED_650: i32 = 650;
/// SDE dgmEffects: debuffLance.
pub const EFFECT_EXCLUDED_11691: i32 = 11691;
/// SDE dgmEffects: doomsdayAOEBubble.
pub const EFFECT_EXCLUDED_6482: i32 = 6482;
/// SDE dgmEffects: doomsdayAOEDamp.
pub const EFFECT_EXCLUDED_6481: i32 = 6481;
/// SDE dgmEffects: doomsdayAOEECM.
pub const EFFECT_EXCLUDED_6513: i32 = 6513;
/// SDE dgmEffects: doomsdayAOENeut.
pub const EFFECT_EXCLUDED_6477: i32 = 6477;
/// SDE dgmEffects: doomsdayAOEPaint.
pub const EFFECT_EXCLUDED_6478: i32 = 6478;
/// SDE dgmEffects: doomsdayAOETrack.
pub const EFFECT_EXCLUDED_6479: i32 = 6479;
/// SDE dgmEffects: doomsdayAOEWeb.
pub const EFFECT_EXCLUDED_6476: i32 = 6476;
/// SDE dgmEffects: doomsdayBeamDOT.
pub const EFFECT_EXCLUDED_6472: i32 = 6472;
/// SDE dgmEffects: doomsdayConeDOT.
pub const EFFECT_EXCLUDED_6473: i32 = 6473;
/// SDE dgmEffects: doomsdayHOG.
pub const EFFECT_EXCLUDED_6474: i32 = 6474;
/// SDE dgmEffects: doomsdaySlash.
pub const EFFECT_EXCLUDED_6201: i32 = 6201;
/// SDE dgmEffects: fighterAbilityAfterburner.
pub const EFFECT_EXCLUDED_6440: i32 = 6440;
/// SDE dgmEffects: fighterAbilityEvasiveManeuvers.
pub const EFFECT_EXCLUDED_6439: i32 = 6439;
/// SDE dgmEffects: fighterAbilityLaunchBomb.
pub const EFFECT_EXCLUDED_6485: i32 = 6485;
/// SDE dgmEffects: fighterAbilityMicroJumpDrive.
pub const EFFECT_EXCLUDED_6442: i32 = 6442;
/// SDE dgmEffects: fighterAbilityMicroWarpDrive.
pub const EFFECT_EXCLUDED_6441: i32 = 6441;
/// SDE dgmEffects: moduleBonusIndustrialInvulnerability.
pub const EFFECT_EXCLUDED_6719: i32 = 6719;
/// SDE dgmEffects: fighterAbilityAttackM.
pub const EFFECT_EXCLUDED_6465: i32 = 6465;
/// SDE dgmEffects: fighterAbilityECM.
pub const EFFECT_EXCLUDED_6437: i32 = 6437;
/// SDE dgmEffects: fighterAbilityEnergyNeutralizer.
pub const EFFECT_EXCLUDED_6434: i32 = 6434;
/// SDE dgmEffects: fighterAbilityKamikaze.
pub const EFFECT_EXCLUDED_6554: i32 = 6554;
/// SDE dgmEffects: fighterAbilityMissiles.
pub const EFFECT_EXCLUDED_6431: i32 = 6431;
/// SDE dgmEffects: fighterAbilityStasisWebifier.
pub const EFFECT_EXCLUDED_6435: i32 = 6435;
/// SDE dgmEffects: fighterAbilityTackle.
pub const EFFECT_EXCLUDED_6464: i32 = 6464;
/// SDE dgmEffects: fighterAbilityWarpDisruption.
pub const EFFECT_EXCLUDED_6436: i32 = 6436;
/// SDE dgmEffects: superWeaponAmarr.
pub const EFFECT_EXCLUDED_4489: i32 = 4489;
/// SDE dgmEffects: superWeaponCaldari.
pub const EFFECT_EXCLUDED_4490: i32 = 4490;
/// SDE dgmEffects: superWeaponGallente.
pub const EFFECT_EXCLUDED_4491: i32 = 4491;
/// SDE dgmEffects: superWeaponMinmatar.
pub const EFFECT_EXCLUDED_4492: i32 = 4492;

// Client inventory flags, locations and row-change keys.
/// Client inventorycommon.const.flagHangar.
pub const FLAG_HANGAR: i32 = 4;
/// Client inventorycommon.const.flagCargo.
pub const FLAG_CARGO: i32 = 5;
/// Client inventorycommon.const.flagLoSlot0.
pub const FLAG_FITTING_FIRST: i32 = 11;
/// Client inventorycommon.const.flagMedSlot0.
pub const FLAG_MED_SLOT_FIRST: i32 = 19;
/// Client inventorycommon.const.flagHiSlot0.
pub const FLAG_HI_SLOT_FIRST: i32 = 27;
/// Client inventorycommon.const.flagHiSlot7.
pub const FLAG_FITTING_LAST: i32 = 34;
/// Client inventorycommon.const.flagRigSlot0.
pub const FLAG_RIG_FIRST: i32 = 92;
/// Client inventorycommon.const.flagRigSlot7.
pub const FLAG_RIG_LAST: i32 = 99;
/// Client inventorycommon.const.flagSubSystemSlot0.
pub const FLAG_SUBSYSTEM_FIRST: i32 = 125;
/// Client inventorycommon.const.flagSubSystemSlot7.
pub const FLAG_SUBSYSTEM_LAST: i32 = 132;
/// Client inventorycommon.const.flagPilot.
pub const FLAG_PILOT: i32 = 57;
/// Client inventorycommon.const.flagDroneBay.
pub const FLAG_DRONE_BAY: i32 = 87;
/// Client inventorycommon.const.flagBooster.
pub const FLAG_BOOSTER: i32 = 88;
/// Client inventorycommon.const.flagImplant.
pub const FLAG_IMPLANT: i32 = 89;
/// Client inventorycommon.const.flagCorpSAG1; other divisions follow it.
pub const FLAG_CORP_SAG1: i32 = 115;
/// Client inventorycommon.const.locationJunkyard: deleted items.
pub const LOCATION_JUNKYARD: i64 = 10;
/// Client carbon const.ixOwnerID: inventory row owner column.
pub const IX_OWNER_ID: i64 = 2;
/// Client carbon const.ixLocationID: inventory row location column.
pub const IX_LOCATION_ID: i64 = 3;
/// Client carbon const.ixFlag: inventory row flag column.
pub const IX_FLAG: i64 = 4;
/// Client inventorycommon.const.ixStackSize: inventory stack-size change key.
pub const IX_STACK_SIZE: i64 = 9;
/// Client inventorycommon.const.ixSingleton: inventory singleton change key.
pub const IX_SINGLETON: i64 = 10;

// Client inventory singleton quantity encoding.
/// Client inventory singleton encoding: blueprint copy.
pub const BLUEPRINT_COPY_QUANTITY: i64 = -2;
/// Client inventory singleton encoding: original or singleton item.
pub const SINGLETON_QUANTITY: i64 = -1;

// Server corporation and alliance creation validation policy.
/// Minimum corporation or alliance name length.
pub const ORG_NAME_MIN_LENGTH: usize = 4;
/// Maximum corporation or alliance name length.
pub const ORG_NAME_MAX_LENGTH: usize = 50;
/// Minimum corporation or alliance short name length.
pub const ORG_SHORT_NAME_MIN_LENGTH: usize = 2;
/// Maximum corporation or alliance short name length.
pub const ORG_SHORT_NAME_MAX_LENGTH: usize = 5;

// Client corporation public row conventions.
/// Client corporation row: hangar and wallet division name count.
pub const CORP_DIVISION_NAME_COUNT: usize = 14;
/// Client corporation public row: unlimited member limit.
pub const CORPORATION_UNLIMITED_MEMBER_LIMIT: i64 = -1;
/// Client corporation public row: NPC corporation type.
pub const CORPORATION_TYPE_NPC: i64 = 1;
/// Client corporation public row: player corporation type.
pub const CORPORATION_TYPE_PLAYER: i64 = 2;

// Client industry job and blueprint conventions.
/// Client industry job status: installed.
pub const INDUSTRY_JOB_STATUS_INSTALLED: i32 = 1;
/// Client blueprint original convention: infinite runs, distinct from quantity encoding.
pub const BLUEPRINT_INFINITE_RUNS: i32 = -1;

// Client corporation and service roles.
/// Client appConst.corpRoleDirector.
pub const ROLE_DIRECTOR: i64 = 1;
/// Client appConst.corpRoleAccountant.
pub const ROLE_ACCOUNTANT: i64 = 256;
/// Client carbon/common/script/sys/serviceConst.py: ROLE_PLAYER | ROLE_LOGIN.
pub const ROLE_PLAYER_LOGIN: i64 = 2305843009213693952 | 4611686018427387904;
/// Client carbon serviceConst: ROLE_GML | ROLE_GMH | ROLE_LEGIONEER | ROLE_WORLDMOD.
pub const GM_ROLES: i64 = 18014398509481984 | 9007199254740992 | 262144 | 4096;

// Client wallet and office constants; server market policy.
/// Gateway PLEX vault protocol: one PLEX is 100 hundredths.
pub(crate) const PLEX_HUNDREDTHS: i64 = 100;
/// Largest character ISK balance displayed correctly by the Infinity client (build 3653293),
/// measured on the client; exactly representable as f64. The database keeps 36 digits.
pub(crate) const CLIENT_MAX_ISK: f64 = 1_000_000_000_000_000_000.0;
/// Largest corporation wallet balance sent to the client; the maintainer observed
/// balances above `CLIENT_MAX_ISK` display correctly. Corporation wallet FmtAmt
/// (`carbon/common/script/util/format.py`) falls back to scientific-notation float
/// text above `appConst.maxLong`. This is the largest f64 not above maxLong;
/// `i64::MAX as f64` rounds above it.
pub(crate) const CLIENT_MAX_CORP_ISK: f64 = 9_223_372_036_854_774_784.0;
/// Largest PLEX balance displayed correctly by the Infinity client (build 3653293),
/// measured on the client; `carbon/common/script/util/format.py` FmtAmt short formats
/// reject amounts >= 1e14 (`WhatKindOfAmountIsThis`). Gateway hundredths
/// (`CLIENT_MAX_PLEX * PLEX_HUNDREDTHS`) remain well inside sint64.
pub(crate) const CLIENT_MAX_PLEX: i64 = 99_999_999_999_999;
/// Client appConst.accountingKeyCash: personal cash and the master corporation wallet.
pub const ACCOUNT_CASH: i32 = 1000;
/// Client appConst.rentalPeriodOffice, in days.
pub const RENTAL_DAYS: i64 = 30;
