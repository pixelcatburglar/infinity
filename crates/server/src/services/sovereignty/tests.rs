//! Database checks for transaction-owned sovereignty mutations.
use crate::dao::{items::sovereignty as items, sovereignty::systems};
mod deploy;
mod capture;
mod lifecycle;
mod recovery;
mod governance;
mod integration;
mod wire;
mod queries;

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database"]
async fn owner_change_preserves_position_and_scoped_claim_lookup() {
    let cfg = crate::config::tests::test_database();
    let (mut client, connection) = cfg.to_pg_config().unwrap().connect(tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let tx = client.transaction().await.unwrap();
    let item: i64 = tx.query_one(
        "INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton,x,y,z) VALUES (1,2,3,0,1,true,10,20,30) RETURNING item_id", &[],
    ).await.unwrap().get("item_id");
    assert!(!items::transfer_owner(&tx, item, 3, 1, 4, 5).await.unwrap());
    assert!(items::transfer_owner(&tx, item, 3, 1, 2, 5).await.unwrap());
    let row = tx.query_one("SELECT owner_id,x,y,z FROM game.items WHERE item_id=$1", &[&item]).await.unwrap();
    assert_eq!(row.get::<_, i64>("owner_id"), 5);
    assert_eq!((row.get::<_, f64>("x"), row.get::<_, f64>("y"), row.get::<_, f64>("z")), (10.0, 20.0, 30.0));
    let initial = systems::lock(&tx, item).await.unwrap();
    let claimed = systems::SystemChange { hub_item_id: Some(item), claim_corporation_id: Some(item), alliance_id: Some(item), claim_time: Some(1), ownership_epoch: 1 };
    systems::replace(&tx, &initial, &claimed).await.unwrap().unwrap();
    let rows = systems::for_corporation(&tx, item).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].system_id, item);
    assert!(systems::for_corporation(&tx, -item).await.unwrap().is_empty());
    tx.rollback().await.unwrap();
}
