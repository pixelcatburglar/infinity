//! Salvage chance uses the positive high-is-good difficulty convention.
use super::recovery;
use crate::dao::items::{creation, inventory, loading::{self, ItemQuery}, salvage};

#[test]
fn easier_wrecks_increase_chance_and_extremes_clamp() {
    assert!((recovery::chance(5.0, 30.0) - 0.35).abs() < f64::EPSILON);
    assert!((recovery::chance(25.0, -20.0) - 0.05).abs() < f64::EPSILON);
    assert_eq!(recovery::chance(5.0, -20.0), 0.0);
    assert_eq!(recovery::chance(80.0, 30.0), 1.0);
}

#[tokio::test]
#[ignore]
async fn wreck_claim_is_single_use_and_does_not_delete_contents() {
    let cfg = crate::config::tests::test_database();
    let (mut client, connection) = cfg.to_pg_config().unwrap().connect(tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let tx = client.transaction().await.unwrap();
    let wreck = creation::positioned(&tx, 1, 101, 102, [0.0; 3]).await.unwrap();
    let child = crate::dao::items::transfers::insert(&tx, 1, 101, wreck, 0, 2).await.unwrap();
    assert!(salvage::claim(&tx, wreck, 102, 1).await.unwrap());
    assert!(!salvage::claim(&tx, wreck, 102, 1).await.unwrap());
    let contents = loading::load_items(&tx, ItemQuery::Location(wreck)).await.unwrap();
    assert_eq!(contents.len(), 1);
    assert_eq!(contents[0].item_id, child);
    let container = creation::positioned(&tx, 1, 101, 102, [0.0; 3]).await.unwrap();
    let item = crate::dao::items::loading::Item {
        item_id: child, type_id: 1, owner_id: 101, location_id: wreck,
        flag_id: 0, quantity: 2, singleton: false, group_id: 0, category_id: 0, custom_info: None,
    };
    assert!(inventory::move_item(&tx, &item, container, 0, 101, false).await.unwrap());
    let moved = loading::load_items(&tx, ItemQuery::Location(container)).await.unwrap();
    assert_eq!(moved.len(), 1);
    assert_eq!(moved[0].item_id, child);
    assert_eq!(moved[0].quantity, 2);
    tx.rollback().await.unwrap();
}
