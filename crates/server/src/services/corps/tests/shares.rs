//! Share initialization, authorization, guarded writes and operation receipts.
use super::fixtures;
use crate::{dao::corporations::shares as ledger, db::outbox::Outbox, services::corps::shares::transfer_core};

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn issuance_transfer_replay_and_refusals() {
    let mut client = fixtures::connect().await;
    let tx = client.transaction().await.unwrap();
    let ceo = fixtures::character(&tx).await;
    let recipient = fixtures::character(&tx).await;
    let issuer = fixtures::corporation(&tx, ceo).await;
    assert!(ledger::initialize(&tx, issuer, 1000).await.unwrap());
    assert!(!ledger::initialize(&tx, issuer, 2000).await.unwrap());
    assert_eq!(ledger::issued_total(&tx, issuer).await.unwrap(), Some(1000));
    let op = ledger::operation_id(&tx).await.unwrap();
    let mut outbox = Outbox::<()>::default();
    transfer_core(&tx, &mut outbox, op, ceo, issuer, issuer, recipient, 1000, true, 1).await.unwrap();
    assert_eq!(ledger::balance(&tx, issuer, issuer).await.unwrap(), 0);
    assert_eq!(ledger::balance(&tx, issuer, recipient).await.unwrap(), 1000);
    let entries = outbox.into_entries();
    assert_eq!(entries.len(), 2);
    let columns = ["corporationID", "shareholderID", "shareholderCorporationID", "shares"];
    let treasury = [marshal::Value::Int(issuer), marshal::Value::Int(issuer), marshal::Value::None, marshal::Value::Int(1000)];
    let target = [marshal::Value::Int(issuer), marshal::Value::Int(recipient), marshal::Value::Int(1), marshal::Value::Int(1000)];
    for (entry, observer, holder, values, removed) in [
        (&entries[0], ceo, issuer, &treasury, true),
        (&entries[1], recipient, recipient, &target, false),
    ] {
        let crate::db::outbox::Effect::Notification(crate::db::outbox::Notification::Pilot { char_id, event, args }) = entry else { panic!("expected pilot notification") };
        assert_eq!(*char_id, observer);
        assert_eq!(event, "OnShareChange");
        let change = marshal::Value::dict(columns.iter().enumerate().map(|(i, key)| {
            let (old, new) = if removed { (values[i].clone(), marshal::Value::None) } else { (marshal::Value::None, values[i].clone()) };
            (*key, marshal::Value::tuple([old, new]))
        }));
        assert_eq!(args, &vec![marshal::Value::Int(holder), marshal::Value::Int(issuer), change]);
    }
    tx.execute("UPDATE game.corporations SET ceo_id=$2 WHERE corporation_id=$1", &[&issuer, &recipient]).await.unwrap();
    tx.execute("UPDATE game.characters SET corporation_id=1 WHERE character_id=$1", &[&ceo]).await.unwrap();
    let mut replay = Outbox::<()>::default();
    transfer_core(&tx, &mut replay, op, ceo, -1, -1, -1, -1, true, 1).await.unwrap();
    assert!(replay.into_entries().is_empty());
    assert_eq!(tx.query_one("SELECT count(*) AS count FROM game.corp_share_transfers WHERE operation_id=$1", &[&op]).await.unwrap().get::<_, i64>("count"), 1);
    tx.execute("UPDATE game.characters SET corporation_id=$1 WHERE character_id=$2", &[&issuer, &ceo]).await.unwrap();
    tx.execute("UPDATE game.corporations SET ceo_id=$2 WHERE corporation_id=$1", &[&issuer, &ceo]).await.unwrap();
    for (actor, bound, target, qty) in [(ceo, issuer, recipient, 1), (recipient, issuer, ceo, 1), (ceo, issuer, issuer, 1), (ceo, issuer, -1, 1), (ceo, issuer, recipient, 0), (ceo, issuer, recipient, -1)] {
        let mut effects = Outbox::<()>::default();
        let id = ledger::operation_id(&tx).await.unwrap();
        assert!(transfer_core(&tx, &mut effects, id, actor, bound, issuer, target, qty, true, 1).await.is_err());
        assert!(effects.into_entries().is_empty());
    }
    assert_eq!(ledger::balance(&tx, issuer, issuer).await.unwrap(), 0);
    assert_eq!(ledger::balance(&tx, issuer, recipient).await.unwrap(), 1000);
    tx.rollback().await.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn inconsistent_null_total_fails_closed_even_for_zero_holding() {
    let mut client = fixtures::connect().await;
    let tx = client.transaction().await.unwrap();
    let ceo = fixtures::character(&tx).await;
    let issuer = fixtures::corporation(&tx, ceo).await;
    tx.execute("INSERT INTO game.corp_shares (corporation_id,shareholder_id,shares) VALUES ($1,$1,0)", &[&issuer]).await.unwrap();
    assert!(!ledger::initialize(&tx, issuer, 1000).await.unwrap());
    assert!(ledger::initialize_existing(&tx, 1000).await.unwrap().is_none());
    assert_eq!(ledger::issued_total(&tx, issuer).await.unwrap(), None);
    tx.rollback().await.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn backfill_only_once_and_personal_transfers_are_distinct_operations() {
    let mut client = fixtures::connect().await;
    let tx = client.transaction().await.unwrap();
    let ceo = fixtures::character(&tx).await;
    let holder = fixtures::character(&tx).await;
    let target = fixtures::character(&tx).await;
    let issuer = fixtures::corporation(&tx, ceo).await;
    assert!(ledger::initialize_existing(&tx, 1000).await.unwrap().is_some());
    assert_eq!(ledger::issued_total(&tx, issuer).await.unwrap(), Some(1000));
    let op = ledger::operation_id(&tx).await.unwrap();
    let mut effects = Outbox::<()>::default();
    transfer_core(&tx, &mut effects, op, ceo, issuer, issuer, holder, 1000, true, 1).await.unwrap();
    assert!(ledger::initialize_existing(&tx, 2000).await.unwrap().is_some());
    assert_eq!(ledger::issued_total(&tx, issuer).await.unwrap(), Some(1000));
    assert_eq!(ledger::balance(&tx, issuer, issuer).await.unwrap(), 0);
    for _ in 0..2 {
        let mut effects = Outbox::<()>::default();
        let op = ledger::operation_id(&tx).await.unwrap();
        transfer_core(&tx, &mut effects, op, holder, 1, issuer, target, 1, false, 1).await.unwrap();
        assert!(!effects.into_entries().is_empty());
    }
    assert_eq!(ledger::balance(&tx, issuer, holder).await.unwrap(), 998);
    assert_eq!(ledger::balance(&tx, issuer, target).await.unwrap(), 2);
    tx.rollback().await.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn retry_after_claim_commits_once_and_failure_never_dispatches() {
    use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};
    use crate::{db::Db, services::{CallError, tests::fixture}};
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let ceo = fixture::create(&db).await.unwrap();
    let target = fixture::create(&db).await.unwrap();
    let issuer: i64 = db.query_one("INSERT INTO game.corporations (name,ticker,ceo_id,creator_id,station_id,created) VALUES ('governance-retry-' || nextval('game.corporations_corporation_id_seq')::text,'GR' || currval('game.corporations_corporation_id_seq')::text,$1,$1,1,1) RETURNING corporation_id", &[&ceo.character]).await.unwrap().get("corporation_id");
    fixture::assign_corporation(&db, ceo.character, issuer, infinity_core::consts::ROLE_DIRECTOR).await.unwrap();
    assert!(ledger::initialize(&db, issuer, 1000).await.unwrap());
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let op = ledger::operation_id(&test_db).await.unwrap();
        let attempts = Arc::new(AtomicUsize::new(0));
        let observed = attempts.clone();
        let effects = test_db.transaction(|tx, outbox: &mut Outbox<()>| {
            let n = attempts.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move {
                transfer_core(tx, outbox, op, ceo.character, issuer, issuer, target.character, 10, true, 1).await?;
                if n == 0 {
                    tx.execute("DO $$ BEGIN RAISE EXCEPTION 'retry fixture' USING ERRCODE = '40001'; END $$", &[]).await.map_err(crate::db::Error::from)?;
                }
                Ok::<_, CallError>(())
            })
        }, |(), outbox| async move { Ok(outbox.into_entries().len()) }).await.unwrap();
        assert_eq!(observed.load(Ordering::SeqCst), 2);
        assert!(effects > 0);
        assert_eq!(ledger::balance(&test_db, issuer, target.character).await.unwrap(), 10);
        let failed_op = ledger::operation_id(&test_db).await.unwrap();
        let dispatched = Arc::new(AtomicUsize::new(0));
        let dispatch_count = dispatched.clone();
        let result: Result<(), CallError> = test_db.transaction(|tx, outbox: &mut Outbox<()>| Box::pin(async move {
            transfer_core(tx, outbox, failed_op, ceo.character, issuer, issuer, target.character, 10, true, 1).await?;
            Err(CallError::Internal(anyhow::anyhow!("rollback fixture")))
        }), |(), _| async move { dispatch_count.fetch_add(1, Ordering::SeqCst); Ok(()) }).await;
        assert!(result.is_err());
        assert_eq!(dispatched.load(Ordering::SeqCst), 0);
        assert_eq!(ledger::balance(&test_db, issuer, target.character).await.unwrap(), 10);
        let receipts: i64 = test_db.query_one("SELECT count(*) AS count FROM game.corp_share_transfers WHERE operation_id=$1", &[&failed_op]).await.unwrap().get("count");
        assert_eq!(receipts, 0);
    }).await;
    db.execute("DELETE FROM game.corp_share_transfers WHERE corporation_id=$1", &[&issuer]).await.unwrap();
    db.execute("DELETE FROM game.corp_shares WHERE corporation_id=$1", &[&issuer]).await.unwrap();
    db.execute("UPDATE game.characters SET corporation_id=1 WHERE character_id=$1", &[&ceo.character]).await.unwrap();
    db.execute("DELETE FROM game.corporations WHERE corporation_id=$1", &[&issuer]).await.unwrap();
    fixture::cleanup(&db, ceo).await.unwrap();
    fixture::cleanup(&db, target).await.unwrap();
    result.unwrap();
}

