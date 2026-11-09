use super::*;
use serde_json::json;

fn bundle() -> Bundle {
    let mut files = BTreeMap::new();
    let mut datasets = BTreeMap::new();
    for family in FAMILIES {
        let singleton = family == "shipadaptionconfig" || family == "shipadaptionchipexchangeconfig";
        let record = if singleton { json!({"key":"", "data":{}}) } else { json!({"key":"1", "data":{"_key":1}}) };
        let bytes = serde_json::to_vec(&json!({"schema":"cfsd-adaptation-dataset", "schema_version":1, "client_build":"test", "source_resource":"test", "records":[record]})).unwrap();
        datasets.insert(family, json!({"path":format!("datasets/{family}.json"), "records":1, "sha256":sha256_hex(&bytes)}));
        files.insert(family.to_owned(), bytes);
    }
    let manifest = serde_json::to_vec(&json!({"schema":"infinity-adaption-content", "schema_version":1, "client_build":"test", "source_manifest_sha256":"0".repeat(64), "constants_sha256":"0".repeat(64), "chip_types":[1], "datasets":datasets, "provenance":{"source_validation":{}, "chip_type_symbols":["test"]}, "validation":{"scope":"chip-ledger", "node_task_effect_gameplay_enabled":false}})).unwrap();
    Bundle { manifest, files, parsed: BTreeMap::new(), chip_types: Vec::new() }
}

#[test]
fn retained_bundle_revalidates_without_filesystem() {
    let original = bundle();
    let encoded = serde_json::to_vec(&original).unwrap();
    let mut decoded: Bundle = serde_json::from_slice(&encoded).unwrap();
    assert!(decoded.validate(1_000_000).is_ok());
    assert_eq!(decoded.manifest_hash(), original.manifest_hash());
}

#[test]
fn corrupt_or_missing_member_is_rejected() {
    let mut candidate = bundle();
    candidate.files.get_mut(FAMILIES[0]).unwrap().push(b' ');
    assert!(candidate.validate(1_000_000).is_err());
    candidate.files.remove(FAMILIES[0]);
    assert!(candidate.validate(1_000_000).is_err());
}

#[test]
fn malformed_envelopes_and_manifest_members_are_rejected() {
    for mutation in ["null", "build", "count", "key", "path", "duplicate"] {
        let mut candidate = bundle();
        let mut manifest: Value = serde_json::from_slice(&candidate.manifest).unwrap();
        let mut dataset: Value = serde_json::from_slice(&candidate.files[FAMILIES[0]]).unwrap();
        match mutation {
            "null" => dataset["records"][0]["data"]["bad"] = Value::Null,
            "build" => dataset["client_build"] = json!("different"),
            "count" => manifest["datasets"][FAMILIES[0]]["records"] = json!(2),
            "key" => dataset["records"][0]["key"] = json!("01"),
            "path" => manifest["datasets"][FAMILIES[0]]["path"] = json!("../outside"),
            "duplicate" => {
                let record = dataset["records"][0].clone();
                dataset["records"].as_array_mut().unwrap().push(record);
                manifest["datasets"][FAMILIES[0]]["records"] = json!(2);
            }
            _ => unreachable!(),
        }
        let bytes = serde_json::to_vec(&dataset).unwrap();
        manifest["datasets"][FAMILIES[0]]["sha256"] = json!(sha256_hex(&bytes));
        candidate.files.insert(FAMILIES[0].to_owned(), bytes);
        candidate.manifest = serde_json::to_vec(&manifest).unwrap();
        assert!(candidate.validate(1_000_000).is_err(), "{mutation}");
    }
}

#[test]
fn unsafe_pointer_components_are_rejected() {
    for component in ["", ".", "..", "a/b", "a\\b"] { assert!(!ordinary(component)); }
    assert!(!digest(&"A".repeat(64)));
    assert!(digest(&"a".repeat(64)));
}

#[test]
fn pointer_load_is_snapshot_and_rejects_corruption() {
    let root = std::env::temp_dir().join(format!("adaption-bundle-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    let original = bundle();
    let hash = original.manifest_hash();
    let directory = root.join("test").join(&hash);
    std::fs::create_dir_all(directory.join("datasets")).unwrap();
    std::fs::write(directory.join("manifest.json"), &original.manifest).unwrap();
    for (name, bytes) in &original.files { std::fs::write(directory.join("datasets").join(format!("{name}.json")), bytes).unwrap(); }
    std::fs::write(root.join("current"), format!("test/{hash}\n")).unwrap();
    let captured = Bundle::load(&root, 1_000_000).unwrap();
    std::fs::write(root.join("current"), "test/invalid\n").unwrap();
    assert_eq!(captured.manifest_hash(), hash);
    assert!(Bundle::load(&root, 1_000_000).is_err());
    std::fs::write(root.join("current"), format!("test/{hash}\n")).unwrap();
    std::fs::write(directory.join("datasets").join(format!("{}.json", FAMILIES[0])), b"bad").unwrap();
    assert!(Bundle::load(&root, 1_000_000).is_err());
    assert_eq!(captured.records(FAMILIES[0]).unwrap().len(), 1);
    std::fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[test]
fn pointer_symlink_is_not_a_regular_file() {
    let root = std::env::temp_dir().join(format!("adaption-pointer-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("other"), b"test").unwrap();
    std::os::unix::fs::symlink(root.join("other"), root.join("current")).unwrap();
    assert!(Bundle::load(&root, 1_000_000).is_err());
    std::fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[test]
fn atomic_pointer_swaps_never_mix_members() {
    let root = std::env::temp_dir().join(format!("adaption-swap-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    let first = bundle();
    let mut second = first.clone();
    let mut manifest: Value = serde_json::from_slice(&second.manifest).unwrap();
    manifest["chip_types"] = json!([1, 2]);
    second.manifest = serde_json::to_vec(&manifest).unwrap();
    let hashes = [first.manifest_hash(), second.manifest_hash()];
    for (candidate, hash) in [(&first, &hashes[0]), (&second, &hashes[1])] {
        let directory = root.join("test").join(hash);
        std::fs::create_dir_all(directory.join("datasets")).unwrap();
        std::fs::write(directory.join("manifest.json"), &candidate.manifest).unwrap();
        for (name, bytes) in &candidate.files { std::fs::write(directory.join("datasets").join(format!("{name}.json")), bytes).unwrap(); }
    }
    std::fs::write(root.join("current"), format!("test/{}\n", hashes[0])).unwrap();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let writer_root = root.clone();
    let writer_hashes = hashes.clone();
    let writer_barrier = barrier.clone();
    let writer = std::thread::spawn(move || {
        writer_barrier.wait();
        for i in 0..100 {
            let temporary = writer_root.join("next");
            std::fs::write(&temporary, format!("test/{}\n", writer_hashes[i % 2])).unwrap();
            std::fs::rename(temporary, writer_root.join("current")).unwrap();
        }
    });
    barrier.wait();
    for _ in 0..100 {
        let candidate = Bundle::load(&root, 1_000_000).unwrap();
        assert!(hashes.contains(&candidate.manifest_hash()));
        assert!(std::ptr::eq(candidate.records(FAMILIES[0]).unwrap(), candidate.records(FAMILIES[0]).unwrap()));
    }
    writer.join().unwrap();
    std::fs::remove_dir_all(root).unwrap();
}
