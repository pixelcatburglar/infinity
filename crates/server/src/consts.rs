//! Game-data constants shared by server modules, grouped by source.
//!
//! IDs follow the SDE and the client's inventorycommon, dogma and corporation
//! constants. FILETIME durations use Windows 100 ns ticks; AU is in metres.
//! Runtime configuration, formulas and module-owned wire protocol constants do
//! not belong here. Shared gateway protocol units are included. This module
//! provides no data lookup or runtime state.
pub mod dogma;
/// `carbon/common/script/sys/serviceConst.py`: ROLE_QA.
pub const ROLE_QA: i64 = 4_503_599_627_370_496;
/// Client inventorycommon.const.categoryImplant.
pub const CATEGORY_IMPLANT: i32 = 20;
/// Client inventorycommon.const.groupBooster.
pub const GROUP_BOOSTER: i32 = 303;

/// `inventorycommon/const.py:48-59`: first wormhole solar-system ID.
pub const WORMHOLE_SYSTEM_START: i64 = 31_000_000;


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
/// Neutral owner used by positioned exploration containers in dao/exploration.rs.
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
/// Client `inventorycommon.const.typeBrokerRelations`.
pub const SKILL_BROKER_RELATIONS: i32 = 3446;
/// Client `inventorycommon.const.typeAccounting`.
pub const SKILL_ACCOUNTING: i32 = 16622;
/// Client `inventorycommon/const.py` typePlasticWrap.
pub const TYPE_PLASTIC_WRAP: i32 = 3468;
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
/// Client carbon/common/script/sys/serviceConst.py: ROLE_PLAYER | ROLE_LOGIN.
pub const ROLE_PLAYER_LOGIN: i64 = 2305843009213693952 | 4611686018427387904;
/// Client carbon/common/script/sys/serviceConst.py: ROLE_GML.
pub const ROLE_GML: i64 = 18014398509481984;
/// Client carbon serviceConst: ROLE_GML | ROLE_GMH | ROLE_LEGIONEER | ROLE_WORLDMOD.
pub const GM_ROLES: i64 = ROLE_GML | 9007199254740992 | 262144 | 4096;

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
