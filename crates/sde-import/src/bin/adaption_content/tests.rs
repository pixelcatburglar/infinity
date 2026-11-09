use super::*;
use serde_json::json;

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("adaption-test-{}-{}", std::process::id(), SERIAL.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Temp { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); } }

fn fixture() -> (Temp, PathBuf, PathBuf, PathBuf) {
    let temp = Temp::new();
    let source = temp.0.join("input"); let output = temp.0.join("output"); let constants = temp.0.join("const.py");
    fs::create_dir(&source).unwrap(); fs::create_dir(source.join("datasets")).unwrap(); fs::create_dir(&output).unwrap();
    fs::write(&constants, "First = 19\nSecond = 23\nTypeAdaptionChips = (\n    Second,\n    First,\n)\n").unwrap();
    let mut datasets = serde_json::Map::new();
    for family in input::FAMILIES {
        let path = format!("datasets/{family}.json");
        let singleton = family == "shipadaptionconfig" || family == "shipadaptionchipexchangeconfig";
        let enum_family = family == "shipadaptionnodetypes" || family == "shipadaptionnodeunlocktypes";
        let record = if singleton { json!({"key":"","data":{}}) } else if enum_family { json!({"key":"0","data":{"_key":0}}) } else { json!({"key":"17","data":{"_key":17}}) };
        let data = serde_json::to_vec(&json!({"client_build":"synthetic", "schema":"cfsd-adaptation-dataset", "schema_version":1, "source_resource":"synthetic", "records":[record]})).unwrap();
        fs::write(source.join(&path), &data).unwrap();
        datasets.insert(family.into(), json!({"path":path,"records":1,"sha256":input::hash(&data)}));
    }
    fs::write(source.join("manifest.json"), serde_json::to_vec(&json!({"schema":"cfsd-adaptation-bundle", "schema_version":1, "client_build":"synthetic", "datasets":datasets, "validation":{"incomplete_for_gameplay":true}})).unwrap()).unwrap();
    (temp, source, constants, output)
}

