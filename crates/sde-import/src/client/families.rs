//! The authoritative client families and their resource and destination registration.
//! Registration follows the observed client consumer projections and their
//! complete dependency closure. Client resources own migrated destinations;
//! separately named SDE inputs remain unmigrated. Unregistered families are
//! not decoded as client data and unsupported layouts fail before mutation.

#[derive(Clone, Copy)]
pub enum Decoder { Types, Catalogues, Dogma, Materials, Blueprints, Localization }

pub struct Family {
    pub name: &'static str,
    pub resource: &'static str,
    pub decoder: Decoder,
    pub required_families: &'static [&'static str],
    pub native_table: &'static str,
    pub compat_table: Option<&'static str>,
    pub typed_tables: &'static [&'static str],
}

// The whole set is one dependency-closed import; no family may be enabled alone.
const CLOSURE: &[&str] = &[
    "types", "groups", "categories", "metagroups", "marketgroups", "dogmaattributes",
    "dogmaattributecategories", "dogmaunits", "dogmaeffects", "typedogma", "typematerials",
    "requiredskillsfortypes", "blueprints", "localization_fsd_main", "localization_fsd_en_us", "localization_fsd_zh",
];

macro_rules! family {
    ($name:literal, $resource:literal, $decoder:ident, $compat:expr, [$($typed:literal),* $(,)?]) => {
        Family { name: $name, resource: $resource, decoder: Decoder::$decoder, required_families: CLOSURE,
            native_table: $name, compat_table: $compat, typed_tables: &[$($typed),*] }
    };
}

pub static FAMILIES: &[Family] = &[
    family!("types", "staticdata/types.fsdbinary", Types, Some("types"), ["inv_types"]),
    family!("groups", "staticdata/groups.fsdbinary", Types, Some("groups"), ["inv_groups", "inv_types"]),
    family!("categories", "staticdata/categories.fsdbinary", Catalogues, Some("categories"), ["inv_categories"]),
    family!("metagroups", "staticdata/metagroups.fsdbinary", Catalogues, None, []),
    family!("marketgroups", "staticdata/marketgroups.fsdbinary", Catalogues, None, []),
    family!("dogmaattributes", "staticdata/dogmaattributes.fsdbinary", Dogma, Some("dogmaattributes"), ["dogma_attributes"]),
    family!("dogmaattributecategories", "staticdata/dogmaattributecategories.fsdbinary", Dogma, None, []),
    family!("dogmaunits", "staticdata/dogmaunits.fsdbinary", Dogma, None, []),
    family!("dogmaeffects", "staticdata/dogmaeffects.fsdbinary", Dogma, Some("dogmaeffects"), ["dogma_effects", "dogma_effect_modifiers"]),
    family!("typedogma", "staticdata/typedogma.fsdbinary", Dogma, Some("typedogma"), ["type_attributes", "type_effects"]),
    family!("typematerials", "staticdata/typematerials.fsdbinary", Materials, Some("typematerials"), ["inv_type_materials"]),
    family!("requiredskillsfortypes", "staticdata/requiredskillsfortypes.fsdbinary", Materials, None, []),
    family!("blueprints", "staticdata/blueprints.static", Blueprints, Some("blueprints"), ["blueprints"]),
    family!("localization_fsd_main", "localizationfsd/localization_fsd_main.pickle", Localization, None, []),
    family!("localization_fsd_en_us", "localizationfsd/localization_fsd_en-us.pickle", Localization, None, []),
    family!("localization_fsd_zh", "localizationfsd/localization_fsd_zh.pickle", Localization, None, []),
];

