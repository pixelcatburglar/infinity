//! Real NPC bounty wallet transactions and client wallet notices.
//! Mirrors walletSvc's OnAccountChange routing using game characters and corporation wallets.
//! Requires an isolated database with real SDE data; Lua event delivery is tested separately.

use super::*;
use anyhow::{anyhow, ensure, Result};
use infinity_core::consts::{ACCOUNT_CASH, CATEGORY_ENTITY, ROLE_ACCOUNTANT, dogma::ATTR_ENTITY_KILL_BOUNTY};
use crate::services::drones::tests::fixture::Fixture;
use marshal::Value;
use std::time::Duration;
use tokio::sync::mpsc::{self, UnboundedReceiver};

const WAIT: Duration = Duration::from_secs(30);
type Notices = UnboundedReceiver<(String, Vec<Value>)>;

enum Case { Split, Overflow, Postcommit }

async fn run_case(case: Case) -> Result<()> {
    let f = Fixture::new().await;
    let (original, original_roles) = match f.db.query_one("SELECT corporation_id,corp_roles FROM game.characters WHERE character_id=$1", &[&f.owner]).await {
        Ok(row) => (row.get::<_, i64>("corporation_id"), row.get::<_, i64>("corp_roles")),
        Err(error) => { f.cleanup().await; return Err(error.into()); }
    };
    let corp = match f.db.query_one(
        "WITH id AS (SELECT nextval('game.corporations_corporation_id_seq') AS n) INSERT INTO game.corporations (corporation_id,name,ticker,creator_id,ceo_id,station_id,created) SELECT n,'reward-test-' || n::text,'R' || n::text,$1,$1,$2,0 FROM id RETURNING corporation_id",
        &[&f.owner, &f.system],
    ).await {
        Ok(row) => row.get::<_, i64>("corporation_id"),
        Err(error) => { f.cleanup().await; return Err(error.into()); }
    };
    let (sender, mut receiver) = mpsc::unbounded_channel();
    f.world.register_pilot(f.owner, sender);
    let result = async {
        ensure!(corp >= 98_000_000, "corporation sequence is outside player range");
        f.db.execute("UPDATE game.characters SET corporation_id=$2,corp_roles=$3 WHERE character_id=$1", &[&f.owner, &corp, &(original_roles | ROLE_ACCOUNTANT)]).await?;
        f.db.execute("INSERT INTO game.corp_wallets (corporation_id,account_key,balance) VALUES ($1,$2,0)", &[&corp, &ACCOUNT_CASH]).await?;
        match case {
            Case::Split => split(&f, corp, &mut receiver).await,
            Case::Overflow => overflow(&f, corp, &mut receiver).await,
            Case::Postcommit => postcommit(&f, corp, &mut receiver).await,
        }
    }.await;
    let restore = f.db.execute("UPDATE game.characters SET corporation_id=$2,corp_roles=$3 WHERE character_id=$1", &[&f.owner, &original, &original_roles]).await;
    let wallet_cleanup = f.db.execute("DELETE FROM game.corp_wallets WHERE corporation_id=$1", &[&corp]).await;
    let corp_cleanup: Result<u64, crate::db::Error> = f.db.transaction(|tx, _: &mut ()| Box::pin(async move {
        crate::services::tests::fixture::cleanup_progress(tx, &[corp], &[]).await?;
        Ok(tx.execute("DELETE FROM game.corporations WHERE corporation_id=$1", &[&corp]).await?)
    }), |value, ()| async move { Ok(value) }).await;
    f.cleanup().await;
    restore?;
    wallet_cleanup?;
    corp_cleanup?;
    result
}

async fn npc(f: &Fixture, taxable: bool) -> Result<(crate::space::Ball, f64)> {
    let multiplier = crate::config::game().combat.bounty_multiplier;
    let (kind, payout) = f.sde.types.iter().filter(|t| t.category_id == Some(CATEGORY_ENTITY))
        .filter_map(|t| {
            let bounty = t.attribute(ATTR_ENTITY_KILL_BOUNTY)?;
            let payout = bounty_payout(bounty, multiplier)?;
            (bounty > 0.0 && payout >= 1.0 && (payout > 100_000.0) == taxable && payout.is_finite()
                && payout == crate::services::market::round2(payout)
                && crate::services::market::round2(payout * 0.1) > 0.0).then_some((t.type_id, payout))
        }).next().ok_or_else(|| anyhow!("real SDE needs a positive NPC bounty in the requested tax range and positive multiplier"))?;
    let mut ball = f.world.ball(f.system, f.ship).await.ok_or_else(|| anyhow!("fixture ship missing"))?;
    ball.type_id = kind;
    ball.char_id = None;
    Ok((ball, payout))
}

