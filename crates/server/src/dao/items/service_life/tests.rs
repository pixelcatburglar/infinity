//! Guarded service-life transitions and finite constraints on a dedicated PostgreSQL database.

use super::*;

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database"]
async fn guarded_damage_transitions_follow_the_expected_row() {
    let cfg = crate::config::tests::test_database();
    let (mut client, connection) = cfg.to_pg_config().unwrap().connect(tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let mut tx = client.transaction().await.unwrap();
    let insert = "INSERT INTO game.items (type_id, owner_id, location_id, flag_id, quantity, singleton) \
        VALUES (1, 1, 1, 1, 1, true) RETURNING item_id";
    let a = ItemId::new(tx.query_one(insert, &[]).await.unwrap().get("item_id")).unwrap();
    let b = ItemId::new(tx.query_one(insert, &[]).await.unwrap().get("item_id")).unwrap();
    assert!(get_many(&tx, &[a, b]).await.unwrap().is_empty());
    assert!(set_if(&tx, a, None, 12.5).await.unwrap());
    assert!(!set_if(&tx, a, None, 30.0).await.unwrap());
    assert!(!set_if(&tx, a, Some(9.0), 30.0).await.unwrap());
    assert!(set_if(&tx, a, Some(12.5), 30.0).await.unwrap());
    let damage = get_many(&tx, &[a, b]).await.unwrap();
    assert_eq!((damage.len(), damage.get(&a)), (1, Some(&30.0)));
    // The finite constraint rejects non-finite damage; the savepoint keeps the transaction usable.
    let save = tx.savepoint("non_finite").await.unwrap();
    assert!(set_if(&save, b, None, f64::INFINITY).await.is_err());
    save.rollback().await.unwrap();
    assert!(get_many(&tx, &[b]).await.unwrap().is_empty());
    tx.rollback().await.unwrap();
}
