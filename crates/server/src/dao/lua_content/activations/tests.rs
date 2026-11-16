//! Real database coverage for publication order, the namespace fence and rollback.
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

fn package(namespace: String, version: &str, byte: u8) -> PackageRow {
    PackageRow {
        namespace, version: version.to_owned(), schema_version: 1,
        compiler_identity: "test".into(), registration: vec![byte], source: vec![byte],
        data: vec![byte], fingerprint: vec![byte; 32],
    }
}

async fn finish(db: &crate::db::Db, namespaces: &[String], result: std::thread::Result<()>) {
    for namespace in namespaces {
        db.execute("DELETE FROM game.lua_content_activations WHERE namespace=$1", &[namespace]).await.unwrap();
        db.execute("DELETE FROM game.lua_content_versions WHERE namespace=$1", &[namespace]).await.unwrap();
    }
    if let Err(panic) = result { std::panic::resume_unwind(panic); }
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database"]
async fn activation_order_validation_and_atomic_rollback() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let id = crate::handshake::random_u64();
    let a = package(format!("activation-test-{id}-a"), "a", 1);
    let b = package(a.namespace.clone(), "b", 2);
    let other = package(format!("activation-test-{id}-b"), "a", 3);
    let rollback = package(format!("activation-test-{id}-rollback"), "a", 4);
    let result = crate::tests::catch_scenario(async {
        let now = crate::client::filetime_now();
        assert!(append_if_changed(&db, &a, now).await.is_err());
        for row in [&a, &b, &other] { assert!(super::super::store(&db, row).await.unwrap()); }
        assert!(publish(&db, &[a.clone()], 0).await.is_err());
        let mut bad = a.clone();
        bad.fingerprint.pop();
        assert!(publish(&db, &[bad], now).await.is_err());
        publish(&db, &[a.clone()], now).await.unwrap();
        publish(&db, &[a.clone()], now).await.unwrap();
        assert_eq!(history(&db, &a.namespace).await.len(), 1);
        publish(&db, &[other.clone(), b.clone()], now).await.unwrap();
        assert_eq!(history(&db, &a.namespace).await.len(), 2);
        let mut decoded: Vec<_> = load_all(&db).await.unwrap().into_iter().filter(|row| row.namespace == a.namespace).collect();
        decoded.sort_by_key(|row| row.ordinal);
        assert_eq!(decoded.len(), 2);
        assert!(decoded[0].ordinal > 0);
        assert_eq!(decoded[0].namespace, a.namespace);
        assert_eq!(decoded[0].version, a.version);
        assert_eq!(decoded[0].fingerprint, a.fingerprint);
        assert_eq!(decoded[0].published_at, now);
        assert!(decoded[0].ordinal < decoded[1].ordinal);
        assert_eq!(decoded[1].version, b.version);
        publish(&db, &[a.clone()], now).await.unwrap();
        let rows = history(&db, &a.namespace).await;
        assert_eq!(rows.len(), 3);
        assert_eq!(rows.iter().map(|row| row.version.as_str()).collect::<Vec<_>>(), ["a", "b", "a"]);
        assert!(rows.windows(2).all(|pair| pair[0].ordinal < pair[1].ordinal));
        assert!(rows.iter().all(|row| row.namespace == a.namespace && row.published_at == now));
        assert_eq!(rows[0].fingerprint, a.fingerprint);
        assert_eq!(history(&db, &other.namespace).await.len(), 1);
        let result: Result<(), db::Error> = db.transaction(|tx, _: &mut ()| {
            let row = rollback.clone();
            Box::pin(async move {
                publish(tx, &[row], now).await?;
                Err(db::Error::Progress(anyhow::anyhow!("rollback test")))
            })
        }, |(), ()| async { Ok(()) }).await;
        assert!(result.is_err());
        assert!(super::super::load(&db, &rollback.namespace, &rollback.version).await.unwrap().is_none());
        assert!(history(&db, &rollback.namespace).await.is_empty());
    }).await;
    finish(&db, &[a.namespace.clone(), other.namespace, rollback.namespace], result).await;
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database"]
async fn namespace_fence_orders_publish_and_prepared_reads() {
    crate::tests::init_config();
    let config = || crate::config::tests::test_database().to_pg_config().unwrap();
    let namespace = format!("fence-test-{}", crate::handshake::random_u64());
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let result = crate::tests::catch_scenario(async {
        let (mut client_a, connection_a) = config().connect(tokio_postgres::NoTls).await.unwrap();
        tokio::spawn(async move { connection_a.await.unwrap(); });
        let (mut client_b, connection_b) = config().connect(tokio_postgres::NoTls).await.unwrap();
        tokio::spawn(async move { connection_b.await.unwrap(); });
        let a = package(namespace.clone(), "a", 1);
        let block = std::time::Duration::from_millis(500);
        // An uncommitted publisher holds the namespace exclusively and blocks shared reads.
        let tx_a = client_a.transaction().await.unwrap();
        let tx_b = client_b.transaction().await.unwrap();
        let before: Vec<_> = load_all(&tx_b).await.unwrap().into_iter().filter(|row| row.namespace == namespace).collect();
        publish(&tx_a, &[a.clone()], 10).await.unwrap();
        assert!(tokio::time::timeout(block, lock_namespace(&tx_b, &namespace, false)).await.is_err());
        tx_a.commit().await.unwrap();
        lock_namespace(&tx_b, &namespace, false).await.unwrap();
        let mut after: Vec<_> = load_all(&tx_b).await.unwrap().into_iter().filter(|row| row.namespace == namespace).collect();
        after.sort_by_key(|row| row.ordinal);
        assert_eq!(after.len(), 1);
        assert_ne!(after, before);
        tx_b.commit().await.unwrap();
        // A held shared lock blocks the exclusive publisher until released.
        let tx_b = client_b.transaction().await.unwrap();
        lock_namespace(&tx_b, &namespace, false).await.unwrap();
        let tx_a = client_a.transaction().await.unwrap();
        assert!(tokio::time::timeout(block, publish(&tx_a, &[package(namespace.clone(), "b", 2)], 20)).await.is_err());
        tx_b.rollback().await.unwrap();
        publish(&tx_a, &[package(namespace.clone(), "b", 2)], 20).await.unwrap();
        tx_a.commit().await.unwrap();
        let count: i64 = client_a.query_one("SELECT count(*) AS count FROM game.lua_content_activations WHERE namespace = $1", &[&namespace]).await.unwrap().get("count");
        assert_eq!(count, 2);
    }).await;
    finish(&db, &[namespace], result).await;
}
