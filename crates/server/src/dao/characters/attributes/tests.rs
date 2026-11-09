use super::*;

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database"]
async fn grant_checks_bounds_deletion_and_transaction_rollback() {
    let (mut client, connection) = tokio_postgres::connect(&crate::config::tests::test_database().url, tokio_postgres::NoTls).await.unwrap();
    let driver = tokio::spawn(async move { connection.await.unwrap() });
    let mut tx = client.transaction().await.unwrap();
    let user: i64 = tx.query_one("INSERT INTO game.accounts (user_name,password_hash,role) VALUES ('free-points-test-' || nextval('game.accounts_user_id_seq'),$1,0) RETURNING user_id", &[&&b"fixture"[..]]).await.unwrap().get("user_id");
    let character: i64 = tx.query_one("INSERT INTO game.characters (user_id,name,race_id,bloodline_id,gender_id,corporation_id,free_skill_points) VALUES ($1,'free-points-test-' || nextval('game.characters_character_id_seq'),1,1,1,1,0) RETURNING character_id", &[&user]).await.unwrap().get("character_id");

    assert_eq!(grant_free_points(&tx, character, 5).await.unwrap(), Some(5));
    assert_eq!(grant_free_points(&tx, character, 7).await.unwrap(), Some(12));
    for points in [0, -1, i64::MIN] {
        assert_eq!(grant_free_points(&tx, character, points).await.unwrap(), None);
        assert_eq!(free_points(&tx, character).await.unwrap(), 12);
    }
    assert_eq!(grant_free_points(&tx, i64::MIN, 1).await.unwrap(), None);
    assert_eq!(free_points(&tx, character).await.unwrap(), 12);

    tx.execute("UPDATE game.characters SET deleted = true WHERE character_id = $1", &[&character]).await.unwrap();
    assert_eq!(grant_free_points(&tx, character, 1).await.unwrap(), None);
    assert_eq!(free_points(&tx, character).await.unwrap(), 12);
    tx.execute("UPDATE game.characters SET deleted = false, free_skill_points = $2 WHERE character_id = $1", &[&character, &(i64::MAX - 1)]).await.unwrap();
    assert_eq!(grant_free_points(&tx, character, 2).await.unwrap(), None);
    assert_eq!(free_points(&tx, character).await.unwrap(), i64::MAX - 1);
    assert_eq!(grant_free_points(&tx, character, 1).await.unwrap(), Some(i64::MAX));
    assert_eq!(grant_free_points(&tx, character, 1).await.unwrap(), None);
    assert_eq!(free_points(&tx, character).await.unwrap(), i64::MAX);

    tx.execute("UPDATE game.characters SET free_skill_points = 0 WHERE character_id = $1", &[&character]).await.unwrap();
    {
        let nested = tx.transaction().await.unwrap();
        assert_eq!(grant_free_points(&nested, character, i64::MAX).await.unwrap(), Some(i64::MAX));
        nested.rollback().await.unwrap();
    }
    assert_eq!(free_points(&tx, character).await.unwrap(), 0);
    tx.rollback().await.unwrap();
    drop(client);
    driver.await.unwrap();
}
