//! Cross-connection governance serialization and durable vote recovery.
use super::fixtures;
use infinity_core::{consts::{ACCOUNT_CASH, FILETIME_DAY}};
use crate::{dao::{corporations::{shares, votes as dao}, jobs}, db::{Db, outbox::Outbox}, services::corps::{dividends, shares as service_shares, vote_close, votes}};
use std::{sync::Arc, time::Duration};
use tokio_postgres::{Client, Transaction};

async fn blocked(tx: &Transaction<'_>, pid: i32) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let waiting: bool = tx.query_one("SELECT pg_backend_pid()=ANY(pg_blocking_pids($1)) AS waiting", &[&pid]).await.unwrap().get("waiting");
            if waiting { break; }
            tokio::task::yield_now().await;
        }
    }).await.expect("transaction did not reach the expected lock");
}

async fn cleanup(client: &Client, corps: &[i64], chars: &[i64]) {
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    fixtures::cleanup_votes(&db, corps).await.unwrap();
    client.execute("DELETE FROM game.corp_dividend_recipients WHERE operation_id IN (SELECT operation_id FROM game.corp_dividends WHERE corporation_id=ANY($1))", &[&corps]).await.unwrap();
    client.execute("DELETE FROM game.corp_dividends WHERE corporation_id=ANY($1)", &[&corps]).await.unwrap();
    client.execute("DELETE FROM game.corp_share_transfers WHERE corporation_id=ANY($1)", &[&corps]).await.unwrap();
    client.execute("DELETE FROM game.corp_shares WHERE corporation_id=ANY($1)", &[&corps]).await.unwrap();
    client.execute("DELETE FROM game.corp_wallets WHERE corporation_id=ANY($1)", &[&corps]).await.unwrap();
    client.execute("UPDATE game.characters SET corporation_id=1 WHERE character_id=ANY($1)", &[&chars]).await.unwrap();
    client.execute("DELETE FROM game.corporations WHERE corporation_id=ANY($1)", &[&corps]).await.unwrap();
    let accounts: Vec<i64> = client.query("SELECT user_id FROM game.characters WHERE character_id=ANY($1)", &[&chars]).await.unwrap().iter().map(|r| r.get("user_id")).collect();
    client.execute("DELETE FROM game.characters WHERE character_id=ANY($1)", &[&chars]).await.unwrap();
    client.execute("DELETE FROM game.accounts WHERE user_id=ANY($1)", &[&accounts]).await.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn competing_transfers_serialize_and_rollback_receipt() {
    let mut first = fixtures::connect().await;
    let mut second = fixtures::connect().await;
    let tx = first.transaction().await.unwrap();
    let ceo = fixtures::character(&tx).await;
    let a = fixtures::character(&tx).await;
    let b = fixtures::character(&tx).await;
    let issuer = fixtures::corporation(&tx, ceo).await;
    assert!(shares::initialize(&tx, issuer, 1000).await.unwrap());
    tx.commit().await.unwrap();
    // Direct cores bypass the process market mutex; the issuer row is the shared DB serialization point.
    let tx = first.transaction().await.unwrap();
    let op1 = shares::operation_id(&tx).await.unwrap();
    let mut out = Outbox::<()>::default();
    service_shares::transfer_core(&tx, &mut out, op1, ceo, issuer, issuer, a, 700, true, 1).await.unwrap();
    assert!(!out.into_entries().is_empty());
    let (send, recv) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        let tx = second.transaction().await.unwrap();
        let op = shares::operation_id(&tx).await.unwrap();
        send.send((tx.query_one("SELECT pg_backend_pid() AS pid", &[]).await.unwrap().get::<_, i32>("pid"), op)).unwrap();
        let mut out = Outbox::<()>::default();
        let result = service_shares::transfer_core(&tx, &mut out, op, ceo, issuer, issuer, b, 700, true, 1).await;
        assert!(result.is_err());
        assert!(out.into_entries().is_empty());
        tx.rollback().await.unwrap();
    });
    let (pid, op2) = recv.await.unwrap();
    blocked(&tx, pid).await;
    tx.commit().await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), task).await.unwrap().unwrap();
    let row = first.query_one("SELECT (SELECT count(*) FROM game.corp_share_transfers WHERE operation_id=ANY($1)) AS receipts, (SELECT COALESCE(sum(shares),0)::bigint FROM game.corp_shares WHERE corporation_id=$2) AS total, (SELECT min(shares) FROM game.corp_shares WHERE corporation_id=$2) AS minimum", &[&[op1, op2].as_slice(), &issuer]).await.unwrap();
    assert_eq!(row.get::<_, i64>("receipts"), 1);
    assert_eq!(row.get::<_, i64>("total"), 1000);
    assert!(row.get::<_, i64>("minimum") >= 0);
    let tx = first.transaction().await.unwrap();
    assert_eq!(shares::balance(&tx, issuer, issuer).await.unwrap(), 300);
    assert_eq!(shares::balance(&tx, issuer, a).await.unwrap(), 700);
    assert_eq!(shares::balance(&tx, issuer, b).await.unwrap(), 0);
    tx.commit().await.unwrap();
    cleanup(&first, &[issuer], &[ceo, a, b]).await;
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn waiting_operations_recheck_authority_after_issuer_lock() {
    let mut first = fixtures::connect().await;
    let mut second = fixtures::connect().await;
    let tx = first.transaction().await.unwrap();
    let ceo = fixtures::character(&tx).await;
    let replacement = fixtures::character(&tx).await;
    let recipient = fixtures::character(&tx).await;
    let issuer = fixtures::corporation(&tx, ceo).await;
    assert!(shares::initialize(&tx, issuer, 1000).await.unwrap());
    assert!(shares::transfer(&tx, issuer, issuer, recipient, 10).await.unwrap());
    tx.execute("INSERT INTO game.corp_wallets (corporation_id,account_key,balance) VALUES ($1,$2,100)", &[&issuer, &ACCOUNT_CASH]).await.unwrap();
    tx.commit().await.unwrap();
    let tx = first.transaction().await.unwrap();
    assert!(shares::lock_issuer(&tx, issuer).await.unwrap());
    let (send, recv) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        let tx = second.transaction().await.unwrap();
        let op = shares::operation_id(&tx).await.unwrap();
        send.send((tx.query_one("SELECT pg_backend_pid() AS pid", &[]).await.unwrap().get::<_, i32>("pid"), op)).unwrap();
        let mut out = Outbox::<()>::default();
        assert!(dividends::payout_core(&tx, &mut out, op, ceo, issuer, 10.0, 1).await.is_err());
        assert!(out.into_entries().is_empty());
        tx.rollback().await.unwrap();
    });
    let (pid, op) = recv.await.unwrap();
    blocked(&tx, pid).await;
    tx.execute("UPDATE game.corporations SET ceo_id=$2 WHERE corporation_id=$1", &[&issuer, &replacement]).await.unwrap();
    tx.execute("UPDATE game.characters SET corporation_id=$1,corp_roles=$3 WHERE character_id=$2", &[&issuer, &replacement, &infinity_core::consts::ROLE_DIRECTOR]).await.unwrap();
    tx.commit().await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), task).await.unwrap().unwrap();
    let row = first.query_one("SELECT (SELECT count(*) FROM game.corp_dividends WHERE operation_id=$1) AS receipts, (SELECT balance::text FROM game.corp_wallets WHERE corporation_id=$2 AND account_key=$3) AS balance", &[&op, &issuer, &ACCOUNT_CASH]).await.unwrap();
    assert_eq!(row.get::<_, i64>("receipts"), 0);
    assert_eq!(crate::isk::IskAmount::parse_decimal(&row.get::<_, String>("balance")).unwrap(), crate::isk::IskAmount::from_cents(10_000).unwrap());
    let mut second = fixtures::connect().await;
    let tx = first.transaction().await.unwrap();
    assert!(crate::services::corps::permissions::is_active_ceo(&tx, replacement, issuer).await.unwrap());
    assert!(shares::lock_issuer(&tx, issuer).await.unwrap());
    let (send, recv) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        let tx = second.transaction().await.unwrap();
        let op = shares::operation_id(&tx).await.unwrap();
        send.send((tx.query_one("SELECT pg_backend_pid() AS pid", &[]).await.unwrap().get::<_, i32>("pid"), op)).unwrap();
        let mut out = Outbox::<()>::default();
        assert!(service_shares::transfer_core(&tx, &mut out, op, replacement, issuer, issuer, recipient, 1, true, 1).await.is_err());
        assert!(out.into_entries().is_empty());
        tx.rollback().await.unwrap();
    });
    let (pid, op) = recv.await.unwrap();
    blocked(&tx, pid).await;
    tx.execute("UPDATE game.characters SET corporation_id=1 WHERE character_id=$1", &[&replacement]).await.unwrap();
    tx.commit().await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), task).await.unwrap().unwrap();
    let count: i64 = first.query_one("SELECT count(*) AS count FROM game.corp_share_transfers WHERE operation_id=$1", &[&op]).await.unwrap().get("count");
    assert_eq!(count, 0);
    let tx = first.transaction().await.unwrap();
    assert_eq!(shares::balance(&tx, issuer, issuer).await.unwrap(), 990);
    tx.commit().await.unwrap();
    cleanup(&first, &[issuer], &[ceo, replacement, recipient]).await;
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn lost_holder_authority_rolls_back_ballot_and_prior_claims() {
    let mut first = fixtures::connect().await;
    let tx = first.transaction().await.unwrap();
    let ceo = fixtures::character(&tx).await;
    let holder = fixtures::character(&tx).await;
    let replacement = fixtures::character(&tx).await;
    let issuer = fixtures::corporation(&tx, ceo).await;
    let foreign = fixtures::corporation(&tx, holder).await;
    assert!(shares::initialize(&tx, issuer, 1000).await.unwrap());
    assert!(shares::transfer(&tx, issuer, issuer, holder, 10).await.unwrap());
    assert!(shares::transfer(&tx, issuer, issuer, foreign, 20).await.unwrap());
    assert!(shares::transfer(&tx, issuer, issuer, replacement, 5).await.unwrap());
    let start = crate::client::filetime_now();
    let id = fixtures::general_vote_fixture(&tx, issuer, ceo, start, start + FILETIME_DAY).await.unwrap();
    tx.commit().await.unwrap();
    let tx = first.transaction().await.unwrap();
    assert!(shares::lock_issuer(&tx, foreign).await.unwrap());
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let (mut voter, _peer) = crate::services::tests::client(db, holder).await;
    let (send, recv) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        send.send(()).unwrap();
        fixtures::vote_call(&mut voter, "InsertVote", &[marshal::Value::Int(issuer), marshal::Value::Int(id), marshal::Value::Int(0)]).await
    });
    recv.await.unwrap();
    let blocker: i32 = tx.query_one("SELECT pg_backend_pid() AS pid", &[]).await.unwrap().get("pid");
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let waiting: bool = tx.query_one("SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE $1=ANY(pg_blocking_pids(pid))) AS waiting", &[&blocker]).await.unwrap().get("waiting");
            if waiting { break; }
            tokio::task::yield_now().await;
        }
    }).await.expect("vote admission did not wait");
    tx.execute("UPDATE game.corporations SET ceo_id=$2 WHERE corporation_id=$1", &[&foreign, &replacement]).await.unwrap();
    tx.commit().await.unwrap();
    assert!(tokio::time::timeout(Duration::from_secs(10), task).await.unwrap().unwrap().is_err());
    let count: i64 = first.query_one("SELECT count(*) AS count FROM game.corp_vote_claims WHERE case_id=$1", &[&id]).await.unwrap().get("count");
    assert_eq!(count, 0);
    let tx = first.transaction().await.unwrap();
    assert!(dao::ballot(&tx, id, holder).await.unwrap().is_none());
    assert!(crate::dao::corporations::registry::lock_admission(&tx, &[], &[issuer, foreign], &[holder, replacement]).await.unwrap());
    let mut out = Outbox::<()>::default();
    votes::cast_inner(&tx, &mut out, replacement, issuer, id, 0, start + 1).await.unwrap();
    assert_eq!(dao::ballot(&tx, id, replacement).await.unwrap().unwrap().weight, 25);
    tx.execute("UPDATE game.corporations SET ceo_id=$2 WHERE corporation_id=$1", &[&foreign, &holder]).await.unwrap();
    let mut out = Outbox::<()>::default();
    votes::cast_inner(&tx, &mut out, holder, issuer, id, 1, start + 1).await.unwrap();
    assert_eq!(dao::ballot(&tx, id, holder).await.unwrap().unwrap().weight, 10);
    tx.commit().await.unwrap();
    cleanup(&first, &[issuer, foreign], &[ceo, holder, replacement]).await;
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn deadline_cast_serializes_with_durable_recovered_close() {
    let mut first = fixtures::connect().await;
    let tx = first.transaction().await.unwrap();
    let ceo = fixtures::character(&tx).await;
    let holder = fixtures::character(&tx).await;
    let late = fixtures::character(&tx).await;
    let issuer = fixtures::corporation(&tx, ceo).await;
    assert!(shares::initialize(&tx, issuer, 1000).await.unwrap());
    assert!(shares::transfer(&tx, issuer, issuer, holder, 30).await.unwrap());
    assert!(shares::transfer(&tx, issuer, issuer, late, 7).await.unwrap());
    let id = fixtures::general_vote_fixture(&tx, issuer, ceo, 1, 1 + FILETIME_DAY).await.unwrap();
    tx.commit().await.unwrap();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let deadline_tx = first.transaction().await.unwrap();
    assert!(crate::dao::corporations::registry::lock_admission(&deadline_tx, &[], &[issuer], &[late]).await.unwrap());
    let mut deadline_out = Outbox::<()>::default();
    assert!(votes::cast_inner(&deadline_tx, &mut deadline_out, late, issuer, id, 1, 1 + FILETIME_DAY).await.is_err());
    assert!(deadline_out.into_entries().is_empty());
    deadline_tx.rollback().await.unwrap();
    let job_id = format!("{}:{id}", vote_close::KIND);
    jobs::cancel(&db, &job_id).await.unwrap();
    vote_close::seed(&db).await.unwrap();
    assert_eq!(jobs::lock_job(&db, &job_id).await.unwrap().unwrap().due_at, 1 + FILETIME_DAY);
    // Exclude all unrelated jobs; this test never claims another fixture's work.
    let others: Vec<String> = first.query("SELECT job_id FROM game.jobs WHERE job_id<>$1", &[&job_id]).await.unwrap().iter().map(|r| r.get("job_id")).collect();
    let excluded: Vec<&str> = others.iter().map(String::as_str).collect();
    let mut claims = jobs::claim(&db, 1 + FILETIME_DAY, 2 + FILETIME_DAY, 1, &excluded).await.unwrap();
    assert_eq!(claims.len(), 1);
    let abandoned = claims.pop().unwrap();
    vote_close::seed(&db).await.unwrap();
    vote_close::seed(&db).await.unwrap();
    let lease = db.query_one("SELECT lease_token,lease_until,attempt FROM game.jobs WHERE job_id=$1", &[&job_id]).await.unwrap();
    assert_eq!((lease.get::<_, Option<i64>>("lease_token"), lease.get::<_, Option<i64>>("lease_until"), lease.get::<_, i32>("attempt")), (Some(abandoned.token), Some(2 + FILETIME_DAY), abandoned.attempt));
    assert_eq!(abandoned.job.id, job_id);
    let mut reclaimed = jobs::claim(&db, 3 + FILETIME_DAY, 4 + FILETIME_DAY, 1, &excluded).await.unwrap();
    assert_eq!(reclaimed.len(), 1);
    let claim = reclaimed.pop().unwrap();
    assert_eq!(claim.job.id, job_id);
    assert_ne!(claim.token, abandoned.token);
    assert_eq!(claim.attempt, abandoned.attempt + 1);
    assert_eq!(jobs::ack(&db, &job_id, abandoned.token).await.unwrap(), 0);
    let world = crate::space::World::new(db.clone(), Arc::new(crate::sde::Sde::load(&db).await.unwrap()), 0);
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    world.register_pilot(ceo, sender);
    let tx = first.transaction().await.unwrap();
    assert!(crate::dao::corporations::registry::lock_admission(&tx, &[], &[issuer], &[holder]).await.unwrap());
    let mut out = Outbox::<()>::default();
    votes::cast_inner(&tx, &mut out, holder, issuer, id, 0, FILETIME_DAY).await.unwrap();
    let blocker: i32 = tx.query_one("SELECT pg_backend_pid() AS pid", &[]).await.unwrap().get("pid");
    let handler_db = db.clone();
    let handler_world = world.clone();
    let handler_job = claim.job.clone();
    let task = tokio::spawn(async move { vote_close::handle(handler_db, handler_world, handler_job).await });
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let waiting: bool = tx.query_one("SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE $1=ANY(pg_blocking_pids(pid))) AS waiting", &[&blocker]).await.unwrap().get("waiting");
            if waiting { break; }
            tokio::task::yield_now().await;
        }
    }).await.expect("close handler did not wait for ballot");
    tx.commit().await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), task).await.unwrap().unwrap().unwrap();
    assert!(receiver.try_recv().is_ok());
    vote_close::handle(db.clone(), world.clone(), claim.job).await.unwrap();
    assert!(receiver.try_recv().is_err());
    assert_eq!(jobs::ack(&db, &job_id, claim.token).await.unwrap(), 1);
    assert!(dao::case(&db, id).await.unwrap().unwrap().closed_at.is_some());
    assert!(dao::ballot(&db, id, late).await.unwrap().is_none());
    let options = dao::options(&db, id).await.unwrap();
    assert_eq!((options[0].votes_for, options[1].votes_for), (30, 0));
    let tx = first.transaction().await.unwrap();
    assert!(crate::dao::corporations::registry::lock_admission(&tx, &[], &[issuer], &[late]).await.unwrap());
    let mut late_out = Outbox::<()>::default();
    assert!(votes::cast_inner(&tx, &mut late_out, late, issuer, id, 1, 1 + FILETIME_DAY).await.is_err());
    assert!(late_out.into_entries().is_empty());
    let mut out = Outbox::<()>::default();
    vote_close::close_inner(&tx, world.sde(), &mut out, id, 1 + FILETIME_DAY).await.unwrap();
    assert!(out.into_entries().is_empty());
    tx.commit().await.unwrap();
    cleanup(&first, &[issuer], &[ceo, holder, late]).await;
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn expiry_seed_recovers_original_deadline_and_is_idempotent() {
    use crate::services::corps::{sanctions, vote_request};
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let world = crate::space::World::new(db.clone(), Arc::new(crate::sde::Sde::load(&db).await.unwrap()), 0);
    let mut first = fixtures::connect().await;
    let tx = first.transaction().await.unwrap();
    let ceo = fixtures::character(&tx).await;
    let issuer = fixtures::corporation(&tx, ceo).await;
    shares::initialize(&tx, issuer, 1000).await.unwrap();
    shares::transfer(&tx, issuer, issuer, ceo, 100).await.unwrap();
    let window = crate::config::game().corporations.vote_action_window_secs * infinity_core::consts::FILETIME_SECOND;
    let end = crate::client::filetime_now() - window - FILETIME_DAY;
    let id = fixtures::general_vote_fixture(&tx, issuer, ceo, end - FILETIME_DAY, end).await.unwrap();
    tx.execute("UPDATE game.corp_vote_cases SET vote_type=$2 WHERE id=$1", &[&id, &vote_request::VOTE_SHARES]).await.unwrap();
    assert!(crate::dao::corporations::registry::lock_admission(&tx, &[], &[issuer], &[ceo]).await.unwrap());
    let mut out = Outbox::<()>::default();
    votes::cast_inner(&tx, &mut out, ceo, issuer, id, 0, end - 1).await.unwrap();
    vote_close::close_inner(&tx, world.sde(), &mut out, id, end + 1).await.unwrap();
    tx.commit().await.unwrap();
    let job_id = format!("{}:{id}", sanctions::KIND);
    assert_eq!(crate::dao::corporations::sanctions::get(&db, id).await.unwrap().unwrap().expires, end + window);
    jobs::cancel(&db, &job_id).await.unwrap();
    sanctions::seed(&db).await.unwrap();
    assert_eq!(jobs::lock_job(&db, &job_id).await.unwrap().unwrap().due_at, end + window);
    let others: Vec<String> = first.query("SELECT job_id FROM game.jobs WHERE job_id<>$1", &[&job_id]).await.unwrap().iter().map(|r| r.get("job_id")).collect();
    let excluded: Vec<&str> = others.iter().map(String::as_str).collect();
    let now = crate::client::filetime_now();
    let claim = jobs::claim(&db, now, now + FILETIME_DAY, 1, &excluded).await.unwrap().pop().unwrap();
    assert_eq!(claim.job.id, job_id);
    sanctions::seed(&db).await.unwrap();
    sanctions::seed(&db).await.unwrap();
    let lease = db.query_one("SELECT lease_token,lease_until,attempt FROM game.jobs WHERE job_id=$1", &[&job_id]).await.unwrap();
    assert_eq!((lease.get::<_, Option<i64>>("lease_token"), lease.get::<_, Option<i64>>("lease_until"), lease.get::<_, i32>("attempt")), (Some(claim.token), Some(now + FILETIME_DAY), claim.attempt));
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    world.register_pilot(ceo, sender);
    sanctions::handle(db.clone(), world.clone(), claim.job.clone()).await.unwrap();
    assert!(receiver.try_recv().is_ok());
    let expired = crate::dao::corporations::sanctions::get(&db, id).await.unwrap().unwrap().expired_at;
    assert!(expired.is_some());
    sanctions::handle(db.clone(), world.clone(), claim.job).await.unwrap();
    assert!(receiver.try_recv().is_err());
    assert_eq!(crate::dao::corporations::sanctions::get(&db, id).await.unwrap().unwrap().expired_at, expired);
    assert_eq!(jobs::ack(&db, &job_id, claim.token).await.unwrap(), 1);
    world.unregister_pilot_generation(ceo, None);
    cleanup(&first, &[issuer], &[ceo]).await;
}
