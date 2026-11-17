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

async fn npc(f: &Fixture, taxable: bool) -> Result<(crate::space::Ball, crate::isk::IskAmount)> {
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
    Ok((ball, crate::isk::IskAmount::from_isk(crate::services::market::round2(payout))?))
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

fn expected_notices(owner: i64, corp: i64, net: crate::isk::IskAmount, tax: crate::isk::IskAmount) -> Vec<Vec<Value>> {
    let mut notices = Vec::new();
    if !net.is_zero() { notices.push(vec![Value::str("cash"), Value::Int(owner), Value::Float(net.client_isk(crate::isk::personal_client_cap()))]); }
    if !tax.is_zero() { notices.push(vec![Value::str("cash"), Value::Int(corp), Value::Float(tax.client_isk(crate::isk::corporation_client_cap()))]); }
    notices
}

async fn batch_started(f: &Fixture) -> Result<i64> {
    Ok(f.db.query_one("SELECT batch_started_at FROM game.npc_bounty_pools WHERE character_id=$1 LIMIT 1", &[&f.owner]).await?.get("batch_started_at"))
}

async fn settle(f: &Fixture) -> Result<()> {
    f.world.flush_positions().await?;
    let started = batch_started(f).await?;
    crate::services::bounty::npc::settle_due(&f.db, &f.world, (f.owner, started), i64::MAX / 4).await.map_err(|e| anyhow!("settle: {e:?}"))
}

async fn split(f: &Fixture, corp: i64, receiver: &mut Notices) -> Result<()> {
    for taxable in [false, true] {
    let (ball, payout) = npc(f, taxable).await?;
    for rate in [0.0_f64, 0.1, 1.0] {
        f.db.execute("UPDATE game.corporations SET tax_rate=$2 WHERE corporation_id=$1", &[&corp, &rate]).await?;
        f.db.execute("UPDATE game.characters SET balance=0 WHERE character_id=$1", &[&f.owner]).await?;
        f.db.execute("UPDATE game.corp_wallets SET balance=0 WHERE corporation_id=$1 AND account_key=$2", &[&corp, &ACCOUNT_CASH]).await?;
        reward_kill(&f.db, &f.world, f.system, &ball, Some(f.owner)).await.map_err(|e| anyhow!("reward: {e:?}"))?;
        // The kill records the pool notice only; wallet and tax wait for the
        // batch deadline, and the durable checkpoint rides the same writer.
        let (name, _) = receiver.try_recv().map_err(|_| anyhow!("pool notice missing"))?;
        ensure!(name == "OnBountyAddedToPayout", "expected pool notice, got {name}");
        ensure!(wallet_notices(receiver).is_empty(), "wallet notified before the batch deadline");
        let (personal, corporate) = balances(f, corp).await?;
        ensure!(personal == "0.00" && corporate == "0.00", "wallet credited before the batch deadline: {personal}, {corporate}");
        settle(f).await?;
        let tax = if taxable { crate::isk::IskAmount::from_isk(crate::services::market::round2(payout.wire_isk() * rate))? } else { crate::isk::IskAmount::ZERO };
        let net = payout.checked_sub(tax).ok_or_else(|| anyhow!("tax exceeds payout"))?;
        let (personal, corporate) = balances(f, corp).await?;
        ensure!(personal == net.decimal() && corporate == tax.decimal(), "wrong split at rate {rate}: {personal}, {corporate}");
        ensure!(net.checked_add(tax) == Some(payout), "split does not add to payout");
        ensure!(wallet_notices(receiver) == expected_notices(f.owner, corp, net, tax), "wrong wallet notices at rate {rate}");
    }
    }
    Ok(())
}

async fn overflow(f: &Fixture, corp: i64, receiver: &mut Notices) -> Result<()> {
    let (ball, _) = npc(f, true).await?;
    f.db.execute("UPDATE game.corporations SET tax_rate=0.1 WHERE corporation_id=$1", &[&corp]).await?;
    f.db.execute("UPDATE game.corp_wallets SET balance='9999999999999999999999999999999999.99'::numeric WHERE corporation_id=$1 AND account_key=$2", &[&corp, &ACCOUNT_CASH]).await?;
    let before = balances(f, corp).await?;
    // Fixture NPC types have no SDE wreck mapping, so reward_kill returns before spawn_wreck.
    reward_kill(&f.db, &f.world, f.system, &ball, Some(f.owner)).await.map_err(|e| anyhow!("reward: {e:?}"))?;
    let (name, _) = receiver.try_recv().map_err(|_| anyhow!("pool notice missing"))?;
    ensure!(name == "OnBountyAddedToPayout", "expected pool notice, got {name}");
    settle(f).await.err().ok_or_else(|| anyhow!("corporate overflow unexpectedly succeeded"))?;
    ensure!(balances(f, corp).await? == before, "overflow did not roll back both balances");
    ensure!(receiver.try_recv().is_err(), "failed payout emitted a notice");
    ensure!(batch_started(f).await.is_ok(), "unpaid batch was deleted");
    Ok(())
}


async fn postcommit(f: &Fixture, corp: i64, receiver: &mut Notices) -> Result<()> {
    let (ball, payout) = npc(f, true).await?;
    let tax = crate::isk::IskAmount::from_isk(crate::services::market::round2(payout.wire_isk() * 0.1))?;
    let net = payout.checked_sub(tax).ok_or_else(|| anyhow!("tax exceeds payout"))?;
    f.db.execute("UPDATE game.corporations SET tax_rate=0.1 WHERE corporation_id=$1", &[&corp]).await?;
    f.db.execute("UPDATE game.characters SET balance=0 WHERE character_id=$1", &[&f.owner]).await?;
    reward_kill(&f.db, &f.world, f.system, &ball, Some(f.owner)).await.map_err(|e| anyhow!("reward: {e:?}"))?;
    let (name, _) = receiver.try_recv().map_err(|_| anyhow!("pool notice missing"))?;
    ensure!(name == "OnBountyAddedToPayout", "expected pool notice, got {name}");
    f.world.flush_positions().await?;
    let started = batch_started(f).await?;
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
    let owner = f.owner;
    let mut task = tokio::spawn(async move { crate::services::bounty::npc::settle_due(&db, &world, (owner, started), i64::MAX / 4).await });
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
            let row = observer.query_one("SELECT ch.balance::text AS personal, cw.balance::text AS corporate FROM game.characters ch JOIN game.corp_wallets cw ON cw.corporation_id=ch.corporation_id WHERE ch.character_id=$1 AND cw.account_key=$2", &[&owner, &ACCOUNT_CASH]).await?;
            ensure!(row.get::<_, String>("personal") == net.decimal() && row.get::<_, String>("corporate") == tax.decimal(), "notice arrived before both balances committed");
            ensure!(notice.1 == vec![Value::str("cash"), Value::Int(owner), Value::Float(net.client_isk(crate::isk::personal_client_cap()))], "unexpected first wallet notice");
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
    ensure!(wallet_notices(receiver) == vec![vec![Value::str("cash"), Value::Int(corp), Value::Float(tax.client_isk(crate::isk::corporation_client_cap()))]], "missing or duplicate corporate notice");
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

// Producer rounding is per kill: reward_kill converts each payout through
// round2(bounty_payout(..)) -> IskAmount, then integer cents accumulate. A zero
// multiplier still yields Some(0.0), so the kill reaches the security-count branch.
#[test]
fn npc_bounty_rounding_is_per_kill() -> Result<()> {
    let payout = |bounty: f64, multiplier: f64| -> Result<crate::isk::IskAmount> {
        let raw = bounty_payout(bounty, multiplier).ok_or_else(|| anyhow!("nonfinite payout"))?;
        Ok(crate::isk::IskAmount::from_isk(crate::services::market::round2(raw))?)
    };
    let two_kills = payout(0.006, 1.0)?.checked_add(payout(0.006, 1.0)?).ok_or_else(|| anyhow!("cents overflow"))?;
    ensure!(two_kills.decimal() == "0.02", "two 0.006 kills each round to one cent, got {}", two_kills.decimal());
    ensure!(payout(0.006 + 0.006, 1.0)?.decimal() == "0.01", "aggregating before rounding would pay one cent");
    ensure!(payout(1.0, 0.0)? == crate::isk::IskAmount::ZERO, "zero multiplier still yields a countable kill");
    Ok(())
}
