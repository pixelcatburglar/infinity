//! Game-data constants shared by server modules, grouped by source.
//!
//! IDs follow the SDE and the client's inventorycommon, dogma and corporation
//! constants. FILETIME durations use Windows 100 ns ticks; AU is in metres.
//! Runtime configuration, formulas and module-owned wire protocol constants do
//! not belong here. Shared gateway protocol units are included. This module
//! provides no data lookup or runtime state.
pub mod dogma;
/// RFC 4122: UUIDs contain 16 bytes.
pub const UUID_LEN: usize = 16;
/// Client inventorycommon/const.py `groupSolarSystem`, used in location monikers.
pub const GROUP_SOLAR_SYSTEM: i64 = 5;
/// Client inventorycommon/const.py `groupStation`, used in location monikers.
pub const GROUP_STATION: i64 = 15;
/// appConst.py maxCargoContainerTransferDistance, metres.
pub const MAX_CARGO_CONTAINER_TRANSFER_DISTANCE: f64 = 2500.0;
/// Client inventorycommon/const.py `categoryAncientRelic`.
pub const CATEGORY_ANCIENT_RELIC: i32 = 34;
/// Client inventorycommon/const.py `categoryApparel`.
pub const CATEGORY_APPAREL: i32 = 30;
/// Client inventorycommon/const.py `categoryBlueprint`.
pub const CATEGORY_BLUEPRINT: i32 = 9;
/// Client inventorycommon/const.py `categoryCommodity`.
pub const CATEGORY_COMMODITY: i32 = 17;
/// Client inventorycommon/const.py `categoryDeployable`.
pub const CATEGORY_DEPLOYABLE: i32 = 22;
/// Client inventorycommon/const.py `categoryFighter`.
pub const CATEGORY_FIGHTER: i32 = 87;
/// Client inventorycommon/const.py `categoryInfrastructureUpgrade`.
pub const CATEGORY_INFRASTRUCTURE_UPGRADE: i32 = 39;
/// Client inventorycommon/const.py `categoryMaterial`.
pub const CATEGORY_MATERIAL: i32 = 4;
/// Client inventorycommon/const.py `categoryOrbital`.
pub const CATEGORY_ORBITAL: i32 = 46;
/// Client inventorycommon/const.py `categoryPersonalization`.
pub const CATEGORY_PERSONALIZATION: i32 = 2118;
/// Client inventorycommon/const.py `categoryPlanetaryCommodities`.
pub const CATEGORY_PLANETARY_COMMODITIES: i32 = 43;
/// Client inventorycommon/const.py `categoryPlanetaryResources`.
pub const CATEGORY_PLANETARY_RESOURCES: i32 = 42;
/// Client inventorycommon/const.py `categoryShipSkin`.
pub const CATEGORY_SHIP_SKIN: i32 = 91;
/// Client inventorycommon/const.py `categorySovereigntyStructure`.
pub const CATEGORY_SOVEREIGNTY_STRUCTURE: i32 = 40;
/// Sovereignty Hub type ID from client `inventorycommon/const.py`.
pub const TYPE_SOVEREIGNTY_HUB: i32 = 32458;
/// Sovereignty Hub group ID from client `inventorycommon/const.py`.
pub const GROUP_SOVEREIGNTY_HUB: i32 = 1012;
/// Client inventorycommon/const.py `categorySpecialEditionAssets`.
pub const CATEGORY_SPECIAL_EDITION_ASSETS: i32 = 63;
/// Client inventorycommon/const.py `categoryStructureModule`.
pub const CATEGORY_STRUCTURE_MODULE: i32 = 66;
/// Client inventorycommon/const.py `flagBoosterBay`.
pub const FLAG_BOOSTER_BAY: i32 = 176;
/// Client inventorycommon/const.py `flagColonyResourcesHold`.
pub const FLAG_COLONY_RESOURCES_HOLD: i32 = 185;
/// Client inventorycommon/const.py `flagCorpseBay`.
pub const FLAG_CORPSE_BAY: i32 = 174;
/// Client inventorycommon/const.py `flagExpeditionHold`.
pub const FLAG_EXPEDITION_HOLD: i32 = 188;
/// Client inventorycommon/const.py `flagFighterBay`.
pub const FLAG_FIGHTER_BAY: i32 = 158;
/// Client inventorycommon/const.py `flagFleetHangar`.
pub const FLAG_FLEET_HANGAR: i32 = 155;
/// Client inventorycommon/const.py `flagFrigateEscapeBay`.
pub const FLAG_FRIGATE_ESCAPE_BAY: i32 = 179;
/// Client inventorycommon/const.py `flagGeneralMiningHold`.
pub const FLAG_GENERAL_MINING_HOLD: i32 = 134;
/// Client inventorycommon/const.py `flagMobileDepotHold`.
pub const FLAG_MOBILE_DEPOT_HOLD: i32 = 183;
/// Client inventorycommon/const.py `flagQuafeBay`.
pub const FLAG_QUAFE_BAY: i32 = 154;
/// Client inventorycommon/const.py `flagShipHangar`.
pub const FLAG_SHIP_HANGAR: i32 = 90;
/// Client inventorycommon/const.py `flagSpecialAsteroidHold`.
pub const FLAG_SPECIAL_ASTEROID_HOLD: i32 = 182;
/// Client inventorycommon/const.py `flagSpecializedAmmoHold`.
pub const FLAG_SPECIALIZED_AMMO_HOLD: i32 = 143;
/// Client inventorycommon/const.py `flagSpecializedCommandCenterHold`.
pub const FLAG_SPECIALIZED_COMMAND_CENTER_HOLD: i32 = 148;
/// Client inventorycommon/const.py `flagSpecializedFilamentHold`.
pub const FLAG_SPECIALIZED_FILAMENT_HOLD: i32 = 212;
/// Client inventorycommon/const.py `flagSpecializedFuelBay`.
pub const FLAG_SPECIALIZED_FUEL_BAY: i32 = 133;
/// Client inventorycommon/const.py `flagSpecializedGasHold`.
pub const FLAG_SPECIALIZED_GAS_HOLD: i32 = 135;
/// Client inventorycommon/const.py `flagSpecializedIceHold`.
pub const FLAG_SPECIALIZED_ICE_HOLD: i32 = 181;
/// Client inventorycommon/const.py `flagSpecializedIndustrialShipHold`.
pub const FLAG_SPECIALIZED_INDUSTRIAL_SHIP_HOLD: i32 = 142;
/// Client inventorycommon/const.py `flagSpecializedLargeShipHold`.
pub const FLAG_SPECIALIZED_LARGE_SHIP_HOLD: i32 = 141;
/// Client inventorycommon/const.py `flagSpecializedMaterialBay`.
pub const FLAG_SPECIALIZED_MATERIAL_BAY: i32 = 151;
/// Client inventorycommon/const.py `flagSpecializedMediumShipHold`.
pub const FLAG_SPECIALIZED_MEDIUM_SHIP_HOLD: i32 = 140;
/// Client inventorycommon/const.py `flagSpecializedMineralHold`.
pub const FLAG_SPECIALIZED_MINERAL_HOLD: i32 = 136;
/// Client inventorycommon/const.py `flagSpecializedPlanetaryCommoditiesHold`.
pub const FLAG_SPECIALIZED_PLANETARY_COMMODITIES_HOLD: i32 = 149;
/// Client inventorycommon/const.py `flagSpecializedSalvageHold`.
pub const FLAG_SPECIALIZED_SALVAGE_HOLD: i32 = 137;
/// Client inventorycommon/const.py `flagSpecializedShipHold`.
pub const FLAG_SPECIALIZED_SHIP_HOLD: i32 = 138;
/// Client inventorycommon/const.py `flagSpecializedSmallShipHold`.
pub const FLAG_SPECIALIZED_SMALL_SHIP_HOLD: i32 = 139;
/// Client inventorycommon/const.py `flagSubsystemBay`.
pub const FLAG_SUBSYSTEM_BAY: i32 = 177;
/// Client inventorycommon/const.py `groupAncientCompressedIce`.
pub const GROUP_ANCIENT_COMPRESSED_ICE: i32 = 903;
/// Client inventorycommon/const.py `groupAncientSalvage`.
pub const GROUP_ANCIENT_SALVAGE: i32 = 966;
/// Client inventorycommon/const.py `groupAssaultFrigate`.
pub const GROUP_ASSAULT_FRIGATE: i32 = 324;
/// Client inventorycommon/const.py `groupAttackBattlecruiser`.
pub const GROUP_ATTACK_BATTLECRUISER: i32 = 1201;
/// Client inventorycommon/const.py `groupBattlecruiser`.
pub const GROUP_BATTLECRUISER: i32 = 419;
/// Client inventorycommon/const.py `groupBiochemicalMaterial`.
pub const GROUP_BIOCHEMICAL_MATERIAL: i32 = 712;
/// Client inventorycommon/const.py `groupCapacitorBoosterCharge`.
pub const GROUP_CAPACITOR_BOOSTER_CHARGE: i32 = 87;
/// Client inventorycommon/const.py `groupCapsule`.
pub const GROUP_CAPSULE: i32 = 29;
/// Client inventorycommon/const.py `groupColonyReagents`.
pub const GROUP_COLONY_REAGENTS: i32 = 4729;
/// Client inventorycommon/const.py `groupCombatReconShip`.
pub const GROUP_COMBAT_RECON_SHIP: i32 = 906;
/// Client inventorycommon/const.py `groupCommandBurstChargeArmor`.
pub const GROUP_COMMAND_BURST_CHARGE_ARMOR: i32 = 1774;
/// Client inventorycommon/const.py `groupCommandBurstChargeExpedition`.
pub const GROUP_COMMAND_BURST_CHARGE_EXPEDITION: i32 = 4905;
/// Client inventorycommon/const.py `groupCommandBurstChargeInfo`.
pub const GROUP_COMMAND_BURST_CHARGE_INFO: i32 = 1773;
/// Client inventorycommon/const.py `groupCommandBurstChargeMining`.
pub const GROUP_COMMAND_BURST_CHARGE_MINING: i32 = 1771;
/// Client inventorycommon/const.py `groupCommandBurstChargeShield`.
pub const GROUP_COMMAND_BURST_CHARGE_SHIELD: i32 = 1769;
/// Client inventorycommon/const.py `groupCommandBurstChargeSkirmish`.
pub const GROUP_COMMAND_BURST_CHARGE_SKIRMISH: i32 = 1772;
/// Client inventorycommon/const.py `groupCommandDestroyer`.
pub const GROUP_COMMAND_DESTROYER: i32 = 1534;
/// Client inventorycommon/const.py `groupCommandPins`.
pub const GROUP_COMMAND_PINS: i32 = 1027;
/// Client inventorycommon/const.py `groupCommandShip`.
pub const GROUP_COMMAND_SHIP: i32 = 540;
/// Client inventorycommon/const.py `groupCompressedGas`.
pub const GROUP_COMPRESSED_GAS: i32 = 4168;
/// Client inventorycommon/const.py `groupCovertOps`.
pub const GROUP_COVERT_OPS: i32 = 830;
/// Client inventorycommon/const.py `groupCruiser`.
pub const GROUP_CRUISER: i32 = 26;
/// Client inventorycommon/const.py `groupDecryptors`.
pub const GROUP_DECRYPTORS: i32 = 1304;
/// Client inventorycommon/const.py `groupDestroyer`.
pub const GROUP_DESTROYER: i32 = 420;
/// Client inventorycommon/const.py `groupDrug`.
pub const GROUP_DRUG: i32 = 313;
/// Client inventorycommon/const.py `groupElectronicAttackShips`.
pub const GROUP_ELECTRONIC_ATTACK_SHIPS: i32 = 893;
/// Client inventorycommon/const.py `groupExhumer`.
pub const GROUP_EXHUMER: i32 = 543;
/// Client inventorycommon/const.py `groupExpeditionCommandShip`.
pub const GROUP_EXPEDITION_COMMAND_SHIP: i32 = 4902;
/// Client inventorycommon/const.py `groupExpeditionFrigate`.
pub const GROUP_EXPEDITION_FRIGATE: i32 = 1283;
/// Client inventorycommon/const.py `groupFestivalMissile`.
pub const GROUP_FESTIVAL_MISSILE: i32 = 500;
/// Client inventorycommon/const.py `groupFlagCruiser`.
pub const GROUP_FLAG_CRUISER: i32 = 1972;
/// Client inventorycommon/const.py `groupForceReconShip`.
pub const GROUP_FORCE_RECON_SHIP: i32 = 833;
/// Client inventorycommon/const.py `groupFrigate`.
pub const GROUP_FRIGATE: i32 = 25;
/// Client inventorycommon/const.py `groupFuelBlock`.
pub const GROUP_FUEL_BLOCK: i32 = 1136;
/// Client inventorycommon/const.py `groupHarvestableCloud`.
pub const GROUP_HARVESTABLE_CLOUD: i32 = 711;
/// Client inventorycommon/const.py `groupHeavyAssaultCruiser`.
pub const GROUP_HEAVY_ASSAULT_CRUISER: i32 = 358;
/// Client inventorycommon/const.py `groupHeavyInterdictors`.
pub const GROUP_HEAVY_INTERDICTORS: i32 = 894;
/// Client inventorycommon/const.py `groupIce`.
pub const GROUP_ICE: i32 = 465;
/// Client inventorycommon/const.py `groupIceProduct`.
pub const GROUP_ICE_PRODUCT: i32 = 423;
/// Client inventorycommon/const.py `groupInterceptor`.
pub const GROUP_INTERCEPTOR: i32 = 831;
/// Client inventorycommon/const.py `groupInterdictor`.
pub const GROUP_INTERDICTOR: i32 = 541;
/// Client inventorycommon/const.py `groupLogistics`.
pub const GROUP_LOGISTICS: i32 = 832;
/// Client inventorycommon/const.py `groupLogisticsFrigate`.
pub const GROUP_LOGISTICS_FRIGATE: i32 = 1527;
/// Client inventorycommon/const.py `groupMineral`.
pub const GROUP_MINERAL: i32 = 18;
/// Client inventorycommon/const.py `groupMiningBarge`.
pub const GROUP_MINING_BARGE: i32 = 463;
/// Client inventorycommon/const.py `groupMobileHomes`.
pub const GROUP_MOBILE_HOMES: i32 = 1246;
/// Client inventorycommon/const.py `groupMoonMaterials`.
pub const GROUP_MOON_MATERIALS: i32 = 427;
/// Client inventorycommon/const.py `groupNamedComponents`.
pub const GROUP_NAMED_COMPONENTS: i32 = 1676;
/// Client inventorycommon/const.py `groupNonInteractableAsteroid`.
pub const GROUP_NON_INTERACTABLE_ASTEROID: i32 = 4714;
/// Client inventorycommon/const.py `groupPrismaticite`.
pub const GROUP_PRISMATICITE: i32 = 4915;
/// Client inventorycommon/const.py `groupRandomJumpKeys`.
pub const GROUP_RANDOM_JUMP_KEYS: i32 = 4041;
/// Client inventorycommon/const.py `groupRefinables`.
pub const GROUP_REFINABLES: i32 = 355;
/// Client inventorycommon/const.py `groupRougeDroneComponents`.
pub const GROUP_ROUGE_DRONE_COMPONENTS: i32 = 886;
/// Client inventorycommon/const.py `groupSalvagedMaterials`.
pub const GROUP_SALVAGED_MATERIALS: i32 = 754;
/// Client inventorycommon/const.py `groupScalableDecorativeAsteroid`.
pub const GROUP_SCALABLE_DECORATIVE_ASTEROID: i32 = 4094;
/// Client inventorycommon/const.py `groupScannerProbe`.
pub const GROUP_SCANNER_PROBE: i32 = 479;
/// Client inventorycommon/const.py `groupSpecialEditionYacht`.
pub const GROUP_SPECIAL_EDITION_YACHT: i32 = 5087;
/// Client inventorycommon/const.py `groupStealthBomber`.
pub const GROUP_STEALTH_BOMBER: i32 = 834;
/// Client inventorycommon/const.py `groupStructureAreaDenialAmmunition`.
pub const GROUP_STRUCTURE_AREA_DENIAL_AMMUNITION: i32 = 4186;
/// Client inventorycommon/const.py `groupStructureAreaMissile`.
pub const GROUP_STRUCTURE_AREA_MISSILE: i32 = 1548;
/// Client inventorycommon/const.py `groupStructureECMScript`.
pub const GROUP_STRUCTURE_E_C_M_SCRIPT: i32 = 1549;
/// Client inventorycommon/const.py `groupStructureFestivalMissile`.
pub const GROUP_STRUCTURE_FESTIVAL_MISSILE: i32 = 1976;
/// Client inventorycommon/const.py `groupStructureFlakMissile`.
pub const GROUP_STRUCTURE_FLAK_MISSILE: i32 = 1547;
/// Client inventorycommon/const.py `groupStructureHeavyFighter`.
pub const GROUP_STRUCTURE_HEAVY_FIGHTER: i32 = 4779;
/// Client inventorycommon/const.py `groupStructureLightFighter`.
pub const GROUP_STRUCTURE_LIGHT_FIGHTER: i32 = 4777;
/// Client inventorycommon/const.py `groupStructureMissile`.
pub const GROUP_STRUCTURE_MISSILE: i32 = 1546;
/// Client inventorycommon/const.py `groupStructureSupportFighter`.
pub const GROUP_STRUCTURE_SUPPORT_FIGHTER: i32 = 4778;
/// Client inventorycommon/const.py `groupStructureWarpDisruptorScript`.
pub const GROUP_STRUCTURE_WARP_DISRUPTOR_SCRIPT: i32 = 1551;
/// Client inventorycommon/const.py `groupSurveyProbe`.
pub const GROUP_SURVEY_PROBE: i32 = 492;
/// Client inventorycommon/const.py `groupTacticalDestroyer`.
pub const GROUP_TACTICAL_DESTROYER: i32 = 1305;
/// Client inventorycommon/const.py `groupTemporalResources`.
pub const GROUP_TEMPORAL_RESOURCES: i32 = 2022;
/// Client inventorycommon/const.py `groupTerranArtifacts`.
pub const GROUP_TERRAN_ARTIFACTS: i32 = 519;
/// Client inventorycommon/const.py `groupTriglavianJumpKeys`.
pub const GROUP_TRIGLAVIAN_JUMP_KEYS: i32 = 4087;
/// Client inventorycommon/const.py `groupUnrefinedMinerals`.
pub const GROUP_UNREFINED_MINERALS: i32 = 4932;
/// Client inventorycommon/const.py `typeCorpse`.
pub const TYPE_CORPSE: i32 = 25;
/// Client inventorycommon/const.py `typeCorpseFemale`.
pub const TYPE_CORPSE_FEMALE: i32 = 29148;
/// Client inventorycommon/const.py `typeHyperQuafe`.
pub const TYPE_HYPER_QUAFE: i32 = 57422;
/// Client inventorycommon/const.py `typeLargeCratesOfQuafe`.
pub const TYPE_LARGE_CRATES_OF_QUAFE: i32 = 4090;
/// Client inventorycommon/const.py `typeNaniteRepairPaste`.
pub const TYPE_NANITE_REPAIR_PASTE: i32 = 28668;
/// Client inventorycommon/const.py `typeOxygen`.
pub const TYPE_OXYGEN: i32 = 3683;
/// Client inventorycommon/const.py `typeQuafe`.
pub const TYPE_QUAFE: i32 = 3699;
/// Client inventorycommon/const.py `typeQuafeUltra`.
pub const TYPE_QUAFE_ULTRA: i32 = 12865;
/// Client inventorycommon/const.py `typeQuafeUltraSpecialEdition`.
pub const TYPE_QUAFE_ULTRA_SPECIAL_EDITION: i32 = 12994;
/// Client inventorycommon/const.py `typeQuafeZero`.
pub const TYPE_QUAFE_ZERO: i32 = 3898;
/// Client inventorycommon/const.py `typeSpikedQuafe`.
pub const TYPE_SPIKED_QUAFE: i32 = 21661;
/// Client inventorycommon/const.py `typeWater`.
pub const TYPE_WATER: i32 = 3645;
/// `eveexceptions/const.py`: UE_LOCID location substitution tag.
pub const UE_LOCID: i64 = 3;
/// Client `appConst.py` corpHeraldry: issuer of EverMarks.
pub const CORP_HERALDRY: i64 = 1_000_419;
/// `carbon/common/script/sys/serviceConst.py`: ROLE_QA.
pub const ROLE_QA: i64 = 4_503_599_627_370_496;
/// Client inventorycommon.const.categoryImplant.
pub const CATEGORY_IMPLANT: i32 = 20;
/// Client inventorycommon.const.groupBooster.
pub const GROUP_BOOSTER: i32 = 303;
/// SDE invCategories: asteroid resources.
pub const CATEGORY_ASTEROID: i32 = 25;
/// inventorycommon/const.py: groupCloudComputing.
pub const GROUP_CLOUD_COMPUTING: i32 = 4196;
/// inventorycommon/const.py: groupShipAdaption.
pub const GROUP_SHIP_ADAPTION: i32 = 4160;
/// inventorycommon/const.py: groupAdaptionChips.
pub const GROUP_ADAPTION_CHIPS: i32 = 4901;
/// inventorycommon/const.py: groupCharacterEnhancement.
pub const GROUP_CHARACTER_ENHANCEMENT: i32 = 4194;

