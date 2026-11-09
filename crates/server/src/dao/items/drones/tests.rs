//! Transaction-rolled-back database tests for persistent drone identities.
use super::*;

async fn source(tx: &(impl Executor + ?Sized), quantity: i64, singleton: bool) -> Item {
    let row = tx.query_one("INSERT INTO game.items (type_id, owner_id, location_id, flag_id, quantity, singleton) VALUES (1, 101, 102, $1, $2, $3) RETURNING item_id", &[&FLAG_DRONE_BAY, &quantity, &singleton]).await.unwrap();
    Item { item_id: row.get("item_id"), type_id: 1, owner_id: 101, location_id: 102, flag_id: FLAG_DRONE_BAY, quantity, singleton, group_id: 0, category_id: 0, custom_info: None }
}

#[tokio::test]
#[ignore]
async fn launch_guards_and_preserves_stack_identities() {
    let cfg = crate::config::tests::test_database();
    let (mut client, connection) = cfg.to_pg_config().unwrap().connect(tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let tx = client.transaction().await.unwrap();
    let item = source(&tx, 3, false).await;
    tx.execute("INSERT INTO game.item_attributes (item_id, attribute_id, value) VALUES ($1, 1, 42)", &[&item.item_id]).await.unwrap();
    let p = [1.0, 2.0, 3.0];
    assert!(launch(&tx, &item, 1, 999, 102, 103, p).await.unwrap().is_none());
    assert!(launch(&tx, &item, 1, 101, 999, 103, p).await.unwrap().is_none());
    assert!(launch(&tx, &item, 4, 101, 102, 103, p).await.unwrap().is_none());
    let mut stale = item.clone(); stale.flag_id = 0;
    assert!(launch(&tx, &stale, 1, 101, 102, 103, p).await.unwrap().is_none());
    stale = item.clone(); stale.type_id = 2;
    assert!(launch(&tx, &stale, 1, 101, 102, 103, p).await.unwrap().is_none());
    stale = item.clone(); stale.quantity = 2;
    assert!(launch(&tx, &stale, 1, 101, 102, 103, p).await.unwrap().is_none());
    let partial = launch(&tx, &item, 1, 101, 102, 103, p).await.unwrap().unwrap();
    assert_ne!(partial[0], item.item_id);
    let mut remaining = item.clone(); remaining.quantity = 2;
    let all = launch(&tx, &remaining, 2, 101, 102, 103, p).await.unwrap().unwrap();
    assert_eq!(all.len(), 2); assert_eq!(all[0], item.item_id);
    let rows = positioned(&tx, 103, &[1]).await.unwrap();
    assert_eq!(rows.iter().filter(|r| r.item_id == item.item_id || partial.contains(&r.item_id) || all.contains(&r.item_id)).count(), 3);
    let attrs = tx.query("SELECT item_id FROM game.item_attributes WHERE item_id = ANY($1)", &[&vec![item.item_id, partial[0], all[1]]]).await.unwrap();
    assert_eq!(attrs.len(), 1); assert_eq!(attrs[0].get::<_, i64>("item_id"), item.item_id);
    let single = source(&tx, 1, true).await;
    assert_eq!(launch(&tx, &single, 1, 101, 102, 103, p).await.unwrap().unwrap(), vec![single.item_id]);
    let large = source(&tx, i64::from(i32::MAX) + 2, false).await;
    assert_eq!(launch(&tx, &large, 1, 101, 102, 103, p).await.unwrap().unwrap().len(), 1);
    let quantity: i64 = tx.query_one("SELECT quantity FROM game.items WHERE item_id = $1", &[&large.item_id]).await.unwrap().get("quantity");
    assert_eq!(quantity, i64::from(i32::MAX) + 1);
    tx.rollback().await.unwrap();
}

#[tokio::test]
#[ignore]
async fn return_abandon_and_death_have_one_guarded_winner() {
    let cfg = crate::config::tests::test_database();
    let (mut client, connection) = cfg.to_pg_config().unwrap().connect(tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let tx = client.transaction().await.unwrap();
    let item = source(&tx, 1, true).await;
    launch(&tx, &item, 1, 101, 102, 103, [1.0, 2.0, 3.0]).await.unwrap().unwrap();
    assert!(!abandon(&tx, item.item_id, 999, 103).await.unwrap());
    assert!(!destroy(&tx, item.item_id, 101, 999).await.unwrap());
    assert!(abandon(&tx, item.item_id, 101, 103).await.unwrap());
    assert!(!destroy(&tx, item.item_id, 101, 103).await.unwrap());
    assert!(return_to_bay(&tx, item.item_id, OWNER_UNOWNED, 103, 104, 105).await.unwrap());
    assert!(!destroy(&tx, item.item_id, OWNER_UNOWNED, 103).await.unwrap());
    let doomed = source(&tx, 1, true).await;
    launch(&tx, &doomed, 1, 101, 102, 103, [1.0, 2.0, 3.0]).await.unwrap().unwrap();
    tx.execute("INSERT INTO game.item_attributes (item_id, attribute_id, value) VALUES ($1, 1, 42)", &[&doomed.item_id]).await.unwrap();
    assert!(destroy(&tx, doomed.item_id, 101, 103).await.unwrap());
    assert!(!return_to_bay(&tx, doomed.item_id, 101, 103, 101, 102).await.unwrap());
    assert!(tx.query("SELECT item_id FROM game.item_attributes WHERE item_id = $1", &[&doomed.item_id]).await.unwrap().is_empty());
    tx.rollback().await.unwrap();
}
