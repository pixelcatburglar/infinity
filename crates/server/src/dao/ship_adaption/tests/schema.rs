#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database"]
async fn actual_schema_block_is_idempotent_and_preserves_constraints() {
    let (mut client, connection) = tokio_postgres::connect(&crate::config::tests::test_database().url, tokio_postgres::NoTls).await.unwrap();
    let driver = tokio::spawn(async move { connection.await.unwrap() });
    let tx = client.transaction().await.unwrap();
    tx.batch_execute("CREATE SCHEMA adaption_schema_test; CREATE TABLE adaption_schema_test.characters (character_id bigint PRIMARY KEY); CREATE TABLE adaption_schema_test.lua_content_versions (namespace text NOT NULL, version text NOT NULL, PRIMARY KEY(namespace, version));").await.unwrap();
    let schema = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/sql/game_schema.sql"));
    let block = schema.split_once("-- Persistent adaptation chip ledger (dao/ship_adaption).").unwrap().1.split_once("-- Sovereignty Hub persistence").unwrap().0.replace("game.", "adaption_schema_test.");
    tx.batch_execute(&block).await.unwrap();
    tx.batch_execute("INSERT INTO adaption_schema_test.characters VALUES (1); INSERT INTO adaption_schema_test.lua_content_versions VALUES ('ship_adaption','test'); INSERT INTO adaption_schema_test.ship_adaption_characters VALUES (1, 7, 'ship_adaption', 'test'); INSERT INTO adaption_schema_test.ship_adaption_chips VALUES (1, 1, 0), (1, 2, 4294967295);").await.unwrap();
    tx.batch_execute(&block).await.unwrap();
    tx.batch_execute(&block).await.unwrap();
    let row = tx.query_one("SELECT revision, content_version FROM adaption_schema_test.ship_adaption_characters WHERE character_id=1", &[]).await.unwrap();
    assert_eq!(row.get::<_, i64>("revision"), 7);
    assert_eq!(row.get::<_, String>("content_version"), "test");
    let rows = tx.query("SELECT type_id, balance FROM adaption_schema_test.ship_adaption_chips ORDER BY type_id", &[]).await.unwrap();
    assert_eq!(rows.iter().map(|r| (r.get::<_, i32>("type_id"), r.get::<_, i64>("balance"))).collect::<Vec<_>>(), vec![(1, 0), (2, i64::from(u32::MAX))]);
    for sql in ["UPDATE adaption_schema_test.ship_adaption_chips SET balance=-1 WHERE type_id=1", "UPDATE adaption_schema_test.ship_adaption_chips SET balance=4294967296 WHERE type_id=1", "UPDATE adaption_schema_test.ship_adaption_characters SET revision=-1", "UPDATE adaption_schema_test.ship_adaption_characters SET content_version='missing'", "INSERT INTO adaption_schema_test.ship_adaption_chips VALUES (2,1,0)"] {
        tx.batch_execute("SAVEPOINT invalid_row").await.unwrap();
        assert!(tx.batch_execute(sql).await.is_err());
        tx.batch_execute("ROLLBACK TO SAVEPOINT invalid_row; RELEASE SAVEPOINT invalid_row").await.unwrap();
    }
    tx.rollback().await.unwrap();
    drop(client);
    driver.await.unwrap();
}