/// Recovered source paths and the subset projected by typed consumers.
pub fn recovered_fields(family: &str) -> (&'static str, &'static str) {
    match family {
        "types" => ("_key typeID basePrice capacity mass radius volume portionSize groupID typeNameID published certificateTemplate descriptionID factionID graphicID iconID isisGroupID marketGroupID metaGroupID metaLevel raceID soundID techLevel variationParentTypeID wreckTypeID quoteAuthorID quoteID isDynamicType designerIDs",
            "_key basePrice capacity mass radius volume portionSize groupID typeNameID descriptionID published graphicID iconID marketGroupID raceID soundID"),
        "groups" => ("_key categoryID groupID groupNameID anchorable anchored fittableNonSingleton published useBasePrice iconID", "_key categoryID groupNameID anchorable anchored fittableNonSingleton published useBasePrice iconID"),
        "categories" => ("_key categoryID categoryNameID published iconID sofBuildClass", "_key categoryNameID published iconID"),
        "metagroups" => ("_key nameID color descriptionID iconID iconSuffix", ""),
        "marketgroups" => ("_key nameID hasTypes descriptionID iconID parentGroupID", "_key nameID parentGroupID"),
        "dogmaattributes" => ("_key attributeID name dataType description categoryID chargeRechargeTimeID displayNameID iconID maxAttributeID minAttributeID tooltipDescriptionID tooltipTitleID unitID defaultValue displayWhenZero highIsGood published stackable",
            "_key name dataType categoryID displayNameID iconID unitID defaultValue highIsGood published stackable"),
        "dogmaattributecategories" => ("_key name description", ""),
        "dogmaunits" => ("_key name descriptionID displayNameID", ""),
        "dogmaeffects" => ("_key effectID effectName effectCategory descriptionID dischargeAttributeID displayNameID distribution durationAttributeID falloffAttributeID fittingUsageChanceAttributeID iconID npcActivationChanceAttributeID npcUsageChanceAttributeID rangeAttributeID resistanceAttributeID trackingSpeedAttributeID guid sfxName disallowAutoRepeat electronicChance isAssistance isOffensive isWarpSafe propulsionChance published rangeChance modifierInfo modifierInfo/*/domain modifierInfo/*/func modifierInfo/*/effectID modifierInfo/*/groupID modifierInfo/*/modifiedAttributeID modifierInfo/*/modifyingAttributeID modifierInfo/*/skillTypeID modifierInfo/*/operation",
            "_key effectName effectCategory dischargeAttributeID distribution durationAttributeID falloffAttributeID fittingUsageChanceAttributeID npcActivationChanceAttributeID npcUsageChanceAttributeID rangeAttributeID resistanceAttributeID trackingSpeedAttributeID guid sfxName disallowAutoRepeat electronicChance isAssistance isOffensive isWarpSafe propulsionChance published rangeChance modifierInfo modifierInfo/*/domain modifierInfo/*/func modifierInfo/*/effectID modifierInfo/*/groupID modifierInfo/*/modifiedAttributeID modifierInfo/*/modifyingAttributeID modifierInfo/*/skillTypeID modifierInfo/*/operation"),
        "typedogma" => ("_key dogmaAttributes dogmaAttributes/*/attributeID dogmaAttributes/*/value dogmaEffects dogmaEffects/*/effectID dogmaEffects/*/isDefault", "_key dogmaAttributes dogmaAttributes/*/attributeID dogmaAttributes/*/value dogmaEffects dogmaEffects/*/effectID dogmaEffects/*/isDefault"),
        "typematerials" => ("_key materials materials/*/materialTypeID materials/*/quantity randomizedMaterials randomizedMaterials/*/materialTypeID randomizedMaterials/*/quantityMax randomizedMaterials/*/quantityMin", "_key materials materials/*/materialTypeID materials/*/quantity"),
        "requiredskillsfortypes" => ("_key *", "_key *"),
        "blueprints" => ("_key blueprintTypeID maxProductionLimit activities activities/* activities/*/materials activities/*/materials/*/typeID activities/*/materials/*/quantity activities/*/products activities/*/products/*/typeID activities/*/products/*/quantity activities/*/skills activities/*/skills/*/typeID activities/*/skills/*/level activities/*/time", "_key maxProductionLimit activities"),
        "localization_fsd_main" => ("_key text", ""),
        "localization_fsd_en_us" | "localization_fsd_zh" => ("_key text metadata tokens", "_key text"),
        _ => ("", ""),
    }
}

/// Source families consumed by each remaining typed projection in sde_typed.sql.
pub fn unmigrated_typed_sources(table: &str) -> &'static [&'static str] {
    match table {
        "icons" => &["icons"], "map_regions" => &["mapregions"],
        "map_constellations" => &["mapconstellations"], "map_solar_systems" => &["mapsolarsystems"],
        "map_stargates" | "map_jumps" => &["mapstargates"],
        "sta_stations" => &["npcstations", "mapsolarsystems"], "sta_operations" => &["stationoperations"],
        "map_celestials" => &["mapstars", "mapplanets", "mapmoons", "mapasteroidbelts", "mapstargates", "npcstations", "mapsolarsystems", "types"],
        "chr_factions" => &["factions"], "chr_races" => &["races"], "chr_bloodlines" => &["bloodlines"],
        "crp_npc_corporations" => &["npccorporations"], "npc_characters" => &["npccharacters"],
        "build_info" => &["_sde"], _ => &[],
    }
}

pub fn is_migrated(schema: &str, table: &str) -> bool {
    FAMILIES.iter().any(|family| match schema {
        "sde_native" => family.native_table == table,
        "sde_compat" => family.name == table,
        _ => false,
    })
}