/// `inventorycommon/const.py:48-59`: first wormhole solar-system ID.
pub const WORMHOLE_SYSTEM_START: i64 = 31_000_000;
/// `inventorycommon/const.py:45`: first known-space solar system.
pub const KNOWN_SPACE_SYSTEM_START: i64 = 30_000_000;
/// `inventorycommon/const.py:1019`: groupSun.
pub const GROUP_SUN: i32 = 6;
/// `inventorycommon/const.py:553`: groupControlTower.
pub const GROUP_CONTROL_TOWER: i32 = 365;
/// `inventorycommon/const.py`: groupStargate.
pub const GROUP_STARGATE: i32 = 10;


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
/// Client menusvc.DEFAULT_INTERACTION_RANGE and server drone scoop policy, metres.
pub const DRONE_SCOOP_RANGE: f64 = 2500.0;
/// `inventorycommon/const.py:3374`: ownerSystem (EVE System), also the owner of unowned items.
pub const OWNER_UNOWNED: i64 = 1;

/// Client entityConst: idle drone activity.
pub const DRONE_STATE_IDLE: i32 = 0;
/// Client entityConst: combat drone activity.
pub const DRONE_STATE_COMBAT: i32 = 1;
/// Client entityConst: mining drone activity.
pub const DRONE_STATE_MINING: i32 = 2;
/// Client entityConst: approaching drone activity.
pub const DRONE_STATE_APPROACHING: i32 = 3;
/// Client entityConst: departing drone activity.
pub const DRONE_STATE_DEPARTING: i32 = 4;

