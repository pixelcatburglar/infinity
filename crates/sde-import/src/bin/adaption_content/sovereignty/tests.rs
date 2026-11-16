use super::*;
use serde_json::{json, Value};
use std::{fs, path::PathBuf};

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("indices-test-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Temp { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); } }

/// Synthetic members exercising only the whitelisted grammar: named literal
/// assignments, one `def` heading and the `enumerate` table literal. Values
/// are test fixtures, not client data.
const FILES: [(&str, &str); 3] = [
    (DEV_INDEX_UTIL, "timeIndexLevels = {1: 2, 2: 4, 3: 6, 4: 8, 5: 10}\ndevIndexDecayRate = 100\ndevIndexDecayTime = 3\n\ndef GetDevIndexLevels():\n    totalLevels = 5\n    for i, (militaryModifier, industrialModifier) in enumerate([(5, 7), (3, 2), (2, 1.5), (1.5, 1.2), (1, 0.7), (0.8, 0.35)]):\n        pass\n\ndef GetTimeIndexLevels():\n    day = 10\n"),
    (OCCUPANCY_CALCULATOR, "MIN_OCCUPANCY_BONUS = 1.0\nMAX_OCCUPANCY_BONUS = 6.0\nMILITARY_INDEX_TO_BONUS = {1: 0.5, 2: 1.0, 3: 1.5, 4: 2.0, 5: 2.5}\nINDUSTRIAL_INDEX_TO_BONUS = {1: 0.5, 2: 1.0, 3: 1.5, 4: 2.0, 5: 2.5}\nSTRATEGIC_INDEX_TO_BONUS = {1: 0.2, 2: 0.4, 3: 0.6, 4: 0.8, 5: 1.0}\nCAPITAL_BONUS = 2.0\nBASE_BONUS = 1.0\n"),
    (DOGMA_CONST, "attributeDevIndexMilitary = 101\nattributeDevIndexIndustrial = 102\nattributeDevIndexSovereignty = 103\n"),
];

fn client_source(temp: &Temp) -> PathBuf {
    let root = temp.0.join("client");
    for (name, text) in FILES {
        let path = root.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }
    root
}

#[test]
fn extract_parses_whitelisted_tables() {
    let temp = Temp::new();
    let root = client_source(&temp);
    let extracted = extract(&root, "build-a").unwrap();
    assert!(extracted.sources.len() == 3 && extracted.sources.values().all(|s| s.bytes > 0 && s.sha256.len() == 64));
    let envelope: Value = serde_json::from_slice(&extracted.bytes).unwrap();
    assert_eq!(envelope["schema"], "infinity-content-dataset");
    let data = &envelope["records"][0]["data"];
    assert_eq!(data["attribute_ids"], json!({"military":101,"industrial":102,"strategic":103}));
    assert_eq!(data["military_levels"][0], json!({"level":0,"min_points":0.0,"max_points":300.0,"modifier":5.0}));
    assert_eq!(data["industrial_levels"][5]["modifier"], 0.35);
    assert_eq!(data["strategic_seconds"], json!([20, 40, 60, 80, 100]));
    assert_eq!(data["occupancy"]["capital"], 2.0);
    assert_eq!(data["occupancy"]["military"][0], json!({"level":1,"bonus":0.5}));
}

#[test]
fn unsupported_or_missing_source_fails() {
    let temp = Temp::new();
    let root = client_source(&temp);
    for (name, from, to) in [
        (DEV_INDEX_UTIL, "devIndexDecayTime = 3\n", ""),
        (DEV_INDEX_UTIL, "devIndexDecayRate = 100", "devIndexDecayRate = 100\ndevIndexDecayRate = 100"),
        (DEV_INDEX_UTIL, "(5, 7)", "(5, x)"),
        (DEV_INDEX_UTIL, "1: 2", "0: 2"),
        (DEV_INDEX_UTIL, "(militaryModifier, industrialModifier)", "(industrialModifier, militaryModifier)"),
        (OCCUPANCY_CALCULATOR, "4: 2.0", "4: x"),
        (DOGMA_CONST, "attributeDevIndexMilitary", "attributeDevIndexRemoved"),
    ] {
        let path = root.join(name);
        let original = fs::read_to_string(&path).unwrap();
        fs::write(&path, original.replacen(from, to, 1)).unwrap();
        assert!(extract(&root, "build-a").is_err(), "{name}: {from}");
        fs::write(&path, original).unwrap();
    }
    assert!(extract(&root, "a/b").is_err() && extract(&temp.0.join("missing"), "build-a").is_err());
}

#[test]
fn standalone_domain_publishes() {
    let temp = Temp::new();
    let root = client_source(&temp);
    let output = temp.0.join("output");
    fs::create_dir(&output).unwrap();
    let relative = super::super::publish(None, Some((root.as_path(), "build-a")), &output, true).unwrap();
    let manifest: Value = serde_json::from_slice(&fs::read(output.join(&relative).join("manifest.json")).unwrap()).unwrap();
    assert_eq!(manifest["schema"], "infinity-content");
    assert!(!manifest.as_object().unwrap().contains_key("adaption") && manifest["datasets"].as_object().unwrap().len() == 1 && manifest["sources"].as_object().unwrap().len() == 3);
    assert_eq!(fs::read_to_string(output.join("current")).unwrap().trim_end(), relative);
}
