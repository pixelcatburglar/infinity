//! Guarded quantity changes against stale item types, using real transactions.
use super::*;
use crate::dao::items::{loading::ItemQuery, transfers};
use crate::db::{outbox::Outbox, Db};
use crate::services::{db_err, items::load_items, notify_error, CallError};
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated database with SDE"]
async fn stale_type_refuses_update_and_delete_and_rolls_back_prior_changes() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let mut types = sde.types.iter().filter(|ty| ty.type_id > 0).map(|ty| ty.type_id);
    let first_type = types.next().unwrap();
    let changed_type = types.find(|ty| *ty != first_type).unwrap();
    for deletion in [false, true] {
        let first = transfers::insert(&db, first_type, 101, 102, 1, 2).await.unwrap();
        let stale = transfers::insert(&db, first_type, 101, 102, 1, 2).await.unwrap();
        let items = load_items(&db, &sde, ItemQuery::Ids(&[first, stale])).await.unwrap();
        let first_row = items.iter().find(|item| item.item_id == first).unwrap();
        let stale_row = items.iter().find(|item| item.item_id == stale).unwrap();
        let changes = BTreeMap::from([
            (first, QuantityChange::new(first_row, Some(1))),
            (stale, QuantityChange::new(stale_row, if deletion { None } else { Some(1) })),
        ]);
        db.execute("UPDATE game.items SET type_id = $2 WHERE item_id = $1", &[&stale, &changed_type]).await.unwrap();
        if !deletion {
            assert!(!crate::dao::items::inventory::move_item(&db, stale_row, 103, 1, 101, false).await.unwrap());
            assert!(crate::dao::items::inventory::split(&db, stale_row, 1, 101, 103, 1, false).await.unwrap().is_none());
        }
        let dispatched = Arc::new(AtomicBool::new(false));
        let delivered = dispatched.clone();
        let result: Result<(), CallError> = db.transaction(
            move |tx, outbox: &mut Outbox| {
                let changes = changes.clone();
                Box::pin(async move {
                    outbox.notify_client("OnItemChange", vec![]);
                    if !apply(tx, &changes).await.map_err(db_err)? {
                        return Err(notify_error(infinity_core::messages::ITEM_NOT_AT_ORIGINAL_LOCATION));
                    }
                    Ok(())
                })
            },
            move |(), _| async move { delivered.store(true, Ordering::SeqCst); Ok(()) },
        ).await;
        assert!(result.is_err());
        assert!(!dispatched.load(Ordering::SeqCst));
        let rows = crate::dao::items::loading::load_items(&db, ItemQuery::Ids(&[first, stale])).await.unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows.iter().find(|row| row.item_id == first).unwrap().quantity, 2);
        let changed = rows.iter().find(|row| row.item_id == stale).unwrap();
        assert_eq!((changed.type_id, changed.quantity), (changed_type, 2));
        crate::dao::items::inventory::delete(&db, first).await.unwrap();
        crate::dao::items::inventory::delete(&db, stale).await.unwrap();
    }
}
