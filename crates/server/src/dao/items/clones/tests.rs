//! Custody constraints and real item identity on a dedicated PostgreSQL database.
use super::*;
use crate::dao::{characters::clones as characters, items::{brain, dogma, loading::{load_items, ItemQuery}}};

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database"]
async fn custody_preserves_identity_and_isolates_generic_readers() {
    let cfg = crate::config::tests::test_database();
    let (mut client, connection) = cfg.to_pg_config().unwrap().connect(tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let mut tx = client.transaction().await.unwrap();
    let owner: i64 = tx.query_one("SELECT nextval('game.items_item_id_seq') AS id", &[]).await.unwrap().get("id");
    let clone = characters::create(&tx, owner, 1).await.unwrap();
    let item: i64 = tx.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton,brain_slot) VALUES (1,$1,$1,$2,1,true,1) RETURNING item_id", &[&owner, &FLAG_IMPLANT]).await.unwrap().get("item_id");
    assert!(dogma::set_override(&tx, item, 1, 3.5).await.unwrap());
    assert!(park(&tx, owner, clone.clone_id, item).await.unwrap());
    assert!(brain::get(&tx, owner, item).await.unwrap().is_none());
    assert!(load_items(&tx, ItemQuery::IdOrLocation(owner)).await.unwrap().is_empty());
    assert!(load_items(&tx, ItemQuery::Id(item)).await.unwrap().is_empty());
    assert!(crate::dao::items::deliveries::ancestry(&tx, item).await.unwrap().is_empty());
    let parent = crate::dao::items::transfers::insert(&tx, 1, owner, 1, 1, 1).await.unwrap();
    let child = crate::dao::items::transfers::insert(&tx, 1, owner, parent, 1, 1).await.unwrap();
    let ancestors = crate::dao::items::deliveries::ancestry(&tx, child).await.unwrap();
    assert_eq!(ancestors.iter().map(|row| row.item_id).collect::<Vec<_>>(), vec![parent, child]);
    assert!(dogma::overrides(&tx, item).await.unwrap().is_empty());
    assert!(!dogma::set_override(&tx, item, 1, 7.0).await.unwrap());
    assert_eq!(overrides(&tx, owner, clone.clone_id, &[item]).await.unwrap()[0].value, 3.5);
    assert!(crate::dao::items::repair::lock_items(&tx, &[item], owner).await.unwrap().is_empty());
    crate::dao::items::transfers::hand_over(&tx, &[crate::dao::items::transfers::Transfer {
        item_id: item, owner: owner + 1, location: 2, flag: 1,
    }]).await.unwrap();
    assert_eq!(list(&tx, owner, clone.clone_id).await.unwrap()[0].item_id, item);
    assert!(list(&tx, owner + 1, clone.clone_id).await.unwrap().is_empty());
    assert!(!activate(&tx, owner + 1, clone.clone_id, item).await.unwrap());
    let save = tx.savepoint("restrict_delete").await.unwrap();
    assert!(characters::destroy(&save, clone.clone_id, owner).await.is_err());
    save.rollback().await.unwrap();
    assert!(activate(&tx, owner, clone.clone_id, item).await.unwrap());
    assert_eq!(brain::get(&tx, owner, item).await.unwrap().unwrap().item_id, item);
    assert_eq!(dogma::overrides(&tx, item).await.unwrap()[0].value, 3.5);
    assert_eq!(characters::destroy(&tx, clone.clone_id, owner).await.unwrap(), 1);
    tx.rollback().await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database"]
async fn custody_rejects_cross_owner_and_duplicate_slots() {
    let cfg = crate::config::tests::test_database();
    let (mut client, connection) = cfg.to_pg_config().unwrap().connect(tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let mut tx = client.transaction().await.unwrap();
    let owner: i64 = tx.query_one("SELECT nextval('game.items_item_id_seq') AS id", &[]).await.unwrap().get("id");
    let clone = characters::create(&tx, owner, 1).await.unwrap();
    let item: i64 = tx.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton,brain_slot,jump_clone_id) VALUES (1,$1,$1,$2,1,true,1,$3) RETURNING item_id", &[&owner, &FLAG_IMPLANT, &clone.clone_id]).await.unwrap().get("item_id");
    let save = tx.savepoint("duplicate_slot").await.unwrap();
    assert!(save.execute("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton,brain_slot,jump_clone_id) VALUES (1,$1,$1,$2,1,true,1,$3)", &[&owner, &FLAG_IMPLANT, &clone.clone_id]).await.is_err());
    save.rollback().await.unwrap();
    let save = tx.savepoint("foreign_owner").await.unwrap();
    assert!(save.execute("UPDATE game.items SET owner_id=$2,location_id=$2 WHERE item_id=$1", &[&item, &(owner + 1)]).await.is_err());
    save.rollback().await.unwrap();
    let save = tx.savepoint("invalid_shape").await.unwrap();
    assert!(save.execute("UPDATE game.items SET brain_slot=NULL WHERE item_id=$1", &[&item]).await.is_err());
    save.rollback().await.unwrap();
    assert_eq!(destroy(&tx, owner, clone.clone_id).await.unwrap(), 1);
    assert_eq!(characters::destroy(&tx, clone.clone_id, owner).await.unwrap(), 1);
    tx.rollback().await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database"]
async fn same_station_clone_replacement_defers_only_location_constraint() {
    let cfg = crate::config::tests::test_database();
    let (mut client, connection) = cfg.to_pg_config().unwrap().connect(tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let tx = client.transaction().await.unwrap();
    let owner: i64 = tx.query_one("SELECT nextval('game.items_item_id_seq') AS id", &[]).await.unwrap().get("id");
    let old = characters::create(&tx, owner, 1).await.unwrap();
    characters::defer_location(&tx).await.unwrap();
    let new = characters::create(&tx, owner, 1).await.unwrap();
    assert_ne!(old.clone_id, new.clone_id);
    assert_eq!(characters::destroy(&tx, old.clone_id, owner).await.unwrap(), 1);
    characters::restore_location(&tx).await.unwrap();
    tx.rollback().await.unwrap();
}
