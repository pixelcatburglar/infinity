use super::*;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Fixture { root: PathBuf, source: input::Input, run: ReportRun }
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("sde-owned sqlite #{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("blueprints.static");
        let db = rusqlite::Connection::open(&path).unwrap();
        db.execute_batch("CREATE TABLE cache (key TEXT PRIMARY KEY, value TEXT); INSERT INTO cache VALUES ('7', '{\"blueprintTypeID\":7,\"marker\":\"A\"}');").unwrap();
        drop(db);
        let source = input::read_input(&root, "blueprints", "blueprints.static").unwrap();
        let manifest = crate::initial_manifest(crate::report::manifest::new_run_id());
        let run = ReportRun::begin(&root.join("report"), &manifest).unwrap();
        Self { root, source, run }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) { std::fs::remove_dir_all(&self.root).unwrap(); }
}

#[test]
fn owned_sqlite_ignores_original_change_and_restore() {
    let fixture = Fixture::new();
    let db = rusqlite::Connection::open(&fixture.source.path).unwrap();
    db.execute("UPDATE cache SET value = ?1 WHERE key = '7'", [r#"{"blueprintTypeID":7,"marker":"B"}"#]).unwrap();
    drop(db);
    assert!(input::recheck(&fixture.source).is_err());
    let staged = stage_blueprint_source(&fixture.source, &fixture.run).unwrap();
    let decoded = blueprints::decode(&staged).unwrap();
    assert_eq!(decoded.records[0]["marker"], "A");
    std::fs::write(&fixture.source.path, &*fixture.source.bytes).unwrap();
    input::recheck(&fixture.source).unwrap();
    assert_eq!(blueprints::decode(&staged).unwrap().records[0]["marker"], "A");
}

#[test]
fn original_journals_block_staging_and_tampering_blocks_verification() {
    for suffix in ["-wal", "-journal"] {
        let fixture = Fixture::new();
        let mut name = fixture.source.path.as_os_str().to_owned();
        name.push(suffix);
        std::fs::write(PathBuf::from(name), b"x").unwrap();
        assert!(stage_blueprint_source(&fixture.source, &fixture.run).is_err());
        assert!(!fixture.run.directory().join(BLUEPRINT_SOURCE).exists());
    }
    let fixture = Fixture::new();
    let staged = stage_blueprint_source(&fixture.source, &fixture.run).unwrap();
    std::fs::write(&staged, b"tampered").unwrap();
    assert!(verify_blueprint_source(&fixture.source, &staged).is_err());
}

#[test]
fn language_projection_failure_reaches_blocked_report_with_snapshot_identity() {
    for bad in [r#"{"_key":2,"languageID":"zh"}"#, r#"{"_key":2}"#] {
        let fixture = Fixture::new();
        let family = families::FAMILIES.iter().find(|f| f.name == "localization_fsd_en_us").unwrap();
        let old_path = "eve-online-client-static-data-test/localization.jsonl";
        std::fs::create_dir_all(fixture.root.join("eve-online-client-static-data-test")).unwrap();
        std::fs::write(fixture.root.join(old_path), bad).unwrap();
        let old_source = input::read_input(&fixture.root, family.name, old_path).unwrap();
        std::fs::create_dir_all(fixture.root.join("localizationfsd")).unwrap();
        std::fs::write(fixture.root.join(family.resource), b"client bytes").unwrap();
        let mut client_source = input::read_input(&fixture.root, family.name, family.resource).unwrap();
        client_source.decoded_records = Some(7);
        let mut rows = exact_rows([r#"{"_key":1,"languageID":"en-us"}"#.to_owned(), bad.to_owned()].into_iter()).unwrap();
        let original = rows.clone();
        let error = project_language(family, &old_source, &mut rows).err().unwrap();
        assert_eq!(rows, original);
        let inputs = vec![client_source, old_source.clone()];
        let mut manifest = crate::initial_manifest(crate::report::manifest::new_run_id());
        let mut reports = Reports::default();
        crate::blocked_manifest(&mut manifest, &inputs, &mut reports, &fixture.run, &error);
        assert_eq!(manifest.summary.blocked_families, 16);
        assert_eq!(manifest.summary.enabled_families, 0);
        assert!(manifest.families.iter().all(|f| f.status == "blocked" && f.counts.output_native_records == 0 && f.counts.output_compat_records == 0));
        let failed = manifest.families.iter().find(|f| f.family == family.name).unwrap();
        assert_eq!(failed.counts.input_records, 7);
        assert_eq!(failed.validation.issue_count, 1);
        assert!(failed.reason.as_ref().unwrap().contains("snapshot provenance projection"));
        assert_eq!(failed.compat_table, None);
        assert!(serde_json::to_value(failed).unwrap()["compat_table"].is_null());
        assert_eq!(reports.issues.len(), 1);
        let issue = &reports.issues[0];
        assert_eq!(issue.family, family.name);
        assert_eq!(issue.path, "/languageID");
        assert_eq!(issue.source_relative_path, old_path);
        assert_eq!(issue.source_sha256, old_source.sha256);
        assert_eq!(issue.evidence["stage"], "snapshot provenance projection");
    }
}

#[test]
fn typed_builder_failure_reaches_real_blocked_manifest_with_source_identity() {
    let fixture = Fixture::new();
    let family = families::FAMILIES.iter().find(|f| f.name == "types").unwrap();
    let path = fixture.root.join(family.resource);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, b"client fixture").unwrap();
    let source = input::read_input(&fixture.root, family.name, family.resource).unwrap();
    let mut compat_raw = BTreeMap::new();
    for name in ["groups", "types", "categories", "typematerials", "dogmaattributes", "dogmaeffects", "typedogma", "blueprints"] { compat_raw.insert(name.into(), vec![]); }
    compat_raw.get_mut("types").unwrap().push(r#"{"_key":42,"graphicID":2147483648}"#.into());
    let decoded = Decoded { native: BTreeMap::new(), native_raw: BTreeMap::new(), compat_raw };
    let error = prepare_typed(&decoded).err().unwrap();
    let mut manifest = crate::initial_manifest(crate::report::manifest::new_run_id());
    let mut reports = Reports::default();
    crate::blocked_manifest(&mut manifest, &[source.clone()], &mut reports, &fixture.run, &error);
    assert_eq!(manifest.summary.blocked_families, 16);
    assert_eq!(manifest.summary.enabled_families, 0);
    assert!(manifest.families.iter().all(|f| f.counts.output_native_records == 0 && f.counts.output_compat_records == 0));
    let issue = &reports.issues[0];
    assert_eq!(issue.family, "types");
    assert_eq!(issue.key, Some(ReportKey::integer("42").unwrap()));
    assert_eq!(issue.path, "/graphicID");
    assert_eq!(issue.source_sha256, source.sha256);
    assert_eq!(issue.source_relative_path, source.relative_path);
}

#[test]
fn jsonb_nul_refuses_values_keys_and_string_record_key() {
    for value in [r#"{"_key":"a\u0000b"}"#, r#"{"_key":1,"x":"\u0000"}"#, r#"{"_key":1,"x\u0000y":1}"#] {
        assert!(check_jsonb_storage(&crate::report::diff::parse_json(value).unwrap(), "").is_err());
    }
    assert!(check_jsonb_storage(&crate::report::diff::parse_json(r#"{"_key":"ok","x":"\n\t\\u0000"}"#).unwrap(), "").is_ok());
}

#[test]
fn malformed_typed_keys_reach_blocked_manifest_without_inventing_key() {
    let fixture = Fixture::new();
    let family = families::FAMILIES.iter().find(|f| f.name == "types").unwrap();
    let path = fixture.root.join(family.resource);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, b"client fixture").unwrap();
    let source = input::read_input(&fixture.root, family.name, family.resource).unwrap();
    for row in [r#"{"_key":1"#, r#"{}"#, r#"{"_key":true}"#, r#"{"_key":1.0}"#] {
        let mut compat_raw = BTreeMap::new();
        for name in ["groups", "types", "categories", "typematerials", "dogmaattributes", "dogmaeffects", "typedogma", "blueprints"] { compat_raw.insert(name.into(), vec![]); }
        compat_raw.get_mut("types").unwrap().push(row.into());
        let decoded = Decoded { native: BTreeMap::new(), native_raw: BTreeMap::new(), compat_raw };
        let error = prepare_typed(&decoded).err().unwrap();
        let mut manifest = crate::initial_manifest(crate::report::manifest::new_run_id());
        let mut reports = Reports::default();
        crate::blocked_manifest(&mut manifest, &[source.clone()], &mut reports, &fixture.run, &error);
        assert_eq!(manifest.summary.blocked_families, 16);
        assert_eq!(manifest.summary.enabled_families, 0);
        assert!(manifest.families.iter().all(|f| f.counts.output_native_records == 0 && f.counts.output_compat_records == 0));
        let issue = &reports.issues[0];
        assert_eq!(issue.family, "types");
        assert_eq!(issue.key, None);
        assert_eq!(issue.path, "/_key");
        assert_eq!(issue.source_relative_path, source.relative_path);
        assert_eq!(issue.source_sha256, source.sha256);
    }
}

#[test]
fn jsonb_numeric_storage_failure_reaches_blocked_manifest() {
    let fixture = Fixture::new();
    let family = families::FAMILIES.iter().find(|f| f.name == "types").unwrap();
    let path = fixture.root.join(family.resource);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, b"client fixture").unwrap();
    let source = input::read_input(&fixture.root, family.name, family.resource).unwrap();
    let key = ReportKey::integer("42").unwrap();
    let good = crate::report::diff::parse_json(r#"{"_key":42,"nested":[1e-9999]}"#).unwrap();
    assert!(check_native_storage(family, &key, &good).is_ok());
    for token in ["1e131072", "1e-16384", "1e-20000"] {
        let row = crate::report::diff::parse_json(&format!(r#"{{"_key":42,"nested":[{token}]}}"#)).unwrap();
        let error = check_native_storage(family, &key, &row).unwrap_err();
        let mut manifest = crate::initial_manifest(crate::report::manifest::new_run_id());
        let mut reports = Reports::default();
        crate::blocked_manifest(&mut manifest, &[source.clone()], &mut reports, &fixture.run, &error);
        assert_eq!(manifest.summary.blocked_families, 16);
        assert_eq!(manifest.summary.enabled_families, 0);
        assert!(manifest.families.iter().all(|f| f.counts.output_native_records == 0 && f.counts.output_compat_records == 0));
        let issue = &reports.issues[0];
        assert_eq!(issue.family, "types");
        assert_eq!(issue.key, Some(key.clone()));
        assert_eq!(issue.path, "/nested/0");
        assert_eq!(issue.source_relative_path, source.relative_path);
        assert_eq!(issue.source_sha256, source.sha256);
    }
}
