//! Arithmetic and transaction coverage for paid store purchases.
use super::*;
use crate::{db::{Db, outbox::{Effect, Notification}}, services::tests::fixture};
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};

#[test]
fn checked_purchase_arithmetic() {
    for (offer, ty, quantity, price, count) in [
        (0, 1, 1, 1, 1), (1, 0, 1, 1, 1), (1, 1, 0, 1, 1),
        (1, 1, -1, 1, 1), (1, 1, 1, -1, 1), (1, 1, 1, 1, 0),
        (1, 1, 1, 1, -1), (i64::from(i32::MAX) + 1, 1, 1, 1, 1),
        (1, 1, i64::MAX, 1, 2),
        (1, 1, 1, i64::MAX, 2),
    ] {
        assert!(Purchase::new(offer, ty, quantity, price, count).is_none());
    }
    let p = Purchase::new(7, 8, 3, 11, 4).unwrap();
    assert_eq!((p.offer_id, p.type_id, p.count, p.cost, p.delivery), (7, 8, 4, 44, 12));
    assert!(Purchase::new(1, 1, 1, 0, 1).is_some());
    for quantity in [i64::from(i32::MAX) + 1, 1_i64 << 32, (1_i64 << 53) + 1, i64::MAX] {
        assert_eq!(Purchase::new(1, 1, quantity, 0, 1).unwrap().delivery, quantity);
        assert_eq!(Purchase::new(1, 1, 1, 0, quantity).unwrap().count, quantity);
    }
}

async fn buy(db: &Db, payer: i64, target: Option<i64>, price: i64) -> bool {
    let item = Purchase::new(7, 1, 3, price, 2).unwrap();
    db.transaction(move |tx, outbox| {
        let item = item;
        Box::pin(async move { purchase(tx, outbox, payer, target, &item, 42, VaultAudience::Account).await })
    }, |value, outbox: Outbox| async move {
        if value {
            assert!(matches!(&outbox.into_entries()[0], Effect::Notification(Notification::Pilot { event, .. }) if event == "OnAurumChangeFromVgs"));
        } else {
            assert!(outbox.into_entries().is_empty());
        }
        Ok(value)
    }).await.unwrap()
}

async fn fingerprint(db: &Db, payer: i64, recipient: i64) -> (i64, i64, i64) {
    let row = db.query_one("SELECT plex_balance::bigint AS balance, (SELECT count(*) FROM game.store_purchases WHERE user_id=$1) AS purchases, (SELECT count(*) FROM game.redeem_tokens WHERE user_id=$2) AS tokens FROM game.accounts WHERE user_id=$1", &[&payer, &recipient]).await.unwrap();
    (row.get("balance"), row.get("purchases"), row.get("tokens"))
}

#[tokio::test]
#[ignore = "requires the test database"]
async fn atomic_paid_purchases_and_concurrency() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let payer = fixture::create(&db).await.unwrap();
    let recipient = fixture::create(&db).await.unwrap();
    crate::dao::accounts::plex::tests::set_balance(&db, payer.account, "100").await.unwrap();
    assert!(buy(&db, payer.account, None, 5).await);
    assert_eq!(fingerprint(&db, payer.account, payer.account).await, (90, 1, 1));
    assert!(buy(&db, payer.account, Some(recipient.character), 5).await);
    assert_eq!(fingerprint(&db, payer.account, recipient.account).await, (80, 2, 1));
    assert_eq!(crate::dao::accounts::plex::tests::ledger_counts(&db, payer.account).await, (3, 1));
    let row = db.query_one("SELECT type_id, quantity, created FROM game.redeem_tokens WHERE user_id=$1", &[&recipient.account]).await.unwrap();
    assert_eq!((row.get::<_, i32>("type_id"), row.get::<_, i64>("quantity"), row.get::<_, i64>("created")), (1, 6, 42));
    let receipt = db.query_one("SELECT offer_id, quantity, price FROM game.store_purchases WHERE user_id=$1 LIMIT 1", &[&payer.account]).await.unwrap();
    assert_eq!((receipt.get::<_, i32>("offer_id"), receipt.get::<_, i64>("quantity"), receipt.get::<_, i64>("price")), (7, 2, 10));
    assert!(!buy(&db, payer.account, Some(-1), 5).await);
    assert!(!buy(&db, payer.account, Some(i64::MAX), 5).await);
    assert!(!buy(&db, payer.account, None, 100).await);
    assert_eq!(fingerprint(&db, payer.account, recipient.account).await, (80, 2, 1));
    assert_eq!(crate::dao::accounts::plex::tests::ledger_counts(&db, payer.account).await, (3, 1));
    for table in ["store_purchases", "redeem_tokens"] {
        let called = Arc::new(AtomicBool::new(false));
        let flag = called.clone();
        let item = Purchase::new(7, 1, 3, 5, 2).unwrap();
        let result: Result<(), CallError> = db.transaction(move |tx, outbox| {
            let item = item;
            Box::pin(async move {
                tx.batch_execute("CREATE FUNCTION pg_temp.reject_purchase() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'injected failure'; END $$").await.map_err(db_err)?;
                tx.batch_execute(&format!("CREATE TRIGGER reject_purchase BEFORE INSERT ON game.{table} FOR EACH ROW EXECUTE FUNCTION pg_temp.reject_purchase()")).await.map_err(db_err)?;
                purchase(tx, outbox, payer.account, Some(recipient.character), &item, 42, VaultAudience::Account).await?;
                Ok(())
            })
        }, move |(), _: Outbox| async move { flag.store(true, Ordering::SeqCst); Ok(()) }).await;
        assert!(result.is_err());
        assert!(!called.load(Ordering::SeqCst));
        assert_eq!(fingerprint(&db, payer.account, recipient.account).await, (80, 2, 1));
    }
    let called = Arc::new(AtomicBool::new(false));
    let item = Purchase::new(7, 1, 3, 5, 2).unwrap();
    let flag = called.clone();
    let result: Result<(), CallError> = db.transaction(move |tx, outbox| {
        let item = item;
        Box::pin(async move {
            assert!(purchase(tx, outbox, payer.account, Some(recipient.character), &item, 42, VaultAudience::Account).await?);
            Err(CallError::Internal(anyhow::anyhow!("injected failure")))
        })
    }, move |(), _: Outbox| async move { flag.store(true, Ordering::SeqCst); Ok(()) }).await;
    assert!(result.is_err());
    assert!(!called.load(Ordering::SeqCst));
    assert_eq!(fingerprint(&db, payer.account, recipient.account).await, (80, 2, 1));
    assert_eq!(crate::dao::accounts::plex::tests::ledger_counts(&db, payer.account).await, (3, 1));

    crate::dao::accounts::plex::tests::set_balance(&db, payer.account, "10").await.unwrap();
    let (a, b) = tokio::join!(buy(&db, payer.account, None, 5), buy(&db, payer.account, None, 5));
    assert_ne!(a, b);
    assert_eq!(fingerprint(&db, payer.account, payer.account).await, (0, 3, 2));
    db.execute("DELETE FROM game.redeem_tokens WHERE user_id = ANY($1)", &[&[payer.account, recipient.account].as_slice()]).await.unwrap();
    crate::dao::accounts::plex::tests::cleanup(&db, payer.account).await;
    fixture::cleanup(&db, payer).await.unwrap();
    fixture::cleanup(&db, recipient).await.unwrap();
}
