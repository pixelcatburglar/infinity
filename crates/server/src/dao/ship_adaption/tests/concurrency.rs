use super::*;
use crate::dao::{characters::{selection, ship}, items::parent_locks::tests::{backend_pid, wait_blocked}};

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database"]
async fn character_lock_serializes_and_stale_revision_rolls_back() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let character = crate::services::tests::fixture::create(&db).await.unwrap();
    let version = format!("adaption_lock_test_{}", character.character);
    let bytes = vec![0u8; 32];
    db.execute("INSERT INTO game.lua_content_versions (namespace,version,schema_version,compiler_identity,registration,source,data,fingerprint) VALUES ('ship_adaption',$1,1,'test',$2,$2,$2,$3)", &[&version, &Vec::<u8>::new(), &bytes]).await.unwrap();
    let cfg = crate::config::tests::test_database();
    let (mut holder, connection) = tokio_postgres::connect(&cfg.url, tokio_postgres::NoTls).await.unwrap();
    let holder_driver = tokio::spawn(async move { connection.await.unwrap() });
    let tx = holder.transaction().await.unwrap();
    selection::lock_character(&tx, character.character).await.unwrap();
    assert!(ship::current(&tx, character.character).await.unwrap().is_some());
    let pid = backend_pid(&tx).await;
    assert_eq!(lock_subject(&tx, character.character, &version).await.unwrap().revision, 0);
    assert!(chips::credit(&tx, character.character, 1, 2).await.unwrap());
    assert!(advance(&tx, character.character, 0, &version).await.unwrap());
    let other_version = version.clone();
    let waiter = tokio::spawn(async move {
        let (mut conn, driver) = tokio_postgres::connect(&cfg.url, tokio_postgres::NoTls).await.unwrap();
        let driver = tokio::spawn(async move { driver.await.unwrap() });
        let tx = conn.transaction().await.unwrap();
        selection::lock_character(&tx, character.character).await.unwrap();
        assert!(ship::current(&tx, character.character).await.unwrap().is_some());
        assert_eq!(lock_subject(&tx, character.character, &other_version).await.unwrap().revision, 1);
        assert!(chips::credit(&tx, character.character, 1, 1).await.unwrap());
        assert!(!advance(&tx, character.character, 0, &other_version).await.unwrap());
        tx.rollback().await.unwrap();
        drop(conn);
        driver.await.unwrap();
    });
    wait_blocked(&db, pid).await;
    tx.commit().await.unwrap();
    waiter.await.unwrap();
    let result = snapshot(&db, character.character).await.unwrap();
    assert_eq!(result.revision(), 1);
    assert_eq!(result.chips, vec![chips::Chip { type_id: 1, balance: 2 }]);
    db.execute("DELETE FROM game.ship_adaption_chips WHERE character_id=$1", &[&character.character]).await.unwrap();
    db.execute("DELETE FROM game.ship_adaption_characters WHERE character_id=$1", &[&character.character]).await.unwrap();
    crate::services::tests::fixture::cleanup(&db, character).await.unwrap();
    drop(holder);
    holder_driver.await.unwrap();
}
