//! Transactional loss-closure deletion and retention guards.
use super::*;

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated database"]
async fn retained_subset_is_guarded_and_other_descendants_are_deleted() {
    let cfg = crate::config::tests::test_database();
    let (mut client, connection) = cfg.to_pg_config().unwrap().connect(tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let tx = client.transaction().await.unwrap();
    let root = super::super::creation::positioned(&tx, 1, 101, 103, [0.0; 3]).await.unwrap();
    let child = super::super::creation::rookie_ship(&tx, 1, 101, root, 4).await.unwrap();
    let grandchild = super::super::creation::rookie_ship(&tx, 1, 101, child, 4).await.unwrap();
    assert!(delete_lost(&tx, root, &[child, child]).await.is_err());
    assert!(delete_lost(&tx, root, &[grandchild, root]).await.is_err());
    assert!(delete_lost(&tx, root, &[i64::MAX]).await.is_err());
    assert_eq!(inventory::descendants(&tx, &[root]).await.unwrap(), vec![root, child, grandchild]);
    delete_lost(&tx, root, &[root, child]).await.unwrap();
    assert_eq!(inventory::descendants(&tx, &[root]).await.unwrap(), vec![root, child]);
    assert!(tx.query_opt("SELECT item_id FROM game.items WHERE item_id=$1", &[&grandchild]).await.unwrap().is_none());
    assert!(delete_lost(&tx, grandchild, &[child]).await.is_err());
    delete_lost(&tx, root, &[]).await.unwrap();
    assert!(inventory::descendants(&tx, &[root]).await.unwrap().is_empty());
    delete_lost(&tx, root, &[]).await.unwrap();
    tx.rollback().await.unwrap();
}
