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
