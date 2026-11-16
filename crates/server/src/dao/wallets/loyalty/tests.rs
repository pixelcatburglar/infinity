//! Loyalty balance regression using an owned character and a rolled-back LP fixture.

use super::*;

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL"]
async fn guarded_balance_allows_zero_and_refuses_negative() {
    let config = crate::config::tests::test_database();
    let db = crate::db::Db::connect(&config).await.expect("database pool");
    let fixture = crate::services::tests::fixture::create(&db).await.expect("character fixture");
    let result = tokio::spawn(async move {
        let (mut client, connection) = tokio_postgres::connect(&config.url, tokio_postgres::NoTls).await.expect("connect");
        tokio::spawn(async move { connection.await.expect("connection"); });
        let tx = client.transaction().await.expect("transaction");
        let char_id = fixture.character;
        let corp: i64 = 1;
        let before: i64 = 10;
        tx.execute("INSERT INTO game.lp_balances (char_id, corp_id, amount) VALUES ($1, $2, $3)", &[&char_id, &corp, &before]).await.expect("LP fixture");
        assert_eq!(add(&tx, char_id, corp, -before).await.unwrap(), Some(0));
        assert_eq!(add(&tx, char_id, corp, -1).await.unwrap(), None);
        assert_eq!(balance(&tx, char_id, corp).await.unwrap(), Some(0));
        tx.execute("UPDATE game.lp_balances SET amount = -2 WHERE char_id = $1 AND corp_id = $2", &[&char_id, &corp]).await.unwrap();
        assert_eq!(add(&tx, char_id, corp, 1).await.unwrap(), None);
        assert_eq!(balance(&tx, char_id, corp).await.unwrap(), Some(-2));
        assert_eq!(add(&tx, char_id, corp, 2).await.unwrap(), Some(0));
        assert_eq!(balance(&tx, char_id, corp).await.unwrap(), Some(0));
        assert_eq!(replace(&tx, char_id, corp, Some(0), 5).await.unwrap(), Some(5));
        assert_eq!(add(&tx, char_id, corp, 1).await.unwrap(), Some(6));
        assert_eq!(replace(&tx, char_id, corp, Some(5), 9).await.unwrap(), None);
        assert_eq!(balance(&tx, char_id, corp).await.unwrap(), Some(6));
        assert_eq!(replace(&tx, char_id, corp, Some(6), i64::MAX).await.unwrap(), Some(i64::MAX));
        assert_eq!(add(&tx, char_id, corp, 1).await.unwrap(), None);
        assert_eq!(balance(&tx, char_id, corp).await.unwrap(), Some(i64::MAX));
        assert_eq!(replace(&tx, char_id, corp, Some(i64::MAX), -1).await.unwrap(), None);
        assert_eq!(balance(&tx, char_id, corp).await.unwrap(), Some(i64::MAX));
        tx.execute("DELETE FROM game.lp_balances WHERE char_id = $1 AND corp_id = $2", &[&char_id, &corp]).await.unwrap();
        assert_eq!(replace(&tx, char_id, corp, None, 9007199254740993).await.unwrap(), Some(9007199254740993));
        assert_eq!(balance(&tx, char_id, corp).await.unwrap(), Some(9007199254740993));
        tx.execute("DELETE FROM game.lp_balances WHERE char_id = $1 AND corp_id = $2", &[&char_id, &corp]).await.unwrap();
        assert_eq!(replace(&tx, char_id, corp, None, 0).await.unwrap(), Some(0));
        assert_eq!(replace(&tx, char_id, corp, None, 1).await.unwrap(), None);
        assert_eq!(replace(&tx, char_id, corp, None, -1).await.unwrap(), None);
        assert_eq!(balance(&tx, char_id, corp).await.unwrap(), Some(0));

        // The same issuer keeps separate balances per owner kind.
        let character_owner: i64 = loop {
            let id = tx.query_one("SELECT nextval('game.characters_character_id_seq') AS id", &[]).await.unwrap().get("id");
            if balance(&tx, id, corp).await.unwrap().is_none() { break id; }
        };
        let corporation_owner: i64 = loop {
            let id = tx.query_one("SELECT nextval('game.corporations_corporation_id_seq') AS id", &[]).await.unwrap().get("id");
            if id != character_owner && balance(&tx, id, corp).await.unwrap().is_none() { break id; }
        };
        assert_eq!(add(&tx, character_owner, corp, 10).await.unwrap(), Some(10));
        assert_eq!(add(&tx, corporation_owner, corp, 20).await.unwrap(), Some(20));
        assert_eq!(balance(&tx, character_owner, corp).await.unwrap(), Some(10));
        assert_eq!(balance(&tx, corporation_owner, corp).await.unwrap(), Some(20));
        tx.rollback().await.expect("rollback");
    }).await;
    crate::services::tests::fixture::cleanup(&db, fixture).await.expect("cleanup character fixture");
    if let Err(error) = result {
        if error.is_panic() {
            std::panic::resume_unwind(error.into_panic());
        }
        panic!("loyalty test task failed: {error}");
    }
}
