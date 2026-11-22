//! Real-PostgreSQL coverage for the transient NPC loot helpers: concurrent
//! `items_item_id_seq` reservations against ordinary inserts and the
//! explicit-ID taken slice. The advisory fence is exercised end-to-end by the
//! inventory commit-proof scenario instead of a second holder here.

use super::*;
use crate::db::{Db, outbox::Outbox};
use crate::services::CallError;

/// One committed `reserve_ids` block inside the runner transaction used by
/// cold loading and the refill worker.
async fn reserve(db: &Db, count: i64) -> Vec<i64> {
    db.transaction(
        move |tx, _: &mut Outbox| Box::pin(async move {
            reserve_ids(tx, count).await.map_err(CallError::from)
        }),
        |ids, _| async move { Ok::<Vec<i64>, CallError>(ids) },
    ).await.unwrap()
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL"]
async fn reserved_blocks_share_the_items_sequence_with_plain_inserts() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    // Ordinary inserts and committed reservation blocks consume the same
    // sequence concurrently: every issued ID is owned by exactly one consumer
    // and a block is a real issued set in issue order, not [min, max] math.
    let mut tasks = Vec::new();
    for _ in 0..4 {
        let db = db.clone();
        tasks.push(tokio::spawn(async move { reserve(&db, 64).await }));
    }
    let mut inserted = Vec::new();
    for _ in 0..16 {
        inserted.push(crate::dao::items::transfers::insert(&db, 8, 1, 1, 0, 1).await.unwrap());
    }
    let mut blocks = Vec::new();
    for task in tasks { blocks.push(task.await.unwrap()); }
    let all: std::collections::HashSet<i64> = blocks.iter().flatten().copied().collect();
    assert_eq!(all.len(), 4 * 64, "reserved ids must be unique across blocks");
    for block in &blocks {
        assert_eq!(block.len(), 64);
        assert!(block.windows(2).all(|w| w[1] > w[0]), "issue order");
    }
    assert!(inserted.iter().all(|id| !all.contains(id)), "plain inserts interleave into the same sequence");
    // The same reserved id writes one durable row and conflicts on reuse.
    let item_id = reserve(&db, 1).await[0];
    let item = move |quantity| GameItem {
        item_id, type_id: 8, owner_id: 7, location_id: 9, flag_id: 5,
        quantity, singleton: false, custom_info: None };
    let row = db.transaction(
        move |tx, _: &mut Outbox| Box::pin(async move {
            insert_taken_slice(tx, &item(3)).await.map_err(CallError::from)
        }),
        |row, _| async move { Ok::<GameItem, CallError>(row) },
    ).await.unwrap();
    assert_eq!((row.item_id, row.type_id, row.owner_id, row.location_id, row.flag_id, row.quantity, row.singleton),
        (item_id, 8, 7, 9, 5, 3, false));
    let stored = db.query_one("SELECT type_id, owner_id, location_id, flag_id, quantity, singleton FROM game.items WHERE item_id=$1", &[&item_id]).await.unwrap();
    assert_eq!((stored.get::<_, i32>("type_id"), stored.get::<_, i64>("owner_id"), stored.get::<_, i64>("location_id"),
        stored.get::<_, i32>("flag_id"), stored.get::<_, i64>("quantity"), stored.get::<_, bool>("singleton")), (8, 7, 9, 5, 3, false));
    assert!(insert_taken_slice(&db, &item(1)).await.is_err(), "an already-owned ID is a consistency error, never an ON CONFLICT skip");
    inserted.push(item_id);
    for id in inserted { db.execute("DELETE FROM game.items WHERE item_id=$1", &[&id]).await.unwrap(); }
}