// SDE inventory type, group and category IDs.
/// SDE invGroups and `inventorycommon/const.py` groupCharacter.
pub const GROUP_CHARACTER: i64 = 1;
/// SDE invGroups and `inventorycommon/const.py` groupCorporation.
pub const GROUP_CORPORATION: i64 = 2;
/// SDE invGroups and `inventorycommon/const.py` groupAlliance.
pub const GROUP_ALLIANCE: i64 = 32;
/// `facwarCommon.py` _ALL_WARFARE_FACTIONS.
pub const WARFARE_FACTIONS: [i64; 6] = [FACTION_CALDARI, FACTION_MINMATAR, FACTION_AMARR, FACTION_GALLENTE, FACTION_GURISTAS, FACTION_ANGEL_CARTEL];
/// SDE invTypes: Velator, the fallback rookie ship.
pub const FALLBACK_ROOKIE_SHIP: i32 = 606;
/// Client `inventorycommon.const.typeBrokerRelations`.
pub const SKILL_BROKER_RELATIONS: i32 = 3446;
/// Client `inventorycommon.const.typeAccounting`.
pub const SKILL_ACCOUNTING: i32 = 16622;
/// Client `inventorycommon/const.py` typePlasticWrap.
pub const TYPE_PLASTIC_WRAP: i32 = 3468;
/// Client `inventorycommon/const.py:typePlex`.
pub const TYPE_PLEX: i32 = 44992;
/// SDE invTypes: Character (Gallente), used for player owners.
pub const TYPE_CHARACTER: i32 = 1373;
/// SDE invTypes: Faction.
pub const TYPE_FACTION: i64 = 30;
/// SDE invTypes: Solar System.
pub const TYPE_SOLAR_SYSTEM: i32 = 5;
/// SDE invTypes: Capsule.
pub const TYPE_CAPSULE: i32 = 670;
/// Client `inventorycommon/const.py` groupBattleship.
pub const GROUP_BATTLESHIP: i32 = 27;
/// Client `inventorycommon/const.py` groupIndustrial.
pub const GROUP_INDUSTRIAL: i32 = 28;
/// Client `inventorycommon/const.py` groupTitan.
pub const GROUP_TITAN: i32 = 30;
/// Client `inventorycommon/const.py` groupTransportShip.
pub const GROUP_TRANSPORT_SHIP: i32 = 380;
/// Client `inventorycommon/const.py` groupEliteBattleship.
pub const GROUP_ELITE_BATTLESHIP: i32 = 381;
/// Client `inventorycommon/const.py` groupDreadnought.
pub const GROUP_DREADNOUGHT: i32 = 485;
/// Client `inventorycommon/const.py` groupFreighter.
pub const GROUP_FREIGHTER: i32 = 513;
/// Client `inventorycommon/const.py` groupCarrier.
pub const GROUP_CARRIER: i32 = 547;
/// Client `inventorycommon/const.py` groupSupercarrier.
pub const GROUP_SUPERCARRIER: i32 = 659;
/// Client `inventorycommon/const.py` groupCapitalIndustrialShip.
pub const GROUP_CAPITAL_INDUSTRIAL_SHIP: i32 = 883;
/// Client `inventorycommon/const.py` groupBlackOps.
pub const GROUP_BLACK_OPS: i32 = 898;
/// Client `inventorycommon/const.py` groupMarauders.
pub const GROUP_MARAUDERS: i32 = 900;
/// Client `inventorycommon/const.py` groupJumpFreighter.
pub const GROUP_JUMP_FREIGHTER: i32 = 902;
/// Client `inventorycommon/const.py` groupIndustrialCommandShip.
pub const GROUP_INDUSTRIAL_COMMAND_SHIP: i32 = 941;
/// Client `inventorycommon/const.py` groupBlockadeRunner.
pub const GROUP_BLOCKADE_RUNNER: i32 = 1202;
/// Client `inventorycommon/const.py` groupForceAux.
pub const GROUP_FORCE_AUX: i32 = 1538;
/// Client `inventorycommon/const.py` groupLancerDreadnought.
pub const GROUP_LANCER_DREADNOUGHT: i32 = 4594;
/// Client `inventorycommon/const.py` groupCommandCarrier.
pub const GROUP_COMMAND_CARRIER: i32 = 5120;
/// SDE invGroups: Planet.
pub const GROUP_PLANET: i32 = 7;
/// SDE invGroups: Moon.
pub const GROUP_MOON: i32 = 8;
/// SDE invGroups: Asteroid Belt.
pub const GROUP_ASTEROID_BELT: i32 = 9;
/// SDE invGroups: Wreck.
pub const GROUP_WRECK: i32 = 186;
/// SDE invGroups: Strategic Cruiser (inventorycommon/util.py:126-127).
pub const GROUP_STRATEGIC_CRUISER: i64 = 963;
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
/// Client `inventorycommon.const.categoryStarbase`; SDE invCategories: Starbase.
pub const CATEGORY_STARBASE: i32 = 23;
/// Client `inventorycommon.const.categoryStructure`; SDE invCategories: Structure.
pub const CATEGORY_STRUCTURE: i32 = 65;
/// SDE invCategories: Skill.
pub const CATEGORY_SKILL: i32 = 16;
/// evetypes/const.py: skills available for direct purchase.
pub const TYPE_LIST_SKILLS_AVAILABLE_FOR_PURCHASE: i64 = 93;
/// SDE invCategories: Drone.
pub const CATEGORY_DRONE: i32 = 18;
/// Mass of most published 5 m³ drones in the static data; used when a drone type has no positive mass.
pub const DRONE_FALLBACK_MASS: f64 = 3000.0;
/// Agility of nearly all published drones in the static data; used when a drone type has none.
pub const DRONE_FALLBACK_AGILITY: f64 = 100.0;
/// SDE invCategories: Subsystem.
pub const CATEGORY_SUBSYSTEM: i32 = 32;
/// SDE invTypes: Hacking skill.
pub const SKILL_HACKING: i32 = 21718;
/// SDE invTypes: Archaeology skill.
pub const SKILL_ARCHAEOLOGY: i32 = 13278;
/// Client inventorycommon/const.py: typeInfomorphPsychology.
pub const SKILL_INFOMORPH_PSYCHOLOGY: i32 = 24242;
/// Client inventorycommon/const.py: typeAdvancedInfomorphPsychology.
pub const SKILL_ADVANCED_INFOMORPH_PSYCHOLOGY: i32 = 33407;
/// Client inventorycommon/const.py: typeEliteInfomorphPsychology.
pub const SKILL_ELITE_INFOMORPH_PSYCHOLOGY: i32 = 73910;

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
/// Client inventorycommon/const.py flagCorpSAG1..7: corporation hangar division count.
pub const CORP_HANGAR_DIVISIONS: i32 = 7;
/// Client inventorycommon.const.flagCorpDeliveries.
pub const FLAG_CORP_DELIVERIES: i32 = 62;
/// Client inventorycommon.const.locationJunkyard: deleted items.
pub const LOCATION_JUNKYARD: i64 = 10;
/// Client carbon const.ixOwnerID: inventory row owner column.
pub const IX_OWNER_ID: i64 = 2;
/// Client carbon const.ixLocationID: inventory row location column.
pub const IX_LOCATION_ID: i64 = 3;
/// Client carbon const.ixFlag: inventory row flag column.
pub const IX_FLAG: i64 = 4;
/// Client carbon/common/lib/const.py ixQuantity: inventory row quantity column.
pub const IX_QUANTITY: i64 = 5;
/// Client inventorycommon.const.ixStackSize: inventory stack-size change key.
pub const IX_STACK_SIZE: i64 = 9;
/// Client inventorycommon.const.ixSingleton: inventory singleton change key.
pub const IX_SINGLETON: i64 = 10;

