//! Real-database regression for contract-style recursive handovers.
use super::*;

#[tokio::test]
#[ignore]
async fn contract_handover_preserves_nested_locations() {
    let cfg = crate::config::tests::test_database();
    let (mut client, connection) = cfg.to_pg_config().unwrap().connect(tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let tx = client.transaction().await.unwrap();
    let root = insert(&tx, 1, 101, 102, 4, 1).await.unwrap();
    let container = insert(&tx, 1, 101, root, 5, 1).await.unwrap();
    let item = insert(&tx, 1, 101, container, 6, 1).await.unwrap();
    hand_over(&tx, &[Transfer { item_id: root, owner: 103, location: 104, flag: 4 }]).await.unwrap();
    let rows = tx.query("SELECT item_id, owner_id, location_id FROM game.items WHERE item_id = ANY($1) ORDER BY item_id", &[&vec![root, container, item]]).await.unwrap();
    assert_eq!(rows.len(), 3);
    for row in &rows { assert_eq!(row.get::<_, i64>("owner_id"), 103); }
    assert_eq!(rows[0].get::<_, i64>("location_id"), 104);
    assert_eq!(rows[1].get::<_, i64>("location_id"), root);
    assert_eq!(rows[2].get::<_, i64>("location_id"), container);
    tx.rollback().await.unwrap();
}

#[tokio::test]
#[ignore]
async fn arithmetic_rejects_singletons_and_updates_stacks() {
    let cfg = crate::config::tests::test_database();
    let (mut client, connection) = cfg.to_pg_config().unwrap().connect(tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let tx = client.transaction().await.unwrap();
    let wrap = insert(&tx, 1, 101, 102, 4, -12345).await.unwrap();
    tx.execute("UPDATE game.items SET singleton = true WHERE item_id = $1", &[&wrap]).await.unwrap();
    let stack = insert(&tx, 1, 101, 102, 4, 5).await.unwrap();
    assert!(!take(&tx, wrap, 1).await.unwrap());
    assert!(!add(&tx, wrap, 1).await.unwrap());
    assert!(take(&tx, stack, 2).await.unwrap());
    assert!(add(&tx, stack, 1).await.unwrap());
    assert!(!take(&tx, 0, 1).await.unwrap());
    assert!(!add(&tx, 0, 1).await.unwrap());
    let rows = tx.query("SELECT item_id, quantity FROM game.items WHERE item_id = ANY($1) ORDER BY item_id", &[&vec![wrap, stack]]).await.unwrap();
    assert_eq!(rows[0].get::<_, i32>("quantity"), -12345);
    assert_eq!(rows[1].get::<_, i32>("quantity"), 4);
    tx.rollback().await.unwrap();
}
