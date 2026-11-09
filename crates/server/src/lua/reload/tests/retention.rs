//! Database-backed authored package retention and generation admission.
use super::super::*;
use crate::{lua::{content::{PinnedPolicy, PolicyInput, PolicyPlan, Rule}, host::Resources}, sde::Sde, space::World};

#[derive(Debug, PartialEq, Eq)]
struct HistoryRow {
    ordinal: i64,
    version: String,
    fingerprint: Vec<u8>,
}

async fn history(db: &crate::db::Db, namespace: &str) -> Vec<HistoryRow> {
    db.query("SELECT ordinal, version, fingerprint FROM game.lua_content_activations WHERE namespace = $1 ORDER BY ordinal", &[&namespace]).await.unwrap().iter().map(|row| HistoryRow {
        ordinal: row.get("ordinal"),
        version: row.get("version"),
        fingerprint: row.get("fingerprint"),
    }).collect()
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn retained_packages_and_admission_follow_publication() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let id = crate::handshake::random_u64();
    let v1 = format!("retention-{id}-1");
    let v2 = format!("retention-{id}-2");
    let root = std::env::temp_dir().join(format!("lua-retention-{id}"));
    let v3 = format!("retention-{id}-3");
    let trigger = format!("retention_reject_{id}");
    let result = crate::tests::catch_scenario(async {
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let world = World::new(db.clone(), sde.clone(), 1);
    let hub = crate::chat::Hub::new("test", db.clone(), world.clone());
    let pilots = crate::lua::host::pilots::Pilots::new(&db, hub).await.unwrap();
    let resources = Resources { db: db.clone(), sde, world, pilots, store_value_limit_bytes: 65536 };
    std::fs::create_dir_all(root.join("content/sovereignty/data")).unwrap();
    let registration = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/lua/sovereignty_content.lua"));
    let source = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/lua/content/sovereignty/policy.lua"));
    let data = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/lua/content/sovereignty/data/policy.json"));
    let write_version = |version: &str, seconds: &str| {
        std::fs::write(root.join("sovereignty_content.lua"), registration.replace("content_version = \"1\"", &format!("content_version = \"{version}\""))).unwrap();
        std::fs::write(root.join("content/sovereignty/policy.lua"), source).unwrap();
        std::fs::write(root.join("content/sovereignty/data/policy.json"), data.replace("\"version\": \"1\"", &format!("\"version\": \"{version}\"")).replace("\"self_destruct_seconds\": 1200", &format!("\"self_destruct_seconds\": {seconds}"))).unwrap();
    };
    write_version(&v1, "1200");
    let config = LuaConfig { scripts_dir: root.to_string_lossy().into_owned(), ..LuaConfig::default() };
    let host = Host::new(config.clone(), resources.clone()).await.unwrap();
    let old = PinnedPolicy::pin(host.snapshot(), "sovereignty").unwrap();
    let old_row = crate::dao::lua_content::load(&db, "sovereignty", &v1).await.unwrap().unwrap();
    let initial = history(&db, "sovereignty").await;
    assert_eq!(initial.last().unwrap().version, v1);
    assert_eq!(initial.last().unwrap().fingerprint, old_row.fingerprint);
    assert_eq!(old_row.registration, registration.replace("content_version = \"1\"", &format!("content_version = \"{v1}\"")).as_bytes());
    assert_eq!(old_row.source, source.as_bytes());
    assert_eq!(old_row.data, data.replace("\"version\": \"1\"", &format!("\"version\": \"{v1}\"")).as_bytes());
    let admitted = host.admit(&old).unwrap();
    let input = PolicyInput { now: 100, system: 42, current_system: None, first_appointment: None, last_loss: None, not_before: None, owned_count: 1 };
    let old_result = admitted.policy().evaluate(Rule::SelfDestruct, &input).unwrap();
    let barrier = tokio::sync::Barrier::new(2);
    let (admitted, ()) = tokio::join!(
        async { let token = host.admit(&old).unwrap(); barrier.wait().await; token },
        async { barrier.wait().await; host.reload().await.unwrap(); }
    );
    assert_eq!(history(&db, "sovereignty").await, initial);
    assert!(host.admit(&old).is_err());
    assert_eq!(admitted.policy().evaluate(Rule::SelfDestruct, &input).unwrap(), old_result);
    assert!(matches!(&old_result, PolicyPlan::SelfDestruct { .. }));
    let before = host.snapshot();
    write_version(&v1, "1201");
    assert!(host.reload().await.is_err());
    assert!(Arc::ptr_eq(&before, &host.snapshot()));
    write_version(&v2, "1201");
    std::fs::write(root.join("content/sovereignty/policy.lua"), format!("{source}\n-- Different authored revision\n")).unwrap();
    let before_b = history(&db, "sovereignty").await;
    host.reload().await.unwrap();
    let after_b = history(&db, "sovereignty").await;
    assert_eq!(after_b.len(), before_b.len() + 1);
    assert_eq!(after_b.last().unwrap().version, v2);
    let current = PinnedPolicy::pin(host.snapshot(), "sovereignty").unwrap();
    let barrier = tokio::sync::Barrier::new(2);
    let ((), rejected) = tokio::join!(
        async { host.reload().await.unwrap(); barrier.wait().await; },
        async { barrier.wait().await; host.admit(&current).is_err() }
    );
    assert!(rejected);
    let before_failure = host.snapshot();
    let before_failure_policy = PinnedPolicy::pin(before_failure.clone(), "sovereignty").unwrap();
    write_version(&v3, "1202");
    let candidate_v3 = PinnedPolicy::pin(Arc::new(vm::build(&config, &resources).unwrap()), "sovereignty").unwrap();
    db.execute(&format!("CREATE FUNCTION game.{trigger}() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.namespace = 'sovereignty' AND NEW.version = '{v3}' THEN RAISE EXCEPTION 'test activation append failure'; END IF; RETURN NEW; END $$"), &[]).await.unwrap();
    db.execute(&format!("CREATE TRIGGER {trigger} BEFORE INSERT ON game.lua_content_activations FOR EACH ROW EXECUTE FUNCTION game.{trigger}()"), &[]).await.unwrap();
    let error = host.reload().await.unwrap_err();
    match error.downcast_ref::<crate::db::Error>() {
        Some(crate::db::Error::Postgres(error)) => assert_eq!(error.as_db_error().unwrap().message(), "test activation append failure"),
        _ => panic!("expected activation append database failure: {error:#}"),
    }
    assert_eq!(history(&db, "sovereignty").await, after_b);
    assert!(crate::dao::lua_content::load(&db, "sovereignty", &v3).await.unwrap().is_none());
    assert!(Arc::ptr_eq(&before_failure, &host.snapshot()));
    assert!(host.admit(&before_failure_policy).is_ok());
    assert_eq!(host.admit(&before_failure_policy).unwrap().policy().evaluate(Rule::SelfDestruct, &input).unwrap(), current.evaluate(Rule::SelfDestruct, &input).unwrap());
    assert!(host.admit(&candidate_v3).is_err());
    db.execute(&format!("DROP TRIGGER {trigger} ON game.lua_content_activations"), &[]).await.unwrap();
    write_version(&v1, "1200");
    let candidate = vm::build(&config, &resources).unwrap();
    persist(&resources, &candidate).await.unwrap();
    let after_a = history(&db, "sovereignty").await;
    assert_eq!(after_a.len(), after_b.len() + 1);
    assert_eq!(after_a.last().unwrap().version, v1);
    assert!(after_a.last().unwrap().ordinal > after_b.last().unwrap().ordinal);
    assert_eq!(after_a.last().unwrap().fingerprint, old_row.fingerprint);
    assert_ne!(current.evaluate(Rule::SelfDestruct, &input).unwrap(), old_result);
    drop(host);
    let restarted = Host::new(config, resources).await.unwrap();
    assert_eq!(history(&db, "sovereignty").await, after_a);
    let retained = restarted.retained("sovereignty", &v1).await.unwrap();
    assert_eq!(retained.evaluate(Rule::SelfDestruct, &input).unwrap(), old_result);
    assert_eq!(retained.namespace(), "sovereignty");
    assert_eq!(retained.version(), v1);
    let retained_row = crate::dao::lua_content::load(&db, "sovereignty", &v1).await.unwrap().unwrap();
    assert_eq!(retained_row.registration, old_row.registration);
    assert_eq!(retained_row.source, old_row.source);
    assert_eq!(retained_row.data, old_row.data);
    assert!(restarted.admit(&retained).is_err());
    assert!(restarted.retained("sovereignty", "missing").await.is_err());
    for (column, value) in [("fingerprint", "decode(repeat('00',32),'hex')"), ("schema_version", "2"), ("compiler_identity", "'other'")] {
        let row = crate::dao::lua_content::load(&db, "sovereignty", &v1).await.unwrap().unwrap();
        db.execute(&format!("UPDATE game.lua_content_versions SET {column} = {value} WHERE namespace = $1 AND version = $2"), &[&"sovereignty", &v1]).await.unwrap();
        assert!(restarted.retained("sovereignty", &v1).await.is_err());
        db.execute("UPDATE game.lua_content_versions SET fingerprint = $3, schema_version = $4, compiler_identity = $5 WHERE namespace = $1 AND version = $2", &[&"sovereignty", &v1, &row.fingerprint, &row.schema_version, &row.compiler_identity]).await.unwrap();
    }
    }).await;
    let mut cleanup_errors = Vec::new();
    for statement in [
        format!("DROP TRIGGER IF EXISTS {trigger} ON game.lua_content_activations"),
        format!("DROP FUNCTION IF EXISTS game.{trigger}()"),
    ] {
        if let Err(error) = db.execute(&statement, &[]).await {
            cleanup_errors.push(format!("{statement}: {error}"));
        }
    }
    if let Err(error) = std::fs::remove_dir_all(&root) {
        if error.kind() != std::io::ErrorKind::NotFound {
            cleanup_errors.push(format!("remove {}: {error}", root.display()));
        }
    }
    if let Err(panic) = result {
        if !cleanup_errors.is_empty() { eprintln!("retention cleanup failed: {}", cleanup_errors.join("; ")); }
        std::panic::resume_unwind(panic);
    }
    assert!(cleanup_errors.is_empty(), "retention cleanup failed: {}", cleanup_errors.join("; "));
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn publication_records_each_namespace_independently() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let world = World::new(db.clone(), sde.clone(), 1);
    let fixture = crate::lua::tests::ship_adaption::fixture::create(&db, sde, world).await;
    let adaptation = history(&db, "ship_adaption").await;
    assert!(!adaptation.is_empty());
    let id = crate::handshake::random_u64();
    let version = format!("activation-multiple-{id}");
    let scripts = fixture.root.join("scripts");
    std::fs::create_dir_all(scripts.join("content/sovereignty/data")).unwrap();
    let registration = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/lua/sovereignty_content.lua"));
    let source = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/lua/content/sovereignty/policy.lua"));
    let data = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/lua/content/sovereignty/data/policy.json"));
    std::fs::write(scripts.join("sovereignty_content.lua"), registration.replace("content_version = \"1\"", &format!("content_version = \"{version}\""))).unwrap();
    std::fs::write(scripts.join("content/sovereignty/policy.lua"), source).unwrap();
    std::fs::write(scripts.join("content/sovereignty/data/policy.json"), data.replace("\"version\": \"1\"", &format!("\"version\": \"{version}\""))).unwrap();
    let before = history(&db, "sovereignty").await;
    fixture.host.reload().await.unwrap();
    let after = history(&db, "sovereignty").await;
    assert_eq!(after.len(), before.len() + 1);
    assert_eq!(after.last().unwrap().version, version);
    assert_eq!(history(&db, "ship_adaption").await, adaptation);
}