async fn balances(f: &Fixture, corp: i64) -> Result<(String, String)> {
    let personal = f.db.query_one("SELECT balance::text AS balance FROM game.characters WHERE character_id=$1", &[&f.owner]).await?.get("balance");
    let corporate = f.db.query_one("SELECT balance::text AS balance FROM game.corp_wallets WHERE corporation_id=$1 AND account_key=$2", &[&corp, &ACCOUNT_CASH]).await?.get("balance");
    Ok((personal, corporate))
}

fn wallet_notices(receiver: &mut Notices) -> Vec<Vec<Value>> {
    let mut notices = Vec::new();
    while let Ok((name, args)) = receiver.try_recv() {
        if name == "OnAccountChange" { notices.push(args); }
    }
    notices
}

fn expected_notices(owner: i64, corp: i64, personal: f64, corporate: f64, net: f64, tax: f64) -> Vec<Vec<Value>> {
    let mut notices = Vec::new();
    if net > 0.0 { notices.push(vec![Value::str("cash"), Value::Int(owner), Value::Float(personal)]); }
    if tax > 0.0 { notices.push(vec![Value::str("cash"), Value::Int(corp), Value::Float(corporate)]); }
    notices
}

async fn split(f: &Fixture, corp: i64, receiver: &mut Notices) -> Result<()> {
    for taxable in [false, true] {
    let (ball, payout) = npc(f, taxable).await?;
    for rate in [0.0_f64, 0.1, 1.0] {
        f.db.execute("UPDATE game.corporations SET tax_rate=$2 WHERE corporation_id=$1", &[&corp, &rate]).await?;
        f.db.execute("UPDATE game.characters SET balance=0 WHERE character_id=$1", &[&f.owner]).await?;
        f.db.execute("UPDATE game.corp_wallets SET balance=0 WHERE corporation_id=$1 AND account_key=$2", &[&corp, &ACCOUNT_CASH]).await?;
        reward_kill(&f.db, &f.world, f.system, &ball, Some(f.owner)).await.map_err(|e| anyhow!("reward: {e:?}"))?;
        let tax = if taxable { crate::services::market::round2(payout * rate) } else { 0.0 };
        let net = payout - tax;
        let expected_personal = crate::services::market::round2(net);
        let expected_corp = crate::services::market::round2(tax);
        let (personal, corporate) = balances(f, corp).await?;
        ensure!(personal == format!("{expected_personal:.2}") && corporate == format!("{expected_corp:.2}"), "wrong split at rate {rate}: {personal}, {corporate}");
        ensure!((expected_personal + expected_corp - crate::services::market::round2(payout)).abs() < 0.001, "split does not add to payout");
        ensure!(wallet_notices(receiver) == expected_notices(f.owner, corp, expected_personal, expected_corp, net, tax), "wrong wallet notices at rate {rate}");
    }
    }
    Ok(())
}

async fn overflow(f: &Fixture, corp: i64, receiver: &mut Notices) -> Result<()> {
    let (ball, _) = npc(f, true).await?;
    f.db.execute("UPDATE game.corporations SET tax_rate=0.1 WHERE corporation_id=$1", &[&corp]).await?;
    f.db.execute("UPDATE game.corp_wallets SET balance='9999999999999999999999999999999999.99'::numeric WHERE corporation_id=$1 AND account_key=$2", &[&corp, &ACCOUNT_CASH]).await?;
    let before = balances(f, corp).await?;
    let items: i64 = f.db.query_one("SELECT count(*) AS n FROM game.items WHERE owner_id=$1", &[&f.owner]).await?.get("n");
    let error = reward_kill(&f.db, &f.world, f.system, &ball, Some(f.owner)).await.err().ok_or_else(|| anyhow!("corporate overflow unexpectedly succeeded"))?;
    match &error {
        CallError::User { msg, dict } => {
            assert_eq!(msg, infinity_core::messages::MSG_CUSTOM_NOTIFY);
            assert_eq!(dict, &vec![("notify".into(), Value::unicode(infinity_core::messages::wallets::AMOUNT_TOO_LARGE))]);
        }
        CallError::Internal(error) => panic!("expected wallet overflow refusal, got {error:#}"),
    }
    ensure!(balances(f, corp).await? == before, "overflow did not roll back both balances");
    ensure!(receiver.try_recv().is_err(), "failed reward emitted a notice");
    let after: i64 = f.db.query_one("SELECT count(*) AS n FROM game.items WHERE owner_id=$1", &[&f.owner]).await?.get("n");
    ensure!(after == items, "failed reward created items");
    Ok(())
}