#[test]
fn dataset_discriminator_is_rejected_at_bundle_root() {
    let (_temp, source, _, _) = fixture();
    let path = source.join("manifest.json");
    let mut manifest: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    manifest["schema"] = json!("cfsd-adaptation-dataset");
    fs::write(path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    assert_eq!(input::load(&source).err().unwrap().to_string(), "unsupported source manifest schema");
}

#[test]
fn constants_are_restricted_and_sorted() {
    let (ids, names) = input::constants(b"Alpha = 23\nBeta = 19\nTypeAdaptionChips = (Alpha, Beta,)\n").unwrap();
    assert_eq!(ids, vec![19, 23]); assert_eq!(names, vec!["Alpha", "Beta"]);
    for source in ["Alpha = 1 + 2\nTypeAdaptionChips = (Alpha,)\n", "Alpha = 0\nTypeAdaptionChips = (Alpha,)\n", "Alpha = 2\nAlpha = 3\nTypeAdaptionChips = (Alpha,)\n", "Alpha = 2\nTypeAdaptionChips = (Alpha, Alpha)\n"] {
        assert!(input::constants(source.as_bytes()).is_err());
    }
}

#[test]
fn constants_ignore_indented_suite_assignments() {
    assert!(input::constants(b"if True:\n    Alpha = 2\nTypeAdaptionChips = (Alpha,)\n").is_err());
    assert!(input::constants(b"Alpha = 2\nif True:\n    TypeAdaptionChips = (Alpha,)\n").is_err());
    assert_eq!(input::constants(b"Alpha = 2\nTypeAdaptionChips = (\n    Alpha,\n)\n").unwrap().0, vec![2]);
}

#[test]
fn contained_dataset_alias_is_rejected() {
    let (_temp, source, _, _) = fixture();
    let manifest_path = source.join("manifest.json");
    let mut manifest: serde_json::Value = serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
    let family = input::FAMILIES[0];
    fs::copy(source.join(format!("datasets/{family}.json")), source.join("datasets/alias.json")).unwrap();
    manifest["datasets"][family]["path"] = json!("datasets/alias.json");
    fs::write(manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    assert!(input::load(&source).is_err());
}

#[test]
fn publication_is_stable_and_failure_preserves_current() {
    let (_temp, source, constants, output) = fixture();
    let first = publish(&source, &constants, &output, true).unwrap();
    assert_eq!(first, publish(&source, &constants, &output, true).unwrap());
    let current = fs::read(output.join("current")).unwrap();
    let manifest: Value = serde_json::from_slice(&fs::read(output.join(&first).join("manifest.json")).unwrap()).unwrap();
    assert_eq!(manifest["chip_types"], json!([19, 23]));
    assert_eq!(manifest["validation"]["scope"], "chip-ledger");
    assert_eq!(manifest["provenance"]["source_validation"]["incomplete_for_gameplay"], true);
    fs::write(&constants, "TypeAdaptionChips = (Unknown,)\n").unwrap();
    assert!(publish(&source, &constants, &output, true).is_err());
    assert_eq!(fs::read(output.join("current")).unwrap(), current);
}

#[test]
fn malformed_and_changed_bundle_is_rejected() {
    let (_temp, source, constants, output) = fixture();
    let path = source.join("manifest.json");
    let original = fs::read(&path).unwrap();
    let mut manifest: Value = serde_json::from_slice(&original).unwrap();
    manifest["datasets"].as_object_mut().unwrap().remove(input::FAMILIES[0]);
    fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    assert!(publish(&source, &constants, &output, false).is_err());
    manifest = serde_json::from_slice(&original).unwrap();
    manifest["datasets"][input::FAMILIES[0]]["path"] = json!("../escape");
    fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    assert!(publish(&source, &constants, &output, false).is_err());
    fs::write(&path, original).unwrap();
    fs::write(source.join(format!("datasets/{}.json", input::FAMILIES[0])), b"{}").unwrap();
    assert!(publish(&source, &constants, &output, false).is_err());
}

#[test]
fn count_build_and_null_are_checked_before_publication() {
    let (_temp, source, constants, output) = fixture();
    let path = source.join("manifest.json");
    let original = fs::read(&path).unwrap();
    let mut manifest: Value = serde_json::from_slice(&original).unwrap();
    manifest["datasets"][input::FAMILIES[0]]["records"] = json!(2);
    fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    assert!(publish(&source, &constants, &output, false).is_err());
    manifest = serde_json::from_slice(&original).unwrap();
    manifest["client_build"] = json!("other-build");
    fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    assert!(publish(&source, &constants, &output, false).is_err());
    manifest = serde_json::from_slice(&original).unwrap();
    manifest["validation"]["invalid"] = Value::Null;
    fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    assert!(publish(&source, &constants, &output, false).is_err());
}

#[test]
fn damaged_existing_package_does_not_switch_current() {
    let (_temp, source, constants, output) = fixture();
    let package = publish(&source, &constants, &output, true).unwrap();
    let before = fs::read(output.join("current")).unwrap();
    fs::write(output.join(package).join("manifest.json"), b"damaged").unwrap();
    assert!(publish(&source, &constants, &output, true).is_err());
    assert_eq!(fs::read(output.join("current")).unwrap(), before);
}

#[test]
fn schema_singleton_and_key_identity_are_checked() {
    for (family, mutation) in [
        ("shipadaptionnodes", 0),
        ("shipadaptionnodes", 1),
        ("shipadaptionconfig", 2),
    ] {
        let (_temp, source, constants, output) = fixture();
        let path = source.join(format!("datasets/{family}.json"));
        let mut dataset: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        match mutation {
            0 => dataset["schema"] = json!("unknown"),
            1 => dataset["records"][0]["data"]["_key"] = json!(29),
            _ => dataset["records"][0]["key"] = json!("1"),
        }
        let data = serde_json::to_vec(&dataset).unwrap();
        fs::write(path, &data).unwrap();
        let manifest_path = source.join("manifest.json");
        let mut manifest: Value = serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
        manifest["datasets"][family]["sha256"] = json!(input::hash(&data));
        fs::write(manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        assert!(publish(&source, &constants, &output, false).is_err());
    }
}
