use super::*;

fn manifest(status: &str) -> Manifest {
    Manifest { schema_version: 1, decoder_version: "synthetic".into(), status: status.into(), phase: "preflight".into(), run_metadata: RunMetadata { run_id: "synthetic-run".into(), ..RunMetadata::default() }, build: BuildIdentity::default(), inputs: vec![], families: vec![], summary: Summary::default() }
}
fn directory() -> PathBuf {
    let id = NEXT_RUN.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("sde-report-test-{}-{id}", std::process::id()))
}
#[test]
fn preflight_replaces_old_success_and_final_reports_are_complete() {
    let dir = directory(); fs::create_dir_all(&dir).unwrap(); fs::write(dir.join("manifest.json"), br#"{"status":"success"}"#).unwrap();
    let mut run = ReportRun::begin(&dir, &manifest("preflight-failed")).unwrap();
    let current: serde_json::Value = serde_json::from_slice(&fs::read(dir.join("manifest.json")).unwrap()).unwrap(); assert_eq!(current["status"], "preflight-failed");
    assert!(run.directory.join("preflight-manifest.json").exists());
    let reports = Reports { issues: vec![Issue { severity: "warning".into(), code: "example".into(), family: "z".into(), key: Some(ReportKey::integer("2").unwrap()), path: "/a".into(), message: "synthetic".into(), source_relative_path: "a".into(), source_sha256: "b".into(), evidence: serde_json::Value::Null }, Issue { severity: "warning".into(), code: "example".into(), family: "a".into(), key: None, path: "".into(), message: "structural".into(), source_relative_path: "a".into(), source_sha256: "b".into(), evidence: serde_json::Value::Null }], ..Reports::default() };
    run.finalize(&manifest("success"), &reports).unwrap();
    let text = fs::read_to_string(run.directory.join("issues.jsonl")).unwrap(); let lines: Vec<_> = text.lines().collect(); assert_eq!(lines.len(), 2); assert!(lines[0].contains("\"family\":\"a\""));
    let current: serde_json::Value = serde_json::from_slice(&fs::read(dir.join("manifest.json")).unwrap()).unwrap(); assert_eq!(current["status"], "success");
    fs::remove_dir_all(dir).unwrap();
}
#[test]
fn publication_failure_keeps_preflight_and_unwritable_destination_fails() {
    let dir = directory(); let mut run = ReportRun::begin(&dir, &manifest("preflight-failed")).unwrap();
    fs::create_dir(run.directory.join("issues.jsonl")).unwrap();
    assert!(run.finalize(&manifest("success"), &Reports::default()).is_err());
    let current: serde_json::Value = serde_json::from_slice(&fs::read(dir.join("manifest.json")).unwrap()).unwrap(); assert_eq!(current["status"], "preflight-failed");
    let blocked = dir.join("file"); fs::write(&blocked, b"x").unwrap(); assert!(ReportRun::begin(&blocked, &manifest("preflight-failed")).is_err());
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn preflight_update_publishes_complete_input_identities() {
    let dir = directory();
    let mut run = ReportRun::begin(&dir, &manifest("preflight-failed")).unwrap();
    let mut updated = manifest("preflight-failed");
    updated.inputs.push(InputIdentity { family: "synthetic".into(), relative_path: "input.json".into(), sha256: "hash".into(), byte_length: 4, format: "json".into(), schema_identity: None, build_evidence: None });
    run.update_preflight(&updated).unwrap();
    let published = fs::read(dir.join("manifest.json")).unwrap();
    assert_eq!(published, fs::read(run.directory.join("preflight-manifest.json")).unwrap());
    let value: serde_json::Value = serde_json::from_slice(&published).unwrap();
    assert_eq!(value["inputs"][0]["relative_path"], "input.json");
    assert!(run.update_preflight(&manifest("success")).is_err());
    assert_eq!(published, fs::read(dir.join("manifest.json")).unwrap());
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn sidecars_are_single_name_durable_and_never_overwritten() {
    let dir = directory();
    let run = ReportRun::begin(&dir, &manifest("preflight-failed")).unwrap();
    run.write_sidecar("source-metadata.json", b"original").unwrap();
    assert!(run.write_sidecar("source-metadata.json", b"replacement").is_err());
    assert_eq!(fs::read(run.directory.join("source-metadata.json")).unwrap(), b"original");
    for name in ["", ".", "..", "../escape", "sub/file", "/absolute", "sub\\file", "bad\0name", "differences.jsonl", "preflight-manifest.json"] {
        assert_eq!(run.write_sidecar(name, b"x").unwrap_err().kind(), io::ErrorKind::InvalidInput, "{name:?}");
    }
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn finalization_is_one_shot_and_success_bundle_is_immutable() {
    let dir = directory();
    let mut run = ReportRun::begin(&dir, &manifest("preflight-failed")).unwrap();
    run.finalize(&manifest("success"), &Reports::default()).unwrap();
    let root = fs::read(dir.join("manifest.json")).unwrap();
    let report = fs::read(run.directory.join("issues.jsonl")).unwrap();
    assert_eq!(run.finalize(&manifest("failed"), &Reports::default()).unwrap_err().kind(), io::ErrorKind::AlreadyExists);
    assert!(run.update_preflight(&manifest("failed")).is_err());
    assert!(run.write_sidecar("late.json", b"late").is_err());
    assert_eq!(root, fs::read(dir.join("manifest.json")).unwrap());
    assert_eq!(report, fs::read(run.directory.join("issues.jsonl")).unwrap());
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn each_report_destination_collision_fails_without_replacement_or_retry() {
    let names = ["differences.jsonl", "issues.jsonl", "unknown-regions.jsonl", "ownership-summary.json", "readback-differences.jsonl"];
    for (index, name) in names.iter().enumerate() {
        let dir = directory();
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("manifest.json"), br#"{"status":"success"}"#).unwrap();
        let mut run = ReportRun::begin(&dir, &manifest("preflight-failed")).unwrap();
        fs::write(run.directory.join(name), b"existing").unwrap();
        assert!(run.finalize(&manifest("success"), &Reports::default()).is_err(), "{name}");
        assert_eq!(fs::read(run.directory.join(name)).unwrap(), b"existing");
        for previous in &names[..index] { assert!(run.directory.join(previous).is_file()); }
        assert_eq!(run.finalize(&manifest("failed"), &Reports::default()).unwrap_err().kind(), io::ErrorKind::AlreadyExists);
        let current: serde_json::Value = serde_json::from_slice(&fs::read(dir.join("manifest.json")).unwrap()).unwrap();
        assert_eq!(current["status"], "preflight-failed");
        fs::remove_dir_all(dir).unwrap();
    }
}

#[test]
fn final_manifest_rename_failure_does_not_allow_retry() {
    let dir = directory();
    let mut run = ReportRun::begin(&dir, &manifest("preflight-failed")).unwrap();
    fs::remove_file(dir.join("manifest.json")).unwrap();
    fs::create_dir(dir.join("manifest.json")).unwrap();
    assert!(run.finalize(&manifest("success"), &Reports::default()).is_err());
    let original = fs::read(run.directory.join("ownership-summary.json")).unwrap();
    assert_eq!(run.finalize(&manifest("failed"), &Reports::default()).unwrap_err().kind(), io::ErrorKind::AlreadyExists);
    assert_eq!(original, fs::read(run.directory.join("ownership-summary.json")).unwrap());
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn run_identity_is_preserved_and_cannot_change() {
    let dir = directory();
    let first = new_run_id();
    assert_ne!(first, new_run_id());
    let mut initial = manifest("preflight-failed");
    initial.run_metadata.run_id = first.clone();
    let mut run = ReportRun::begin(&dir, &initial).unwrap();
    assert_eq!(run.directory, dir.join(&first));
    let published: serde_json::Value = serde_json::from_slice(&fs::read(dir.join("manifest.json")).unwrap()).unwrap();
    assert_eq!(published["run_metadata"]["run_id"], first);
    assert_eq!(run.update_preflight(&manifest("preflight-failed")).unwrap_err().kind(), io::ErrorKind::InvalidInput);
    assert_eq!(run.finalize(&manifest("success"), &Reports::default()).unwrap_err().kind(), io::ErrorKind::InvalidInput);
    assert_eq!(run.finalize(&initial, &Reports::default()).unwrap_err().kind(), io::ErrorKind::AlreadyExists);
    fs::remove_dir_all(dir).unwrap();
}

/// Run in a separate test process under syscall fault injection. The fixture contains
/// manifest.json with old success and old-success/evidence.json with sentinel bytes.
#[test]
#[ignore]
fn report_fault_scenario() {
    let dir = PathBuf::from(std::env::var_os("INFINITY_REPORT_FAULT_DIR").expect("fixture directory"));
    let phase = std::env::var("INFINITY_REPORT_FAULT_PHASE").expect("begin or finalize phase");
    assert!(phase == "begin" || phase == "finalize");
    let old_manifest = fs::read(dir.join("manifest.json")).unwrap();
    let old: serde_json::Value = serde_json::from_slice(&old_manifest).unwrap();
    assert_eq!(old["status"], "success");
    let sentinel_path = dir.join("old-success/evidence.json");
    let sentinel = fs::read(&sentinel_path).unwrap();
    let names = ["differences.jsonl", "issues.jsonl", "unknown-regions.jsonl", "ownership-summary.json", "readback-differences.jsonl"];
    let mut stage = "begin";
    let mut run = None;
    let result = match ReportRun::begin(&dir, &manifest("preflight-failed")) {
        Err(e) => Err(e),
        Ok(mut started) if phase == "finalize" => {
            stage = "update_preflight";
            let result = started.update_preflight(&manifest("preflight-failed")).and_then(|()| {
                stage = "finalize";
                started.finalize(&manifest("success"), &Reports::default())
            });
            run = Some(started);
            result
        }
        Ok(started) => { run = Some(started); Ok(()) }
    };
    eprintln!("report_fault_scenario stage={stage} result={result:?}");
    assert_eq!(fs::read(sentinel_path).unwrap(), sentinel);
    let current_bytes = fs::read(dir.join("manifest.json")).unwrap();
    let current: serde_json::Value = serde_json::from_slice(&current_bytes).unwrap();
    if current_bytes == old_manifest {
        let error = result.as_ref().expect_err("old success cannot represent this invocation");
        assert_eq!(stage, "begin");
        assert!(error.to_string().contains("report start marker publication failed"), "{error}");
        assert!(error.to_string().contains("database must not connect"), "{error}");
    } else if current["status"] == "success" {
        // A final root-directory fsync failure may occur after the rename.
        assert_eq!(stage, "finalize");
        let started = run.as_ref().unwrap();
        for name in names { assert!(started.directory.join(name).is_file(), "{name}"); }
    } else {
        assert_eq!(current["status"], "preflight-failed");
        assert_eq!(current["run_metadata"]["run_id"], "synthetic-run");
    }
    if let Some(mut started) = run {
        if stage == "finalize" {
            let before: Vec<_> = names.iter().map(|name| fs::read(started.directory.join(name)).ok()).collect();
            assert_eq!(started.finalize(&manifest("failed"), &Reports::default()).unwrap_err().kind(), io::ErrorKind::AlreadyExists);
            for (name, bytes) in names.iter().zip(before) {
                assert_eq!(fs::read(started.directory.join(name)).ok(), bytes, "{name}");
            }
            assert_eq!(fs::read(dir.join("manifest.json")).unwrap(), current_bytes);
        }
    }
    if result.is_ok() {
        assert_eq!(current["status"], if phase == "begin" { "preflight-failed" } else { "success" });
    }
}
