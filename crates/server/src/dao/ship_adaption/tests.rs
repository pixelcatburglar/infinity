use super::*;
mod concurrency;
mod schema;

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database"]
async fn ledger_guards_balances_revision_and_rollback() {
    let (mut client, connection) = tokio_postgres::connect(&crate::config::tests::test_database().url, tokio_postgres::NoTls).await.unwrap();
    let driver = tokio::spawn(async move { connection.await.unwrap() });
    let tx = client.transaction().await.unwrap();
    let name = format!("adaption_test_{}", std::process::id());
    let user: i64 = tx.query_one("INSERT INTO game.accounts (user_name,password_hash,role) VALUES ($1,$2,0) RETURNING user_id", &[&name, &Vec::<u8>::new()]).await.unwrap().get("user_id");
    let character: i64 = tx.query_one("INSERT INTO game.characters (user_id,name,race_id,bloodline_id,gender_id,corporation_id) VALUES ($1,$2,1,1,1,1) RETURNING character_id", &[&user, &name]).await.unwrap().get("character_id");
    let bytes = vec![0u8; 32];
    tx.execute("INSERT INTO game.lua_content_versions (namespace,version,schema_version,compiler_identity,registration,source,data,fingerprint) VALUES ('ship_adaption',$1,1,'test',$2,$2,$2,$3)", &[&name, &Vec::<u8>::new(), &bytes]).await.unwrap();
    let empty = snapshot(&tx, character).await.unwrap();
    assert_eq!(empty.revision(), 0);
    assert!(empty.subject.is_none() && empty.chips.is_empty());
    assert_eq!(lock_subject(&tx, character, &name).await.unwrap().revision, 0);
    assert!(!chips::debit(&tx, character, 1, 1).await.unwrap());
    assert!(chips::credit(&tx, character, 3, 0).await.unwrap());
    assert!(chips::credit(&tx, character, 1, u32::MAX).await.unwrap());
    assert!(!chips::credit(&tx, character, 1, 1).await.unwrap());
    assert!(chips::debit(&tx, character, 1, u32::MAX).await.unwrap());
    assert!(!chips::debit(&tx, character, 1, 1).await.unwrap());
    assert!(chips::credit(&tx, character, 2, 1).await.unwrap());
    assert!(advance(&tx, character, 0, &name).await.unwrap());
    assert!(!advance(&tx, character, 0, &name).await.unwrap());
    let current = snapshot(&tx, character).await.unwrap();
    assert_eq!(current.revision(), 1);
    assert_eq!(current.chips, vec![chips::Chip { type_id: 1, balance: 0 }, chips::Chip { type_id: 2, balance: 1 }, chips::Chip { type_id: 3, balance: 0 }]);
    tx.rollback().await.unwrap();
    assert!(client.query_opt("SELECT character_id FROM game.ship_adaption_characters WHERE character_id = $1", &[&character]).await.unwrap().is_none());
    drop(client);
    driver.await.unwrap();
}
