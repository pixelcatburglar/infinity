use super::*;

#[test]
fn sanitize_replaces_non_finite_outside_strings() {
    assert_eq!(sanitize(r#"{"a": NaN, "b": -Infinity, "c": Infinity}"#), r#"{"a": null, "b": null, "c": null}"#);
    assert_eq!(sanitize(r#"{"NaN": "Infinity NaN"}"#), r#"{"NaN": "Infinity NaN"}"#);
}

#[test]
fn sanitize_drops_nul_escapes_only() {
    assert_eq!(sanitize(r#"{"a": "x\u0000y", "b": "\\u0000"}"#), r#"{"a": "xy", "b": "\\u0000"}"#);
}

#[test]
fn table_names_are_safe() {
    assert_eq!(table_name(Path::new("mapObjects__celestials.jsonl")), "mapobjects__celestials");
    assert_eq!(table_name(Path::new("localization_fsd_en-us.jsonl")), "localization_fsd_en_us");
    assert_eq!(table_name(Path::new("_sde.jsonl")), "_sde");
}
#[test]
fn filesystem_failure_reports_discovery_and_partial_progress() {
    let root = std::env::temp_dir().join(format!("sde-report-progress-{}", std::process::id()));
    let client_dir = root.join("client");
    for family in client::families::FAMILIES {
        let path = client_dir.join(family.resource);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, b"truncated").unwrap();
    }
    let mut inputs = Vec::new();
    let mut manifest = initial_manifest(report::manifest::new_run_id());
    let run = report::manifest::ReportRun::begin(&root.join("report"), &manifest).unwrap();
    let mut reports = report::manifest::Reports::default();
    let error = client::prepare::decode_inputs(&client_dir, &mut reports, &mut inputs, &run).unwrap_err();
    assert_eq!(inputs.len(), client::families::FAMILIES.len());
    assert!(inputs.iter().all(|input| input.decoded_records.is_none() && !input.sha256.is_empty()));
    assert_eq!(error.downcast_ref::<client::prepare::FamilyContext>().unwrap().family, "types");

    // Exercise a later failure without constructing sixteen valid binary resources.
    inputs[0].decoded_records = Some(42);
    let failed = &client::families::FAMILIES[1];
    let error = anyhow::anyhow!("malformed fixture").context(client::prepare::FamilyContext {
        family: failed.name.into(), resource: failed.resource.into(), stage: "decode", path: None,
    });
    manifest.inputs = input_identities(&inputs);
    blocked_manifest(&mut manifest, &inputs, &mut reports, &run, &error);
    assert_eq!(manifest.summary.blocked_families, 16);
    assert_eq!(manifest.summary.enabled_families, 0);
    assert_eq!(manifest.families.len(), 16);
    assert!(manifest.families.iter().all(|f| f.status == "blocked" && f.counts.output_native_records == 0 && f.counts.output_compat_records == 0));
    assert_eq!(manifest.families[0].counts.input_records, 42);
    assert!(manifest.families[0].reason.as_ref().unwrap().contains("dependency closure"));
    assert!(manifest.families[1].reason.as_ref().unwrap().contains("decode failed"));
    assert_eq!(reports.issues[0].family, failed.name);
    assert_eq!(reports.issues[0].source_sha256, inputs[1].sha256);
    assert_eq!(manifest.inputs[1].sha256, inputs[1].sha256);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn registered_families_forbid_old_jobs_in_both_schemas() {
    assert_eq!(client::families::FAMILIES.len(), 16);
    assert_eq!(client::families::FAMILIES.iter().filter(|f| f.compat_table.is_some()).count(), 8);
    for family in client::families::FAMILIES {
        assert!(client::families::is_migrated("sde_native", family.name));
        assert!(client::families::is_migrated("sde_compat", family.name));
        assert_eq!(family.required_families.len(), 16);
    }
}


async fn static_snapshot(client: &Client) -> Vec<(String, Vec<String>)> {
    let tables = client.query("SELECT n.nspname AS schema_name, c.relname AS table_name, c.oid::text AS table_oid FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname IN ('sde_native','sde_compat','sde') AND c.relkind IN ('r','p') ORDER BY schema_name,table_name", &[]).await.unwrap();
    let mut snapshot = Vec::new();
    let schema_sql = "SELECT definition FROM (
        SELECT 'namespace:' || n.oid::text || ':' || n.nspname AS definition FROM pg_namespace n WHERE n.nspname IN ('sde_native','sde_compat','sde')
        UNION ALL SELECT 'relation:' || c.oid::text || ':' || n.nspname || ':' || c.relname || ':' || c.relkind::text FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname IN ('sde_native','sde_compat','sde')
        UNION ALL SELECT 'index:' || i.indexrelid::text || ':' || pg_get_indexdef(i.indexrelid) FROM pg_index i JOIN pg_class c ON c.oid=i.indexrelid JOIN pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname IN ('sde_native','sde_compat','sde')
        UNION ALL SELECT 'constraint:' || x.oid::text || ':' || pg_get_constraintdef(x.oid) FROM pg_constraint x JOIN pg_namespace n ON n.oid=x.connamespace WHERE n.nspname IN ('sde_native','sde_compat','sde')
        UNION ALL SELECT 'function:' || p.oid::text || ':' || pg_get_functiondef(p.oid) FROM pg_proc p JOIN pg_namespace n ON n.oid=p.pronamespace WHERE n.nspname IN ('sde_native','sde_compat','sde')
    ) definitions ORDER BY definition";
    snapshot.push(("schema objects".into(), client.query(schema_sql, &[]).await.unwrap().iter().map(|row| row.get("definition")).collect()));
    for table in tables {
        let schema: String = table.get("schema_name");
        let name: String = table.get("table_name");
        let oid: String = table.get("table_oid");
        let columns = client.query("SELECT attnum::text || ':' || attname || ':' || atttypid::text || ':' || atttypmod::text || ':' || attnotnull::text AS column_definition FROM pg_attribute WHERE attrelid=$1::text::oid AND attnum > 0 ORDER BY attnum", &[&oid]).await.unwrap()
            .iter().map(|r| r.get::<_, String>("column_definition")).collect::<Vec<_>>();
        let quote = |s: &str| format!("\"{}\"", s.replace('"', "\"\""));
        let sql = format!("SELECT md5(row_to_json(t)::text) AS row_hash FROM {}.{} t ORDER BY row_hash", quote(&schema), quote(&name));
        let mut values = columns;
        values.extend(client.query(&sql, &[]).await.unwrap().iter().map(|r| r.get::<_, String>("row_hash")));
        snapshot.push((format!("{schema}.{name}:{oid}"), values));
    }
    snapshot
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL"]
async fn malformed_input_preserves_database_sentinel() {
    let url = std::env::var("INFINITY_TEST_DATABASE_URL").expect("dedicated test database URL");
    let client = connect(&url).await.unwrap();
    let schema = format!("sde_import_sentinel_{}", std::process::id());
    client.batch_execute(&format!("CREATE SCHEMA \"{schema}\"; CREATE TABLE \"{schema}\".sentinel (value integer); INSERT INTO \"{schema}\".sentinel VALUES (42)")).await.unwrap();
    let before = static_snapshot(&client).await;
    let root = std::env::temp_dir().join(format!("sde-import-negative-{}", std::process::id()));
    std::fs::create_dir_all(root.join("client/StaticData")).unwrap();
    std::fs::create_dir_all(root.join("client/staticdata")).unwrap();
    let args = |report_dir| Args { sde: root.join("sde"), client_dir: root.join("client"), report_dir,
        typed_sql: root.join("typed.sql"), jobs: 1, db_url: url.clone() };
    assert!(run_import(args(root.join("report-ambiguous")), report::manifest::new_run_id()).await.is_err());
    assert_eq!(before, static_snapshot(&client).await);
    std::fs::remove_dir(root.join("client/StaticData")).unwrap();
    std::fs::write(root.join("client/staticdata/types.fsdbinary"), b"truncated").unwrap();
    assert!(run_import(args(root.join("report-malformed")), report::manifest::new_run_id()).await.is_err());
    assert_eq!(before, static_snapshot(&client).await);
    let mut unsupported = vec![0u8; 56];
    unsupported[24..32].copy_from_slice(&24u64.to_le_bytes());
    unsupported[32..40].copy_from_slice(&24u64.to_le_bytes());
    std::fs::write(root.join("client/staticdata/types.fsdbinary"), unsupported).unwrap();
    assert!(run_import(args(root.join("report-unsupported")), report::manifest::new_run_id()).await.is_err());
    assert_eq!(before, static_snapshot(&client).await);
    std::fs::write(root.join("not-a-directory"), b"x").unwrap();
    assert!(run_import(args(root.join("not-a-directory/report")), report::manifest::new_run_id()).await.is_err());
    assert_eq!(before, static_snapshot(&client).await);
    let value: i32 = client.query_one(&format!("SELECT value AS sentinel_value FROM \"{schema}\".sentinel"), &[]).await.unwrap().get("sentinel_value");
    assert_eq!(value, 42);
    client.batch_execute(&format!("DROP SCHEMA \"{schema}\" CASCADE")).await.unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL"]
async fn copy_uses_owned_bytes_after_input_replacement() {
    let url = std::env::var("INFINITY_TEST_DATABASE_URL").expect("dedicated test database URL");
    let mut client = connect(&url).await.unwrap();
    let root = std::env::temp_dir().join(format!("sde-import-owned-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("rows.jsonl"), b"{\"_key\":1,\"value\":\"original\"}\n").unwrap();
    let input = client::input::read_input(&root, "rows", "rows.jsonl").unwrap();
    let job = Job { schema: "sde_import_immutable_test", table: "rows".into(), source: JobSource::PreparedBytes(input.bytes) };
    std::fs::write(root.join("rows.jsonl"), b"{\"_key\":1,\"value\":\"replacement\"}\n").unwrap();
    client.batch_execute("CREATE SCHEMA sde_import_immutable_test").await.unwrap();
    load_table(&mut client, &job).await.unwrap();
    let value: String = client.query_one("SELECT data->>'value' AS stored_value FROM sde_import_immutable_test.rows", &[]).await.unwrap().get("stored_value");
    assert_eq!(value, "original");
    client.batch_execute("DROP SCHEMA sde_import_immutable_test CASCADE").await.unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external import inputs"]
async fn persistent_reference_preflight_preserves_all_static_rows() {
    use futures_util::FutureExt;
    use std::panic::AssertUnwindSafe;
    let url = std::env::var("INFINITY_TEST_DATABASE_URL").expect("dedicated database URL");
    let root = std::env::temp_dir().join(format!("sde-import-preflight-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let args = Args {
        client_dir: std::env::var_os("INFINITY_TEST_CLIENT_DIR").expect("client input").into(),
        sde: std::env::var_os("INFINITY_TEST_SDE_DIR").expect("snapshot input").into(),
        typed_sql: std::env::var_os("INFINITY_TEST_TYPED_SQL").expect("typed SQL input").into(),
        report_dir: root.join("report"), jobs: 1, db_url: url.clone(),
    };
    let client = connect(&url).await.unwrap();
    let id = i32::MAX;
    let inserted = client.execute("INSERT INTO sde_native.types (row_no,key,data) SELECT (SELECT COALESCE(max(row_no),0)+1 FROM sde_native.types), $1::int::text, jsonb_build_object('_key',$1::int,'typeID',$1::int,'groupID',(SELECT (data->>'groupID')::int FROM sde_native.types LIMIT 1)) WHERE NOT EXISTS (SELECT 1 FROM sde_native.types WHERE key=$1::int::text)", &[&id]).await.unwrap();
    assert_eq!(inserted, 1, "synthetic ID already exists");
    let outcome = AssertUnwindSafe(async {
        let before = static_snapshot(&client).await;
        assert!(run_import(args, report::manifest::new_run_id()).await.is_err());
        assert_eq!(before, static_snapshot(&client).await);
        let manifest: serde_json::Value = serde_json::from_slice(&std::fs::read(root.join("report/manifest.json")).unwrap()).unwrap();
        assert_eq!(manifest["phase"], "database-preflight");
        assert_eq!(manifest["status"], "preflight-failed");
        let entries = std::fs::read_dir(root.join("report")).unwrap();
        let run = entries.map(|entry| entry.unwrap().path()).find(|path| path.is_dir()).unwrap();
        let issues = std::fs::read_to_string(run.join("issues.jsonl")).unwrap();
        assert!(issues.contains("persistent-reference"));
    }).catch_unwind().await;
    client.execute("DELETE FROM sde_native.types WHERE key=$1::int::text AND data->>'typeID'=$1::int::text", &[&id]).await.unwrap();
    std::fs::remove_dir_all(root).unwrap();
    if let Err(panic) = outcome { std::panic::resume_unwind(panic); }
}

#[test]
fn binary_cli_prints_one_run_identity_before_failures() {
    let Some(binary) = std::env::var_os("INFINITY_TEST_IMPORT_BINARY") else { return };
    fn identity(output: &std::process::Output) -> String {
        assert!(!output.status.success());
        let stderr = String::from_utf8(output.stderr.clone()).unwrap();
        let ids: Vec<_> = stderr.lines().filter_map(|line| line.strip_prefix("sde-import run_id=")).collect();
        assert_eq!(ids.len(), 1, "{stderr}");
        assert!(!ids[0].is_empty());
        assert!(stderr.lines().next().unwrap().starts_with("sde-import run_id="));
        ids[0].to_owned()
    }
    let root = std::env::temp_dir().join(format!("sde-import-cli-{}", std::process::id()));
    std::fs::create_dir_all(root.join("client/StaticData")).unwrap();
    std::fs::create_dir_all(root.join("client/staticdata")).unwrap();
    let parse = std::process::Command::new(&binary).arg("--unknown").arg("value").output().unwrap();
    identity(&parse);
    let invoke = |report: &Path| {
        std::process::Command::new(&binary).env("DATABASE_URL", "host=127.0.0.1 port=1 connect_timeout=1")
            .arg("--sde").arg(root.join("sde")).arg("--client-dir").arg(root.join("client"))
            .arg("--typed-sql").arg(root.join("typed.sql")).arg("--report-dir").arg(report).output().unwrap()
    };
    std::fs::write(root.join("not-directory"), b"x").unwrap();
    let marker = invoke(&root.join("not-directory/report"));
    identity(&marker);
    assert!(String::from_utf8_lossy(&marker.stderr).contains("database must not connect"));
    let ambiguous = invoke(&root.join("report"));
    let run_id = identity(&ambiguous);
    let manifest: serde_json::Value = serde_json::from_slice(&std::fs::read(root.join("report/manifest.json")).unwrap()).unwrap();
    assert_eq!(manifest["run_metadata"]["run_id"], run_id);
    assert_eq!(manifest["status"], "preflight-failed");
    assert_eq!(manifest["phase"], "filesystem-preflight");
    std::fs::remove_dir_all(root).unwrap();
}
