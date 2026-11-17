//! FSD configuration validation and `InnerGetRepairTime` branch coverage.

use super::*;
use infinity_core::consts::{CATEGORY_MATERIAL, FLAG_HANGAR};

const DEFAULTS: &str = r#"{"repairDuration": 3600, "defaultServiceLife": 100.0, "estPricePercentage": 0.16,
    "defaultShipRepairDuration": 3600, "defaultMaxReduceServiceLife": 50.0, "manufacturingTimePercentage": 0.1,
    "defaultRefurbishedComponentDropChance": 0.5, "defaultRefurbishedComponentDropMaxNum": 10,
    "defaultRefurbishedComponentDropMinNum": 1, "defaultRefurbishedComponentAddServiceLifeValue": 1.0}"#;

fn config(data: String) -> Vec<dao::ConfigRow> { vec![dao::ConfigRow { data }] }
fn types_config(groups: &str, types: &str) -> Vec<dao::ConfigRow> {
    config(format!(r#"{{"repairableShipGroups": {groups}, "repairableShipTypes": {types}, "defaultRepairableConfig": {DEFAULTS}}}"#))
}
fn components_config(pairs: &str) -> Vec<dao::ConfigRow> {
    config(format!(r#"{{"typeRefurbishedComponents": {pairs}}}"#))
}
fn unrepairable_config(categories: &str, groups: &str, types: &str) -> Vec<dao::ConfigRow> {
    config(format!(r#"{{"unrepairableCategories": {categories}, "unrepairableGroups": {groups}, "unrepairableTypes": {types}}}"#))
}
fn variation(type_id: i32, group: Option<i32>, category: Option<i32>, parent: Option<i32>, meta: Option<f64>) -> dao::VariationRow {
    dao::VariationRow { type_id, group_id: group, category_id: category, parent, meta_level: meta }
}
fn lookups() -> Lookups {
    Lookups {
        types: (1..=30).collect(), groups: (100..=110).collect(), categories: (200..=210).collect(),
        fixed_materials: [13, 14].into_iter().collect(),
        build_times: [(13, 100.0), (16, 360.0), (18, 30.0), (19, 40.0), (20, 50.0)].into_iter().collect(),
        compression_sources: [19].into_iter().collect(), compression_outputs: [20].into_iter().collect(),
    }
}
fn build(
    t: Vec<dao::ConfigRow>, c: Vec<dao::ConfigRow>, u: Vec<dao::ConfigRow>,
    variations: Vec<dao::VariationRow>, randomized: Vec<dao::RandomizedMaterialRow>, lookups: &Lookups,
) -> anyhow::Result<RepairableShip> {
    RepairableShip::from_rows(&t, &c, &u, &variations, &randomized, lookups)
}
fn inputs() -> (Vec<dao::ConfigRow>, Vec<dao::ConfigRow>, Vec<dao::ConfigRow>) {
    (types_config(r#"{"100": {"repairDuration": 60}}"#, r#"{"10": {"repairDuration": 7}, "11": {"repairDuration": 9}}"#),
        components_config(r#"{"21": 22}"#), unrepairable_config("[200]", "[101]", "[23]"))
}

#[test]
fn validates_configuration_and_computes_repair_seconds() {
    let (t, c, u) = inputs();
    let variations = vec![
        variation(10, Some(100), Some(CATEGORY_SHIP), None, None),          // group duration wins over type duration
        variation(11, Some(102), Some(CATEGORY_SHIP), None, None),          // type duration when group absent
        variation(12, Some(102), Some(CATEGORY_SHIP), None, None),          // default ship duration
        variation(13, Some(103), Some(CATEGORY_MODULE), None, None),        // fixed materials -> industry build time
        variation(14, Some(GROUP_COMPRESSED_GAS), Some(CATEGORY_MATERIAL), None, None), // decompressible gas -> not industry
        variation(15, Some(104), Some(CATEGORY_MODULE), Some(16), Some(2.0)), // variation parent branch
        variation(16, Some(104), Some(CATEGORY_MODULE), None, Some(5.0)),
        variation(17, Some(GROUP_STATION as i32), Some(CATEGORY_MATERIAL), None, None), // station group excluded
        variation(18, Some(105), Some(CATEGORY_MATERIAL), None, None),      // randomized materials -> industry
        variation(19, Some(105), Some(CATEGORY_MATERIAL), None, None),      // compression source -> industry
        variation(20, Some(105), Some(CATEGORY_MATERIAL), None, None),      // compression output -> industry
        variation(24, Some(106), Some(CATEGORY_MODULE), Some(25), None),    // parent without blueprint -> 0 build time
        variation(25, Some(106), Some(CATEGORY_MODULE), None, Some(3.0)),
    ];
    let randomized = vec![dao::RandomizedMaterialRow { type_id: 18 }];
    let lookups = lookups();
    let ship = build(t, c, u, variations, randomized, &lookups).unwrap();
    assert_eq!((ship.repair_seconds(10).unwrap(), ship.repair_seconds(11).unwrap()), (65.0, 14.0));
    assert_eq!((ship.repair_seconds(12).unwrap(), ship.repair_seconds(13).unwrap()), (3605.0, 15.0));
    assert_eq!(ship.repair_seconds(15).unwrap(), 905.0);
    assert_eq!((ship.repair_seconds(18).unwrap(), ship.repair_seconds(19).unwrap(), ship.repair_seconds(20).unwrap()), (8.0, 9.0, 10.0));
    assert_eq!((ship.repair_seconds(14).unwrap(), ship.repair_seconds(17).unwrap(), ship.repair_seconds(24).unwrap()), (5.0, 5.0, 5.0));
    assert_eq!(ship.component(21), Some(22));
    assert!(ship.repair_seconds(999).is_none());
    assert_eq!((ship.max_life(), ship.max_reduce(), ship.restore_per_component(), ship.price_percentage()), (100.0, 50.0, 1.0, 0.16));
    assert!(ship.unrepairable(200, 1, 1) && ship.unrepairable(1, 101, 1) && ship.unrepairable(1, 1, 23) && !ship.unrepairable(1, 1, 1));
}

#[test]
fn rejects_invalid_configuration_rows() {
    let lookups = lookups();
    let (t, c, u) = inputs();
    let variations = vec![variation(10, Some(100), Some(CATEGORY_SHIP), None, None)];
    assert!(build(t.clone(), components_config(r#"{"21": 22, "23": 22}"#), u.clone(), variations.clone(), vec![], &lookups).is_ok());
    assert!(build(vec![], c.clone(), u.clone(), variations.clone(), vec![], &lookups).is_err());
    assert!(build(vec![dao::ConfigRow { data: "{}".into() }, dao::ConfigRow { data: "{}".into() }], c.clone(), u.clone(), variations.clone(), vec![], &lookups).is_err());
    for bad in [
        types_config(r#"{"999": {"repairDuration": 60}}"#, "{}"),
        types_config(r#"{"100": {"repairDuration": -1}}"#, "{}"),
        types_config(r#"{"100": {}}"#, "{}"),
        components_config(r#"{"21": 21}"#),
        components_config(r#"{"21": 22, "021": 23}"#),
        unrepairable_config("[]", "[999]", "[]"),
    ] {
        let label = bad[0].data.clone();
        let (t2, c2, u2) = if bad[0].data.starts_with(r#"{"repairableShip"#) { (bad, c.clone(), u.clone()) }
            else if bad[0].data.starts_with(r#"{"typeRefurbished"#) { (t.clone(), bad, u.clone()) }
            else { (t.clone(), c.clone(), bad) };
        assert!(build(t2, c2, u2, variations.clone(), vec![], &lookups).is_err(), "{label}");
    }
    // Missing parent meta omits the time; an explicit zero remains computable.
    let mut variations = vec![
        variation(15, Some(104), Some(CATEGORY_MODULE), Some(16), None),
        variation(16, Some(104), Some(CATEGORY_MODULE), None, None),
    ];
    let ship = build(t.clone(), c.clone(), u.clone(), variations.clone(), vec![], &lookups).unwrap();
    assert_eq!(ship.repair_seconds(15), None);
    variations[1].meta_level = Some(0.0);
    assert_eq!(build(t.clone(), c.clone(), u.clone(), variations.clone(), vec![], &lookups).unwrap().repair_seconds(15), Some(5.0));
    variations[1].meta_level = Some(-1.0);
    assert!(build(t, c, u, variations, vec![], &lookups).is_err());
}

#[test]
fn client_category_flag_and_exclusion_predicates() {
    let (t, c, u) = inputs();
    let ship = build(t, c, u, vec![variation(10, Some(100), Some(CATEGORY_SHIP), None, None)], vec![], &lookups()).unwrap();
    for args in [(1, CATEGORY_MODULE), (1, CATEGORY_DRONE), (1, CATEGORY_SUBSYSTEM), (1, CATEGORY_SHIP)] {
        assert!(ship.has_service_life(args.0, args.1));
    }
    assert!(!ship.has_service_life(GROUP_CAPSULE, CATEGORY_SHIP) && !ship.has_service_life(1, CATEGORY_CHARGE));
    for flag in [11, 35, 92, 99, 125, 128, FLAG_FIGHTER_BAY, FLAG_DRONE_BAY] {
        assert!(ship.repair_flag(flag), "flag {flag}");
    }
    for flag in [10, 36, 91, 100, 124, 129, FLAG_HANGAR] {
        assert!(!ship.repair_flag(flag), "flag {flag}");
    }
    assert!(ship.reduce_flag(FLAG_FIGHTER_BAY) && !ship.reduce_flag(FLAG_DRONE_BAY));
    assert!(ship.enters_recovery(1, 1, CATEGORY_MODULE) && !ship.enters_recovery(58745, 1, CATEGORY_MODULE));
}