/// PostgreSQL bigint, inventory DBRow I8 and client carbon/common/lib/const.py
/// `maxBigint` share this signed 64-bit limit for an ordinary physical stack.
pub const MAX_STACK_QUANTITY: i64 = i64::MAX;

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

/// SDE invTypes: Corporation.
pub const TYPE_CORPORATION: i64 = 2;
/// SDE invTypes: Alliance.
pub const TYPE_ALLIANCE: i64 = 16159;

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
/// Client industry/const.py STATUS_READY.
pub const INDUSTRY_JOB_STATUS_READY: i32 = 3;
/// Client blueprint original convention: infinite runs, distinct from quantity encoding.
pub const BLUEPRINT_INFINITE_RUNS: i32 = -1;

// Client corporation and service roles.
/// Client appConst.corpRoleDirector.
pub const ROLE_DIRECTOR: i64 = 1;
/// Client appConst.corpRoleContractManager.
pub const ROLE_CONTRACT_MANAGER: i64 = 72_057_594_037_927_936;
/// Client appConst.corpRoleAccountant.
pub const ROLE_ACCOUNTANT: i64 = 256;
/// Client appConst.corpRoleHangarCanTake1; divisions 2-7 are the next bits.
pub const ROLE_HANGAR_CAN_TAKE_1: i64 = 8192;
/// Client industry/const.py: ROLE_FACTORY_MANAGER.
pub const ROLE_FACTORY_MANAGER: i64 = 1024;
/// Client industry/const.py: ROLE_FACTORY_MANUFACTURING.
pub const ROLE_FACTORY_MANUFACTURING: i64 = 1_125_899_906_842_624;
/// Client industry/const.py: ROLE_FACTORY_SCIENCE.
pub const ROLE_FACTORY_SCIENCE: i64 = 2_251_799_813_685_248;
/// Client appConst.corpRoleHangarCanQuery1; divisions 2-7 are the next bits.
pub const ROLE_HANGAR_CAN_QUERY_1: i64 = 1_048_576;
/// Client appConst.corpRoleJuniorAccountant.
pub const ROLE_JUNIOR_ACCOUNTANT: i64 = 4_503_599_627_370_496;
/// Client appConst.corpRoleTrader.
pub const ROLE_TRADER: i64 = 18_014_398_509_481_984;
/// Roles allowed to view corporation deliveries (inventorysvc.py).
pub const ROLE_DELIVERIES_VIEW: i64 = ROLE_ACCOUNTANT | ROLE_JUNIOR_ACCOUNTANT | ROLE_TRADER;
/// Roles allowed to take corporation deliveries (itemLocking.py).
pub const ROLE_DELIVERIES_TAKE: i64 = ROLE_ACCOUNTANT | ROLE_TRADER;
/// Client `carbon/common/script/sys/serviceConst.py:39`: ROLE_SERVICE.
pub const ROLE_SERVICE: i64 = 1_152_921_504_606_846_976;
/// Client carbon/common/script/sys/serviceConst.py: ROLE_PLAYER | ROLE_LOGIN.
pub const ROLE_PLAYER_LOGIN: i64 = 2305843009213693952 | 4611686018427387904;
/// Client carbon/common/script/sys/serviceConst.py: ROLE_GML.
pub const ROLE_GML: i64 = 18014398509481984;
/// Client carbon serviceConst: ROLE_GML | ROLE_GMH | ROLE_LEGIONEER | ROLE_WORLDMOD.
pub const GM_ROLES: i64 = ROLE_GML | 9007199254740992 | 262144 | 4096;