async fn postcommit(f: &Fixture, corp: i64, receiver: &mut Notices) -> Result<()> {
    let (ball, payout) = npc(f, true).await?;
    f.db.execute("UPDATE game.corporations SET tax_rate=0.1 WHERE corporation_id=$1", &[&corp]).await?;
    f.db.execute("UPDATE game.characters SET balance=0 WHERE character_id=$1", &[&f.owner]).await?;
    let config = crate::config::tests::test_database().to_pg_config()?;
    let (observer, observer_connection) = config.connect(tokio_postgres::NoTls).await?;
    let observer_driver = tokio::spawn(observer_connection);
    let (mut blocker, blocker_connection) = config.connect(tokio_postgres::NoTls).await?;
    let blocker_driver = tokio::spawn(blocker_connection);
    let pid: i32 = blocker.query_one("SELECT pg_backend_pid() AS pid", &[]).await?.get("pid");
    let tx = blocker.transaction().await?;
    tx.query_one("SELECT balance FROM game.corp_wallets WHERE corporation_id=$1 AND account_key=$2 FOR UPDATE", &[&corp, &ACCOUNT_CASH]).await?;
    let db = f.db.clone();
    let world = f.world.clone();
    let (system, owner) = (f.system, f.owner);
    let mut task = tokio::spawn(async move { reward_kill(&db, &world, system, &ball, Some(owner)).await });
    let blocked = tokio::time::timeout(WAIT, async {
        loop {
            let found: bool = observer.query_one("SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE $1 = ANY(pg_blocking_pids(pid))) AS blocked", &[&pid]).await?.get("blocked");
            if found { break; }
            tokio::task::yield_now().await;
        }
        let old: String = observer.query_one("SELECT balance::text AS balance FROM game.characters WHERE character_id=$1", &[&owner]).await?.get("balance");
        ensure!(old == "0.00", "personal credit visible before commit: {old}");
        ensure!(receiver.try_recv().is_err(), "notice delivered before commit");
        Ok::<(), anyhow::Error>(())
    }).await.map_err(|_| anyhow!("reward did not block on corporation wallet")).and_then(|r| r);
    let release = tx.rollback().await;
    let observed = if blocked.is_ok() && release.is_ok() {
        async {
            let notice = tokio::time::timeout(WAIT, receiver.recv()).await?.ok_or_else(|| anyhow!("wallet receiver closed"))?;
            ensure!(notice.0 == "OnAccountChange", "first reward notice was not a wallet notice");
            let tax = crate::services::market::round2(payout * 0.1);
            let personal = crate::services::market::round2(payout - tax);
            let row = observer.query_one("SELECT ch.balance::text AS personal, cw.balance::text AS corporate FROM game.characters ch JOIN game.corp_wallets cw ON cw.corporation_id=ch.corporation_id WHERE ch.character_id=$1 AND cw.account_key=$2", &[&owner, &ACCOUNT_CASH]).await?;
            ensure!(row.get::<_, String>("personal") == format!("{personal:.2}") && row.get::<_, String>("corporate") == format!("{tax:.2}"), "notice arrived before both balances committed");
            ensure!(notice.1 == vec![Value::str("cash"), Value::Int(owner), Value::Float(personal)], "unexpected first wallet notice");
            Ok::<(), anyhow::Error>(())
        }.await
    } else { Ok(()) };
    let completion = match tokio::time::timeout(WAIT, &mut task).await {
        Ok(joined) => joined.map_err(|e| anyhow!("reward task: {e}")).and_then(|result| result.map_err(|e| anyhow!("reward: {e:?}"))),
        Err(_) => { task.abort(); let _ = task.await; Err(anyhow!("reward did not finish after releasing wallet")) }
    };
    drop(observer);
    drop(blocker);
    let _ = observer_driver.await;
    let _ = blocker_driver.await;
    blocked?;
    release?;
    observed?;
    completion?;
    let tax = crate::services::market::round2(payout * 0.1);
    ensure!(wallet_notices(receiver) == vec![vec![Value::str("cash"), Value::Int(corp), Value::Float(tax)]], "missing or duplicate corporate notice");
    Ok(())
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE"]
async fn npc_reward_split_routes_wallet_notices() -> Result<()> { run_case(Case::Split).await }

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE"]
async fn npc_reward_corporate_overflow_rolls_back() -> Result<()> { run_case(Case::Overflow).await }

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE"]
async fn npc_reward_notices_follow_commit() -> Result<()> { run_case(Case::Postcommit).await }
