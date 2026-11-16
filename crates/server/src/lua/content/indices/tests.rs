//! Compile and retained-package behavior with independent synthetic fixtures;
//! the shipped Lua source decides the values, client tables are never copied.

use super::*;

const SOURCE: &[u8] = include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/lua/content/sovereignty_indices/policy.lua"));
const AUTHORED: &[u8] = include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/lua/content/sovereignty_indices/data/policy.json"));

fn authored(fields: &[(&str, serde_json::Value)]) -> Vec<u8> {
    let mut data = serde_json::json!({
        "version": "9", "source": "test",
        "military_points_per_death": 1000.0, "industrial_points_per_m3": 2.0,
        "decay_points": 50.0, "decay_interval_seconds": 60,
    });
    for (key, value) in fields { data[*key] = value.clone(); }
    serde_json::to_vec(&data).unwrap()
}

fn dataset(mutate: impl FnOnce(&mut serde_json::Value)) -> Vec<u8> {
    let bands = |width: f64, modifier: f64| (0..6).map(|level| serde_json::json!({
        "level": level, "min_points": level as f64 * width, "max_points": (level + 1) as f64 * width, "modifier": modifier,
    })).collect::<Vec<_>>();
    let bonuses = |bonus: f64| (1..=5).map(|level| serde_json::json!({ "level": level, "bonus": bonus })).collect::<Vec<_>>();
    let mut data = serde_json::json!({
        "schema": "infinity-content-dataset", "schema_version": 1, "client_build": "t", "source_resource": "test",
        "records": [{ "key": "", "data": {
            "attribute_ids": { "military": 11, "industrial": 22, "strategic": 33 },
            "military_levels": bands(10.0, 2.0), "industrial_levels": bands(20.0, 3.0),
            "strategic_seconds": [10, 20, 40, 80, 160],
            "occupancy": { "base": 1.0, "min": 1.0, "max": 6.0, "capital": 2.0,
                "military": bonuses(0.5), "industrial": bonuses(0.25), "strategic": bonuses(0.1) },
        }}],
    });
    mutate(&mut data);
    serde_json::to_vec(&data).unwrap()
}

fn compile_source(source: &[u8], authored: &[u8], dataset: &[u8]) -> mlua::Result<Definitions> {
    compile(&Lua::new(), source, authored, dataset, &LuaConfig::default(), None)
}

#[test]
fn shipped_compile_maps_authored_and_dataset_to_typed_parameters() {
    let definitions = compile_source(SOURCE, AUTHORED, &dataset(|_| {})).unwrap();
    assert_eq!((definitions.military_points_per_death, definitions.industrial_points_per_m3, definitions.decay_points), (100000.0, 1.0, 100000.0));
    assert_eq!(definitions.decay_interval_ticks, 900 * FILETIME_SECOND);
    assert_eq!((definitions.military_attribute, definitions.industrial_attribute, definitions.strategic_attribute), (11, 22, 33));
    assert_eq!((definitions.military[3].min_points, definitions.military[5].max_points, definitions.military[4].modifier), (30.0, 60.0, 2.0));
    assert_eq!((definitions.industrial[0].min_points, definitions.industrial[5].max_points), (0.0, 120.0));
    assert_eq!(definitions.strategic_seconds, [10, 20, 40, 80, 160]);
    assert_eq!((definitions.occupancy.base, definitions.occupancy.capital, definitions.occupancy.strategic[3].bonus), (1.0, 2.0, 0.1));
}

#[test]
fn authored_source_and_json_decide_the_policy_values() {
    let source = String::from_utf8(SOURCE.to_vec()).unwrap();
    let doubled = source.replacen("authored.decay_points", "(authored.decay_points * 2)", 1);
    assert_ne!(source, doubled);
    let changed = compile_source(doubled.as_bytes(), AUTHORED, &dataset(|_| {})).unwrap();
    assert_eq!(changed.decay_points, 200000.0);
    let alternative = compile_source(SOURCE, &authored(&[("military_points_per_death", serde_json::json!(7.0))]), &dataset(|_| {})).unwrap();
    assert_eq!(alternative.military_points_per_death, 7.0);
}

#[test]
fn invalid_exports_tables_and_inputs_are_rejected() {
    for source in [
        b"return {}".as_slice(),
        b"return { Compile = function() return 3 end }".as_slice(),
        b"return { Compile = function() return { attributes = {} } end }".as_slice(),
        b"return { Compile = function() error('x') end, Extra = function() end }".as_slice(),
    ] {
        assert!(compile_source(source, &authored(&[]), &dataset(|_| {})).is_err());
    }
    assert!(compile_source(SOURCE, &authored(&[("decay_points", serde_json::json!(-1.0))]), &dataset(|_| {})).is_err());
    assert!(compile_source(SOURCE, &authored(&[]), &dataset(|data| data["records"][0]["data"]["attribute_ids"]["industrial"] = serde_json::json!(11))).is_err());
    assert!(compile_source(SOURCE, &authored(&[]), &dataset(|data| data["records"][0]["data"]["military_levels"][3]["min_points"] = serde_json::json!(0.0))).is_err());
    assert!(compile_source(SOURCE, &authored(&[]), br#"{"records":[]}"#.as_slice()).is_err());
    assert!(compile(&Lua::new(), SOURCE, &authored(&[]), &dataset(|_| {}), &LuaConfig::default(), Some(&crate::sde::Sde::default())).is_err());
}

#[test]
fn retained_roundtrip_recompiles_and_rejects_tampering() {
    let dataset = dataset(|_| {});
    let compiled = compile_source(SOURCE, AUTHORED, &dataset).unwrap();
    let data = retained_data(AUTHORED, &dataset, &compiled).unwrap();
    let version = format!("{}/{}", authored_version(AUTHORED).unwrap(), sha256_hex(&dataset));
    assert_eq!(rebuild(&Lua::new(), SOURCE, &data, &version, &LuaConfig::default(), None).unwrap(), compiled);
    assert!(rebuild(&Lua::new(), SOURCE, &data, "9/deadbeef", &LuaConfig::default(), None).is_err());
    let mut tampered: serde_json::Value = serde_json::from_slice(&data).unwrap();
    tampered["compiled"]["decay_points"] = serde_json::json!(compiled.decay_points + 1.0);
    assert!(rebuild(&Lua::new(), SOURCE, &serde_json::to_vec(&tampered).unwrap(), &version, &LuaConfig::default(), None).is_err());
}