// Client wallet and office constants; server market policy.
/// Gateway PLEX vault protocol: one PLEX is 100 hundredths.
pub const PLEX_HUNDREDTHS: i64 = 100;
/// Largest character ISK balance displayed correctly by the Infinity client (build 3653293),
/// measured on the client; exactly representable as f64. The database keeps 36 digits.
pub const CLIENT_MAX_ISK: f64 = 1_000_000_000_000_000_000.0;
/// Largest corporation wallet balance sent to the client; the maintainer observed
/// balances above `CLIENT_MAX_ISK` display correctly. Corporation wallet FmtAmt
/// (`carbon/common/script/util/format.py`) falls back to scientific-notation float
/// text above `appConst.maxLong`. This is the largest f64 not above maxLong;
/// `i64::MAX as f64` rounds above it.
pub const CLIENT_MAX_CORP_ISK: f64 = 9_223_372_036_854_774_784.0;
/// Largest amount the Infinity client's short amount formats display (build
/// 3653293): `carbon/common/script/util/format.py` FmtAmt short formats reject
/// absolute amounts >= 1e14 (`WhatKindOfAmountIsThis`). PLEX balances and item
/// stack quantities are sent to the client capped at this value; the
/// authoritative stored amounts stay exact. Gateway hundredths
/// (`CLIENT_MAX_SHORT_AMOUNT * PLEX_HUNDREDTHS`) remain well inside sint64.
pub const CLIENT_MAX_SHORT_AMOUNT: i64 = 99_999_999_999_999;
/// Client appConst.accountingKeyCash: personal cash and the master corporation wallet.
pub const ACCOUNT_CASH: i32 = 1000;
/// Client appConst.refMarketTransaction.
pub const REF_MARKET_TRANSACTION: i32 = 2;
/// Client appConst.refMarketEscrow.
pub const REF_MARKET_ESCROW: i32 = 42;
/// Client appConst.refBrokerfee.
pub const REF_BROKER_FEE: i32 = 46;
/// Client appConst.refTransactionTax.
pub const REF_TRANSACTION_TAX: i32 = 54;
/// Client appConst.refMarketSecurityTax (appConst:804; market category:931).
pub const REF_MARKET_SECURITY_TAX: i32 = 194;
/// Client appConst.corpRoleAccountCanTake1; subsequent divisions shift by one bit.
pub const ROLE_ACCOUNT_CAN_TAKE_1: i64 = 134_217_728;
/// Client carbon/common/lib/const.py UE_LOC.
pub const UE_LOC: i64 = 101;
/// Client inventorycommon/const.py ownerSCC.
pub const OWNER_SCC: i64 = 1_000_132;
/// `inventorycommon/const.py:3368`: ownerDED.
pub const OWNER_DED: i64 = 1_000_137;
/// Client appConst.rentalPeriodOffice, in days.
pub const RENTAL_DAYS: i64 = 30;
