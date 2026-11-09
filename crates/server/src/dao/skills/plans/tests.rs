use super::*;
use crate::services::tests::fixture;

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database"]
async fn legacy_utf16_over_limit_aborts_schema_without_truncation() {
    let mut config = crate::config::tests::test_database();
    config.schema_on_start = false;
    let db = crate::db::Db::connect(&config).await.unwrap();
    let character = fixture::create(&db).await.unwrap();
    let (mut client, connection) = tokio_postgres::connect(&config.url, tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let tx = client.transaction().await.unwrap();
    let id = vec![1u8; 16];
    let text = format!("{}🚀", "中".repeat(49));
    create(&tx, &id, character.character, &text, "unchanged", &[], &[]).await.unwrap();
    let row = tx.query_one("SELECT name, description FROM game.skill_plans WHERE plan_id=$1", &[&id]).await.unwrap();
    assert_eq!(row.get::<_, String>("name"), text);
    assert_eq!(row.get::<_, String>("description"), "unchanged");
    tx.batch_execute("SAVEPOINT before_schema").await.unwrap();
    let error = tx.batch_execute(include_str!("../../../../sql/game_schema.sql")).await.unwrap_err();
    let message = error.as_db_error().unwrap().message();
    assert!(message.contains("invalid skill plan"));
    assert!(message.contains(&"01".repeat(16)));
    assert!(message.contains(&character.character.to_string()));
    tx.batch_execute("ROLLBACK TO SAVEPOINT before_schema").await.unwrap();
    let row = tx.query_one("SELECT name, description FROM game.skill_plans WHERE plan_id=$1", &[&id]).await.unwrap();
    assert_eq!(row.get::<_, String>("name"), text);
    assert_eq!(row.get::<_, String>("description"), "unchanged");
    tx.rollback().await.unwrap();
    assert!(get(&db, &id, Some(character.character)).await.unwrap().is_none());
    fixture::cleanup(&db, character).await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn legacy_multiple_active_plans_abort_migration_without_data_loss() {
    let mut config = crate::config::tests::test_database();
    config.schema_on_start = false;
    let db = crate::db::Db::connect(&config).await.unwrap();
    let character = fixture::create(&db).await.unwrap();
    let (mut client, connection) = tokio_postgres::connect(&config.url, tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let tx = client.transaction().await.unwrap();
    tx.batch_execute("ALTER TABLE game.skill_plans ADD COLUMN active boolean NOT NULL DEFAULT false").await.unwrap();
    let first = vec![1u8; 16]; let second = vec![2u8; 16];
    create(&tx, &first, character.character, "first", "", &[], &[]).await.unwrap();
    create(&tx, &second, character.character, "second", "", &[], &[]).await.unwrap();
    tx.execute("UPDATE game.skill_plans SET active=true WHERE character_id=$1", &[&character.character]).await.unwrap();
    let error = tx.batch_execute(include_str!("../../../../sql/game_schema.sql")).await.unwrap_err();
    assert!(error.to_string().contains("multiple active skill plans") || error.as_db_error().is_some_and(|e| e.message().contains("multiple active skill plans")));
    tx.rollback().await.unwrap();
    assert!(all(&db, character.character).await.unwrap().is_empty());
    fixture::cleanup(&db, character).await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn unknown_legacy_skill_is_audit_warning_not_startup_failure() {
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = crate::sde::Sde::load(&db).await.unwrap();
    let character = fixture::create(&db).await.unwrap();
    let id = [crate::handshake::random_u64().to_be_bytes(), crate::handshake::random_u64().to_be_bytes()].concat();
    create(&db, &id, character.character, "legacy", "", &[i32::MAX], &[1]).await.unwrap();
    assert!(crate::services::skill_plans::validation::audit(&db, &sde).await.is_ok());
    assert!(get(&db, &id, Some(character.character)).await.unwrap().is_some());
    delete(&db, character.character, &id).await.unwrap();
    fixture::cleanup(&db, character).await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn legacy_active_conflicting_with_tracking_aborts_migration() {
    let mut config = crate::config::tests::test_database();
    config.schema_on_start = false;
    let db = crate::db::Db::connect(&config).await.unwrap();
    let character = fixture::create(&db).await.unwrap();
    let (mut client, connection) = tokio_postgres::connect(&config.url, tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let tx = client.transaction().await.unwrap();
    tx.batch_execute("ALTER TABLE game.skill_plans ADD COLUMN active boolean NOT NULL DEFAULT false").await.unwrap();
    let id = [crate::handshake::random_u64().to_be_bytes(), crate::handshake::random_u64().to_be_bytes()].concat();
    create(&tx, &id, character.character, "legacy", "", &[], &[]).await.unwrap();
    tx.execute("UPDATE game.skill_plans SET active=true WHERE plan_id=$1", &[&id]).await.unwrap();
    tx.execute("INSERT INTO game.skill_plan_tracking(character_id,certified_plan_id) VALUES ($1,1)", &[&character.character]).await.unwrap();
    let error = tx.batch_execute(include_str!("../../../../sql/game_schema.sql")).await.unwrap_err();
    assert!(error.as_db_error().is_some_and(|e| e.message().contains("conflicting active skill plan")));
    tx.rollback().await.unwrap();
    assert!(all(&db, character.character).await.unwrap().is_empty());
    fixture::cleanup(&db, character).await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn legacy_active_migration_backfills_and_reapplies_without_loss() {
    let mut config = crate::config::tests::test_database();
    config.schema_on_start = false;
    let db = crate::db::Db::connect(&config).await.unwrap();
    let character = fixture::create(&db).await.unwrap();
    let (mut client, connection) = tokio_postgres::connect(&config.url, tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let tx = client.transaction().await.unwrap();
    tx.batch_execute("ALTER TABLE game.skill_plans ADD COLUMN active boolean NOT NULL DEFAULT false").await.unwrap();
    let id = [crate::handshake::random_u64().to_be_bytes(), crate::handshake::random_u64().to_be_bytes()].concat();
    create(&tx, &id, character.character, "legacy", "", &[], &[]).await.unwrap();
    tx.execute("UPDATE game.skill_plans SET active=true WHERE plan_id=$1", &[&id]).await.unwrap();
    for _ in 0..2 {
        tx.batch_execute(include_str!("../../../../sql/game_schema.sql")).await.unwrap();
        let row = tx.query_one("SELECT personal_plan_id FROM game.skill_plan_tracking WHERE character_id=$1", &[&character.character]).await.unwrap();
        assert_eq!(row.get::<_, Vec<u8>>("personal_plan_id"), id);
        assert!(get(&tx, &id, Some(character.character)).await.unwrap().is_some());
    }
    tx.rollback().await.unwrap();
    fixture::cleanup(&db, character).await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database"]
async fn fresh_skill_plan_schema_applies_repeatedly() {
    let config = crate::config::tests::test_database();
    let (mut client, connection) = tokio_postgres::connect(&config.url, tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let tx = client.transaction().await.unwrap();
    let schema = format!("skill_plan_test_{}", crate::handshake::random_u64());
    tx.batch_execute(&format!("CREATE SCHEMA {schema}; CREATE TABLE {schema}.characters (character_id bigint PRIMARY KEY)")).await.unwrap();
    let sql = include_str!("../../../../sql/game_schema.sql");
    let section = sql.split_once("-- Personal skill plans (public gateway eve_public.character.skill.plan).\n").unwrap().1.split_once("-- Retail Serenity average market prices").unwrap().0;
    let section = section.replace("game.", &format!("{schema}.")).replace("table_schema = 'game'", &format!("table_schema = '{schema}'"));
    tx.batch_execute(&section).await.unwrap();
    tx.batch_execute(&section).await.unwrap();
    let row = tx.query_one("SELECT count(*) AS n FROM information_schema.columns WHERE table_schema=$1 AND table_name='skill_plans' AND column_name='active'", &[&schema]).await.unwrap();
    assert_eq!(row.get::<_, i64>("n"), 0);
    tx.rollback().await.unwrap();
}
