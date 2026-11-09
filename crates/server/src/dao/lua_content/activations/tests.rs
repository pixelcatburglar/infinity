//! Real database coverage for publication order and transaction rollback.
use super::*;

struct HistoryRow {
    ordinal: i64,
    namespace: String,
    version: String,
    fingerprint: Vec<u8>,
    published_at: i64,
}

async fn history(db: &crate::db::Db, namespace: &str) -> Vec<HistoryRow> {
    db.query("SELECT ordinal, namespace, version, fingerprint, published_at FROM game.lua_content_activations WHERE namespace = $1 ORDER BY ordinal", &[&namespace]).await.unwrap().iter().map(|row| HistoryRow {
        ordinal: row.get("ordinal"),
        namespace: row.get("namespace"),
        version: row.get("version"),
        fingerprint: row.get("fingerprint"),
        published_at: row.get("published_at"),
    }).collect()
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database"]
async fn activation_order_validation_and_atomic_rollback() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let id = crate::handshake::random_u64();
    let package = |namespace: String, version: &str, byte: u8| PackageRow {
        namespace, version: version.to_owned(), schema_version: 1,
        compiler_identity: "test".into(), registration: vec![byte], source: vec![byte],
        data: vec![byte], fingerprint: vec![byte; 32],
    };
    let a = package(format!("activation-test-{id}-a"), "a", 1);
    let b = package(a.namespace.clone(), "b", 2);
    let other = package(format!("activation-test-{id}-b"), "a", 3);
    let rollback = package(format!("activation-test-{id}-rollback"), "a", 4);
    let now = crate::client::filetime_now();
    assert!(append_if_changed(&db, &a, now).await.is_err());
    for row in [&a, &b, &other] { assert!(super::super::store(&db, row).await.unwrap()); }
    assert!(append_if_changed(&db, &a, 0).await.is_err());
    let mut bad = a.clone();
    bad.fingerprint.pop();
    assert!(append_if_changed(&db, &bad, now).await.is_err());
    assert!(append_if_changed(&db, &a, now).await.unwrap());
    assert!(!append_if_changed(&db, &a, now).await.unwrap());
    assert!(append_if_changed(&db, &other, now).await.unwrap());
    assert!(append_if_changed(&db, &b, now).await.unwrap());
    assert!(append_if_changed(&db, &a, now).await.unwrap());
    let rows = history(&db, &a.namespace).await;
    assert_eq!(rows.iter().map(|row| row.version.as_str()).collect::<Vec<_>>(), ["a", "b", "a"]);
    assert!(rows.windows(2).all(|pair| pair[0].ordinal < pair[1].ordinal));
    assert!(rows.iter().all(|row| row.namespace == a.namespace && row.published_at == now));
    assert_eq!(rows[0].fingerprint, a.fingerprint);
    assert_eq!(history(&db, &other.namespace).await.len(), 1);
    let result: Result<(), db::Error> = db.transaction(|tx, _: &mut ()| {
        let row = rollback.clone();
        Box::pin(async move {
            assert!(super::super::store(tx, &row).await?);
            assert!(append_if_changed(tx, &row, now).await?);
            Err(db::Error::Progress(anyhow::anyhow!("rollback test")))
        })
    }, |(), ()| async { Ok(()) }).await;
    assert!(result.is_err());
    assert!(super::super::load(&db, &rollback.namespace, &rollback.version).await.unwrap().is_none());
    assert!(history(&db, &rollback.namespace).await.is_empty());
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database"]
async fn list_decodes_ordered_namespace_history() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let namespace = format!("activation-reader-{}", crate::handshake::random_u64());
    let row = PackageRow {
        namespace: namespace.clone(), version: "a".into(), schema_version: 1,
        compiler_identity: "test".into(), registration: vec![1], source: vec![1],
        data: vec![1], fingerprint: vec![1; 32],
    };
    let now = crate::client::filetime_now();
    assert!(super::super::store(&db, &row).await.unwrap());
    assert!(append_if_changed(&db, &row, now).await.unwrap());
    let mut next = row.clone();
    next.version = "b".into();
    next.fingerprint = vec![2; 32];
    assert!(super::super::store(&db, &next).await.unwrap());
    assert!(append_if_changed(&db, &next, now).await.unwrap());
    let rows = list(&db, &namespace).await.unwrap();
    assert_eq!(rows.len(), 2);
    assert!(rows[0].ordinal > 0);
    assert_eq!(rows[0].namespace, namespace);
    assert_eq!(rows[0].version, row.version);
    assert_eq!(rows[0].fingerprint, row.fingerprint);
    assert_eq!(rows[0].published_at, now);
    assert!(rows[0].ordinal < rows[1].ordinal);
    assert_eq!(rows[1].version, next.version);
}
