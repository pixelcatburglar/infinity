//! Real-database regression for contract-style recursive handovers.
use super::*;

#[tokio::test]
#[ignore]
async fn contract_handover_preserves_nested_locations() {
    let cfg = crate::config::tests::test_database();
    let (mut client, connection) = cfg.to_pg_config().unwrap().connect(tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let tx = client.transaction().await.unwrap();
    let db = crate::db::Db::connect(&cfg).await.unwrap();
    let sde = crate::sde::Sde::load(&db).await.unwrap();
    let root = insert(&tx, 1, 101, 102, 4, 1).await.unwrap();
    let container = insert(&tx, 1, 101, root, 5, 1).await.unwrap();
    let item = insert(&tx, 1, 101, container, 6, 1).await.unwrap();
    let closure = crate::dao::items::inventory::descendants(&tx, &[root]).await.unwrap();
    let lineage = crate::dao::items::deliveries::ancestry_many(&tx, &closure).await.unwrap();
    let mut requests = crate::services::inventory::destination::parent_requests(
        &sde, lineage.iter().map(|row| (row.ancestor.item_id, row.ancestor.type_id)),
        crate::dao::items::parent_locks::LockStrength::Update, crate::dao::items::parent_locks::LockStrength::Share,
    ).unwrap();
    requests.extend(closure.iter().map(|&item_id| crate::dao::items::parent_locks::LockRequest {
        item_id, strength: crate::dao::items::parent_locks::LockStrength::Update,
    }));
    let locks = crate::services::inventory::destination::lock_items(&tx, &sde, &requests, &[]).await.unwrap();
    assert_eq!(crate::dao::items::inventory::descendants(&tx, &[root]).await.unwrap(), closure);
    assert!(crate::dao::items::deliveries::ancestry_many(&tx, &closure).await.unwrap() == lineage);
    hand_over(&tx, &[Transfer { item_id: root, owner: 103, location: 104, flag: 4 }], &closure, &locks, &[]).await.unwrap();
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
async fn deposit_splits_overflow_and_validates_created_rows() {
    let cfg = crate::config::tests::test_database();
    let (mut client, connection) = cfg.to_pg_config().unwrap().connect(tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let tx = client.transaction().await.unwrap();
    let max = infinity_core::consts::MAX_STACK_QUANTITY;
    let id = insert(&tx, 1, 101, 102, 4, max - 3).await.unwrap();
    let result = deposit_stack(&tx, 102, 4, 1, 101, 10, DepositCandidate::InStatement, None).await.unwrap();
    let merged = result.merged.unwrap();
    let created = result.created.unwrap();
    assert_eq!((merged.item.item_id, merged.old_quantity, merged.added_quantity, merged.item.quantity), (id, Some(max - 3), 3, max));
    assert_eq!((created.old_quantity, created.added_quantity, created.item.quantity), (None, 7, 7));
    assert_ne!(id, created.item.item_id);
    let captured = deposit_candidate(&tx, 102, 4, 1, 101).await.unwrap().unwrap();
    assert_eq!(captured.item_id, created.item.item_id);
    tx.execute("UPDATE game.items SET quantity = $2 WHERE item_id = $1", &[&captured.item_id, &max]).await.unwrap();
    let drift = deposit_stack(&tx, 102, 4, 1, 101, 1, DepositCandidate::Created(&captured), None).await.unwrap();
    assert!(drift.merged.is_none() && drift.created.is_none());
    let full = crate::dao::items::parent_locks::capture(&tx, &[captured.item_id]).await.unwrap().pop().unwrap();
    tx.execute("DELETE FROM game.items WHERE item_id = $1", &[&captured.item_id]).await.unwrap();
    let missing = deposit_stack(&tx, 102, 4, 1, 101, 1, DepositCandidate::Created(&full), None).await.unwrap();
    assert!(missing.merged.is_none() && missing.created.is_none());
    let replacement = insert(&tx, 1, 101, 102, 4, max).await.unwrap();
    let full = crate::dao::items::parent_locks::capture(&tx, &[replacement]).await.unwrap().pop().unwrap();
    let overflow = deposit_stack(&tx, 102, 4, 1, 101, 1, DepositCandidate::Created(&full), None).await.unwrap();
    assert!(overflow.merged.is_none());
    assert_eq!(overflow.created.unwrap().added_quantity, 1);
    let singleton = insert(&tx, 1, 101, 103, 4, -12345).await.unwrap();
    tx.execute("UPDATE game.items SET singleton = true WHERE item_id = $1", &[&singleton]).await.unwrap();
    let separate = deposit_stack(&tx, 103, 4, 1, 101, 1, DepositCandidate::InStatement, None).await.unwrap();
    assert!(separate.merged.is_none());
    assert_ne!(separate.created.unwrap().item.item_id, singleton);
    let row = tx.query_one("SELECT quantity FROM game.items WHERE item_id = $1", &[&singleton]).await.unwrap();
    assert_eq!(row.get::<_, i64>("quantity"), -12345);
    tx.rollback().await.unwrap();
}

#[tokio::test]
#[ignore]
async fn prepared_expected_quantity_and_wide_values() {
    use crate::dao::items::parent_locks::{self, LockRequest, LockStrength};
    let cfg = crate::config::tests::test_database();
    let (mut client, connection) = cfg.to_pg_config().unwrap().connect(tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let tx = client.transaction().await.unwrap();
    for (index, quantity) in [1_i64 << 31, 1_i64 << 32, (1_i64 << 53) + 1, infinity_core::consts::MAX_STACK_QUANTITY].into_iter().enumerate() {
        let location = 200 + index as i64;
        let first = deposit_stack(&tx, location, 4, 1, 101, quantity, DepositCandidate::InStatement, None).await.unwrap();
        assert_eq!(first.created.unwrap().added_quantity, quantity);
    }
    let max = infinity_core::consts::MAX_STACK_QUANTITY;
    let id = insert(&tx, 1, 101, 300, 4, max - 3).await.unwrap();
    let captured = deposit_candidate(&tx, 300, 4, 1, 101).await.unwrap().unwrap();
    let locks = parent_locks::lock(&tx, &[], &[LockRequest { item_id: id, strength: LockStrength::Update }]).await.unwrap();
    let exact = deposit_stack(&tx, 300, 4, 1, 101, 3, DepositCandidate::Prepared(Some((&captured, max - 3))), Some(&locks)).await.unwrap();
    assert_eq!(exact.merged.unwrap().item.quantity, max);
    assert!(exact.created.is_none());
    let full = deposit_stack(&tx, 300, 4, 1, 101, max, DepositCandidate::Prepared(Some((&captured, max))), Some(&locks)).await.unwrap();
    assert!(full.merged.is_none());
    assert_eq!(full.created.unwrap().added_quantity, max);
    let stale = deposit_stack(&tx, 300, 4, 1, 101, 1, DepositCandidate::Prepared(Some((&captured, max - 3))), Some(&locks)).await.unwrap();
    assert!(stale.merged.is_none());
    assert_eq!(stale.created.unwrap().added_quantity, 1);
    let total: String = tx.query_one("SELECT sum(quantity::numeric)::text AS total FROM game.items WHERE location_id = $1 AND owner_id = $2", &[&300_i64, &101_i64]).await.unwrap().get("total");
    assert_eq!(total.parse::<i128>().unwrap(), i128::from(max) * 2 + 1);
    let zero = deposit_stack(&tx, 300, 4, 1, 101, 0, DepositCandidate::InStatement, None).await.unwrap();
    assert!(zero.merged.is_none() && zero.created.is_none());
    tx.rollback().await.unwrap();
}

#[tokio::test]
#[ignore = "requires a dedicated PostgreSQL database"]
async fn concurrent_deposits_preserve_total_across_physical_stacks() {
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let parent = insert(&db, 1, 101, 102, 4, 1).await.unwrap();
    let result = crate::tests::catch_scenario(async {
        let max = infinity_core::consts::MAX_STACK_QUANTITY;
        insert(&db, 1, 101, parent, 4, max - 3).await.unwrap();
        let (first, second) = tokio::join!(
            deposit_stack(&db, parent, 4, 1, 101, 10, DepositCandidate::InStatement, None),
            deposit_stack(&db, parent, 4, 1, 101, 10, DepositCandidate::InStatement, None),
        );
        for receipt in [first.unwrap(), second.unwrap()] {
            let added: i128 = receipt.merged.iter().chain(receipt.created.iter()).map(|row| i128::from(row.added_quantity)).sum();
            assert_eq!(added, 10);
        }
        let total: String = db.query_one("SELECT sum(quantity::numeric)::text AS total FROM game.items WHERE location_id = $1", &[&parent]).await.unwrap().get("total");
        assert_eq!(total.parse::<i128>().unwrap(), i128::from(max) + 17);
    }).await;
    db.execute("DELETE FROM game.items WHERE item_id = $1 OR location_id = $1", &[&parent]).await.unwrap();
    if let Err(panic) = result { std::panic::resume_unwind(panic); }
}

#[tokio::test]
#[ignore = "requires a dedicated PostgreSQL database"]
async fn later_write_failure_rolls_back_split_deposit() {
    let cfg = crate::config::tests::test_database();
    let (mut client, connection) = cfg.to_pg_config().unwrap().connect(tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let db = crate::db::Db::connect(&cfg).await.unwrap();
    let parent = insert(&db, 1, 101, 102, 4, 1).await.unwrap();
    let result = crate::tests::catch_scenario(async {
        let max = infinity_core::consts::MAX_STACK_QUANTITY;
        let source = insert(&db, 1, 101, parent, 5, 11).await.unwrap();
        let target = insert(&db, 1, 101, parent, 4, max - 3).await.unwrap();
        let tx = client.transaction().await.unwrap();
        tx.execute("UPDATE game.items SET quantity = quantity - $2 WHERE item_id = $1", &[&source, &10_i64]).await.unwrap();
        let receipt = deposit_stack(&tx, parent, 4, 1, 101, 10, DepositCandidate::InStatement, None).await.unwrap();
        let created = receipt.created.unwrap().item.item_id;
        assert!(receipt.merged.is_some());
        assert!(tx.execute("UPDATE game.items SET quantity = NULL WHERE item_id = $1", &[&target]).await.is_err());
        tx.rollback().await.unwrap();
        let rows = client.query("SELECT item_id, quantity FROM game.items WHERE item_id = ANY($1) ORDER BY item_id", &[&vec![source, target, created]]).await.unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!((rows[0].get::<_, i64>("item_id"), rows[0].get::<_, i64>("quantity")), (source, 11));
        assert_eq!((rows[1].get::<_, i64>("item_id"), rows[1].get::<_, i64>("quantity")), (target, max - 3));
    }).await;
    client.execute("DELETE FROM game.items WHERE item_id = $1 OR location_id = $1", &[&parent]).await.unwrap();
    if let Err(panic) = result { std::panic::resume_unwind(panic); }
}
