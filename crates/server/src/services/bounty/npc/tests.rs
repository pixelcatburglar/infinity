//! NPC bounty batch payout: durable capture, per-source journals, exact tax
//! seams, cancellation/unknown reconciliation, restart and offline recovery,
//! and stable aggregate security gain.
use super::*;
use anyhow::{anyhow, ensure, Result};
use infinity_core::consts::{ACCOUNT_CASH, OWNER_DED, REF_BOUNTY_PRIZES, REF_CORPORATION_TAX_NPC_BOUNTIES, ROLE_ACCOUNTANT};
use crate::db::executor::Executor;
use crate::dao::npc_bounties::Pool;
use crate::isk::IskAmount;
use crate::services::drones::tests::fixture::Fixture;
use std::time::Duration;

const FUTURE: i64 = i64::MAX / 4;
const WAIT: Duration = Duration::from_secs(30);

async fn kill(f: &Fixture, system: i64, character: i64, cents: i128, at: i64) -> Result<()> {
    f.world.with(system, move |s| s.add_npc_bounty(character, 42, IskAmount::from_cents(cents).unwrap(), crate::npc_security::FrozenMultiplier::new(1.0).unwrap(), at)).await
        .ok_or_else(|| anyhow!("system actor missing"))?
        .map_err(|e| anyhow!("bounty record: {e:?}"))
}

async fn rows(f: &Fixture, character: i64) -> Result<Vec<Pool>> {
    Ok(crate::dao::npc_bounties::for_character(&f.db, character).await?)
}

async fn started(f: &Fixture, character: i64) -> Result<i64> {
    Ok(rows(f, character).await?.first().map(|row| row.batch.started_at).ok_or_else(|| anyhow!("no bounty batch"))?)
}

fn cents(v: i128) -> IskAmount { IskAmount::from_cents(v).unwrap() }

async fn corp_fixture(f: &Fixture, rate: f64) -> Result<i64> {
    let corp: i64 = f.db.query_one(
        "WITH id AS (SELECT nextval('game.corporations_corporation_id_seq') AS n) INSERT INTO game.corporations (corporation_id,name,ticker,creator_id,ceo_id,station_id,created) SELECT n,'npc-bounty-' || n::text,'N' || n::text,$1,$1,$2,0 FROM id RETURNING corporation_id",
        &[&f.owner, &f.system]).await?.get("corporation_id");
    ensure!(corp >= 98_000_000, "corporation sequence is outside player range");
    f.db.execute("INSERT INTO game.corp_wallets (corporation_id,account_key,balance) VALUES ($1,$2,0)", &[&corp, &ACCOUNT_CASH]).await?;
    f.db.execute("UPDATE game.corporations SET tax_rate=$2 WHERE corporation_id=$1", &[&corp, &rate]).await?;
    f.db.execute("UPDATE game.characters SET corporation_id=$2,corp_roles=corp_roles|$3 WHERE character_id=$1", &[&f.owner, &corp, &ROLE_ACCOUNTANT]).await?;
    Ok(corp)
}

async fn cleanup_corp(f: &Fixture, corp: i64, original: i64) {
    let _ = f.db.execute("UPDATE game.characters SET corporation_id=$2 WHERE character_id=$1", &[&f.owner, &original]).await;
    let _ = f.db.execute("DELETE FROM game.wallet_journal WHERE account_id IN (SELECT account_id FROM game.wallet_accounts WHERE owner_id=$1)", &[&corp]).await;
    let _ = f.db.execute("DELETE FROM game.wallet_accounts WHERE owner_id=$1", &[&corp]).await;
    let _ = f.db.execute("DELETE FROM game.corp_wallets WHERE corporation_id=$1", &[&corp]).await;
    let _ = f.db.execute("DELETE FROM game.corporations WHERE corporation_id=$1", &[&corp]).await;
}

async fn journal(f: &Fixture, owner: i64) -> Result<Vec<(i32, i64, i64, i64, String, i16)>> {
    Ok(f.db.query(
        "SELECT j.entry_type_id, j.owner_id1, j.owner_id2, j.reference_id, j.amount::text AS amount, j.side
         FROM game.wallet_journal j JOIN game.wallet_accounts a ON a.account_id=j.account_id WHERE a.owner_id=$1 ORDER BY j.journal_id",
        &[&owner]).await?.iter().map(|r| (r.get("entry_type_id"), r.get("owner_id1"), r.get("owner_id2"), r.get("reference_id"), r.get("amount"), r.get("side"))).collect())
}

async fn corp_journal(f: &Fixture, corp: i64) -> Result<Vec<(i32, i64, String)>> {
    Ok(f.db.query(
        "SELECT j.entry_type_id, j.owner_id2, j.amount::text AS amount
         FROM game.wallet_journal j JOIN game.wallet_accounts a ON a.account_id=j.account_id WHERE a.owner_id=$1 ORDER BY j.journal_id",
        &[&corp]).await?.iter().map(|r| (r.get("entry_type_id"), r.get("owner_id2"), r.get("amount"))).collect())
}

async fn cleanup_pools(f: &Fixture, owner: i64) {
    let _ = f.db.execute("DELETE FROM game.npc_bounty_pools WHERE character_id=$1", &[&owner]).await;
    let _ = f.db.execute("DELETE FROM game.jobs WHERE job_id LIKE 'npc-bounty:' || $1::text || ':%'", &[&owner.to_string()]).await;
    let _ = f.db.execute("DELETE FROM game.wallet_journal WHERE account_id IN (SELECT account_id FROM game.wallet_accounts WHERE owner_id=$1)", &[&owner]).await;
    let _ = f.db.execute("DELETE FROM game.wallet_accounts WHERE owner_id=$1", &[&owner]).await;
    let _ = f.db.execute("UPDATE game.characters SET balance=0,security_status=0 WHERE character_id=$1", &[&owner]).await;
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE"]
async fn npc_payout_final_capture_and_source_journals() -> Result<()> {
    let f = Fixture::real_system().await;
    let other = f.sde.map.systems().find(|s| s.solar_system_id != f.system)
        .map(|s| s.solar_system_id).ok_or_else(|| anyhow!("need two SDE systems"))?;
    let result = async {
        // A stale durable row smaller than live is overwritten absolutely by
        ensure!(f.sde.accounting.iter().any(|e| e.id == REF_BOUNTY_PRIZES)
            && f.sde.accounting.iter().any(|e| e.id == REF_CORPORATION_TAX_NPC_BOUNTIES),
            "NPC bounty journal entry types missing from the SDE catalogue");
        // the final capture, never summed.
        let t0 = crate::client::filetime_now();
        f.db.execute("INSERT INTO game.npc_bounty_pools (character_id,system_id,batch_started_at,due_at,amount,kill_count) VALUES ($1,$2,$3,$4,'1.00',0)",
            &[&f.owner, &f.system, &t0, &(t0 + 12_000_000_000_i64)]).await?;
        kill(&f, f.system, f.owner, 1000, t0).await?;
        f.world.flush_positions().await?;
        let pools = rows(&f, f.owner).await?;
        ensure!(pools.len() == 1 && pools[0].amount == cents(1000), "live absolute must overwrite durable, got {pools:?}");
        f.world.system(other).await?;
        kill(&f, f.system, f.owner, 500, t0 + 1).await?;
        kill(&f, other, f.owner, 250, t0 + 2).await?;
        f.world.flush_positions().await?;
        let pools = rows(&f, f.owner).await?;
        ensure!(pools.len() == 2, "per-source rows missing: {pools:?}");
        for row in &pools {
            if row.system_id == f.system { ensure!(row.amount == cents(1500) && row.kill_count == 2, "wrong source row {pools:?}"); }
            else { ensure!(row.system_id == other && row.amount == cents(250) && row.kill_count == 1, "wrong source row {pools:?}"); }
        }
        let started = started(&f, f.owner).await?;
        let balance: String = f.db.query_one("SELECT balance::text AS balance FROM game.characters WHERE character_id=$1", &[&f.owner]).await?.get("balance");
        ensure!(balance == "0.00", "wallet credited before the batch deadline: {balance}");
        settle_due(&f.db, &f.world, (f.owner, started), FUTURE).await.map_err(|e| anyhow!("settle: {e:?}"))?;
        ensure!(rows(&f, f.owner).await?.is_empty(), "paid rows remain");
        let balance: String = f.db.query_one("SELECT balance::text AS balance FROM game.characters WHERE character_id=$1", &[&f.owner]).await?.get("balance");
        ensure!(balance == "17.50", "expected full net without tax, got {balance}");
        let entries = journal(&f, f.owner).await?;
        let mut systems = [f.system, other];
        systems.sort_unstable();
        ensure!(entries.len() == 2, "expected one posting per source system, got {entries:?}");
        for (entry_type, owner1, owner2, _reference, _amount, side) in &entries {
            ensure!(*entry_type == REF_BOUNTY_PRIZES && *owner1 == OWNER_DED && *owner2 == f.owner && *side == 1, "wrong posting shape {entries:?}");
        }
        for (index, system) in systems.iter().enumerate() {
            let expected = if *system == f.system { "15.00" } else { "2.50" };
            ensure!(entries[index].3 == *system && entries[index].4 == expected, "wrong source posting {entries:?}");
        }
        let security: f64 = f.db.query_one("SELECT security_status FROM game.characters WHERE character_id=$1", &[&f.owner]).await?.get("security_status");
        ensure!(security > 0.0, "batch security settlement missing: {security}");
        settle_due(&f.db, &f.world, (f.owner, started), FUTURE).await.map_err(|e| anyhow!("settle: {e:?}"))?;
        let balance: String = f.db.query_one("SELECT balance::text AS balance FROM game.characters WHERE character_id=$1", &[&f.owner]).await?.get("balance");
        ensure!(balance == "17.50", "job replay double-credited: {balance}");
        Ok::<(), anyhow::Error>(())
    }.await;
    let owner = f.owner;
    let name = format!("npc_gain_fault_{owner}");
    let function = format!("game.{name}_fn");
    let table = format!("game.{name}_updates");
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    f.world.register_pilot(owner, tx);
    let fault = crate::tests::catch_scenario(async {
        f.db.uncached().execute(&format!("CREATE TABLE IF NOT EXISTS {table} (character_id bigint PRIMARY KEY, updates bigint NOT NULL)"), &[]).await.unwrap();
        f.db.uncached().execute(&format!("CREATE OR REPLACE FUNCTION {function}() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF TG_OP = 'DELETE' AND OLD.character_id = {owner} THEN RAISE EXCEPTION 'fixture delete fault'; END IF; IF TG_OP = 'UPDATE' AND NEW.character_id = {owner} THEN INSERT INTO {table} VALUES (NEW.character_id, 1) ON CONFLICT (character_id) DO UPDATE SET updates = {table}.updates + 1; END IF; IF TG_OP = 'DELETE' THEN RETURN OLD; ELSE RETURN NEW; END IF; END $$"), &[]).await.unwrap();
        f.db.uncached().execute(&format!("DROP TRIGGER IF EXISTS {name}_count ON game.characters"), &[]).await.unwrap();
        f.db.uncached().execute(&format!("CREATE TRIGGER {name}_count AFTER UPDATE OF security_status ON game.characters FOR EACH ROW EXECUTE FUNCTION {function}()"), &[]).await.unwrap();
        f.db.uncached().execute(&format!("DROP TRIGGER IF EXISTS {name}_delete ON game.npc_bounty_pools"), &[]).await.unwrap();
        f.db.uncached().execute(&format!("CREATE TRIGGER {name}_delete BEFORE DELETE ON game.npc_bounty_pools FOR EACH ROW EXECUTE FUNCTION {function}()"), &[]).await.unwrap();
        kill(&f, f.system, owner, 100, crate::client::filetime_now()).await.unwrap();
        f.world.flush_positions().await.unwrap();
        while rx.try_recv().is_ok() {}
        let started = started(&f, owner).await.unwrap();
        let before_rows = rows(&f, owner).await.unwrap();
        let before_balance: String = f.db.query_one("SELECT balance::text AS balance FROM game.characters WHERE character_id=$1", &[&owner]).await.unwrap().get("balance");
        let before_security: f64 = f.db.query_one("SELECT security_status FROM game.characters WHERE character_id=$1", &[&owner]).await.unwrap().get("security_status");
        let before_journal = journal(&f, owner).await.unwrap();
        let mut security_notices = 0_i64;
        let take_security = |rx: &mut tokio::sync::mpsc::UnboundedReceiver<(String, Vec<marshal::Value>)>, security_notices: &mut i64| while let Ok((name, _)) = rx.try_recv() { if name == "OnSecurityStatusUpdate" { *security_notices += 1; } };
        assert!(settle_due(&f.db, &f.world, (owner, started), FUTURE).await.is_err());
        take_security(&mut rx, &mut security_notices);
        assert_eq!(security_notices, 0);
        assert_eq!((rows(&f, owner).await.unwrap(), journal(&f, owner).await.unwrap()), (before_rows, before_journal));
        let balance: String = f.db.query_one("SELECT balance::text AS balance FROM game.characters WHERE character_id=$1", &[&owner]).await.unwrap().get("balance");
        let security: f64 = f.db.query_one("SELECT security_status FROM game.characters WHERE character_id=$1", &[&owner]).await.unwrap().get("security_status");
        assert_eq!((balance, security), (before_balance, before_security));
        f.db.uncached().execute(&format!("DROP TRIGGER IF EXISTS {name}_delete ON game.npc_bounty_pools"), &[]).await.unwrap();
        settle_due(&f.db, &f.world, (owner, started), FUTURE).await.unwrap();
        take_security(&mut rx, &mut security_notices);
        assert_eq!(security_notices, 1);
        settle_due(&f.db, &f.world, (owner, started), FUTURE).await.unwrap();
        take_security(&mut rx, &mut security_notices);
        assert_eq!(security_notices, 1);
        let updates: i64 = f.db.query_one(&format!("SELECT updates FROM {table} WHERE character_id=$1"), &[&owner]).await.unwrap().get("updates");
        assert_eq!(updates, 1);
    }).await;
    f.db.uncached().execute(&format!("DROP TRIGGER IF EXISTS {name}_count ON game.characters"), &[]).await.unwrap();
    f.db.uncached().execute(&format!("DROP TRIGGER IF EXISTS {name}_delete ON game.npc_bounty_pools"), &[]).await.unwrap();
    f.db.uncached().execute(&format!("DROP FUNCTION IF EXISTS {function}()"), &[]).await.unwrap();
    f.db.uncached().execute(&format!("DROP TABLE IF EXISTS {table}"), &[]).await.unwrap();
    cleanup_pools(&f, f.owner).await;
    f.cleanup().await;
    result.and_then(|value| fault.map(|()| value).map_err(|panic| anyhow!("security fault: {panic:?}")))
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE"]
async fn npc_bounty_restart_offline_and_reverse_system_load() -> Result<()> {
    let f = Fixture::real_system().await;
    let other = f.sde.map.systems().find(|s| s.solar_system_id != f.system)
        .map(|s| s.solar_system_id).ok_or_else(|| anyhow!("need two SDE systems"))?;
    let (world, db) = (f.world.clone(), f.db.clone());
    let result = async {
        let t0 = crate::client::filetime_now();
        kill(&f, f.system, f.owner, 1000, t0).await?;
        f.world.flush_positions().await?;
        let started = started(&f, f.owner).await?;
        // A fresh process recovers the batch metadata at first cold load; a
        // kill in a reverse-loaded system joins the same deadline.
        let world2 = crate::space::World::new(db.clone(), f.sde.clone(), 1);
        world2.system(other).await?;
        world2.with(other, move |s| s.add_npc_bounty(f.owner, 42, cents(250), crate::npc_security::FrozenMultiplier::new(1.0).unwrap(), t0 + 1)).await
            .ok_or_else(|| anyhow!("system actor missing"))?.map_err(|e| anyhow!("bounty record: {e:?}"))?;
        world2.flush_positions().await?;
        let pools = rows(&f, f.owner).await?;
        ensure!(pools.len() == 2 && pools.iter().all(|row| row.batch.started_at == started), "reverse-loaded system lost the original deadline: {pools:?}");
        // The offline payout handler settles without loading any system.
        let world3 = crate::space::World::new(db.clone(), f.sde.clone(), 1);
        settle_due(&db, &world3, (f.owner, started), FUTURE).await.map_err(|e| anyhow!("settle: {e:?}"))?;
        let balance: String = db.query_one("SELECT balance::text AS balance FROM game.characters WHERE character_id=$1", &[&f.owner]).await?.get("balance");
        ensure!(balance == "12.50", "offline settlement paid wrong amount: {balance}");
        ensure!(rows(&f, f.owner).await?.is_empty(), "paid rows remain");
        // An empty stale job clears nothing and pays nothing.
        settle_due(&db, &world3, (f.owner, started), FUTURE).await.map_err(|e| anyhow!("settle: {e:?}"))?;
        let balance: String = db.query_one("SELECT balance::text AS balance FROM game.characters WHERE character_id=$1", &[&f.owner]).await?.get("balance");
        ensure!(balance == "12.50", "stale job re-credited: {balance}");
        let _ = world;
        Ok::<(), anyhow::Error>(())
    }.await;
    cleanup_pools(&f, f.owner).await;
    f.cleanup().await;
    result
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE"]
async fn npc_payout_cancel_preserves_owner_and_new_kills() -> Result<()> {
    let f = Fixture::real_system().await;
    let original: i64 = f.db.query_one("SELECT corporation_id FROM game.characters WHERE character_id=$1", &[&f.owner]).await?.get("corporation_id");
    let mut owned_corp = None;
    let result = async {
        let corp = corp_fixture(&f, 0.1).await?;
        owned_corp = Some(corp);
        let t0 = crate::client::filetime_now();
        kill(&f, f.system, f.owner, 12_000_000, t0).await?;
        f.world.flush_positions().await?;
        let started = started(&f, f.owner).await?;
        let config = crate::config::tests::test_database().to_pg_config()?;
        let (observer, observer_connection) = config.connect(tokio_postgres::NoTls).await?;
        let observer_driver = tokio::spawn(observer_connection);
        let (mut blocker, blocker_connection) = config.connect(tokio_postgres::NoTls).await?;
        let blocker_driver = tokio::spawn(blocker_connection);
        let pid: i32 = blocker.query_one("SELECT pg_backend_pid() AS pid", &[]).await?.get("pid");
        let tx = blocker.transaction().await?;
        tx.query_one("SELECT balance FROM game.corp_wallets WHERE corporation_id=$1 AND account_key=$2 FOR UPDATE", &[&corp, &ACCOUNT_CASH]).await?;
        let (db, world) = (f.db.clone(), f.world.clone());
        let task = tokio::spawn(async move { settle_due(&db, &world, (f.owner, started), FUTURE).await });
        // The armed economy transaction blocks on the corporation wallet row;
        // dropping its future cancels the attempt while the owner survives.
        tokio::time::timeout(WAIT, async {
            loop {
                let found: bool = observer.query_one("SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE $1 = ANY(pg_blocking_pids(pid))) AS blocked", &[&pid]).await?.get("blocked");
                if found { break; }
                tokio::task::yield_now().await;
            }
            Ok::<(), anyhow::Error>(())
        }).await.map_err(|_| anyhow!("payment did not block on corporation wallet"))??;
        task.abort();
        let _ = task.await;
        let release = tx.rollback().await;
        release?;
        // A kill during the frozen window keeps its own time and lands pending.
        kill(&f, f.system, f.owner, 6_000_000, t0 + 1).await?;
        let before = rows(&f, f.owner).await?;
        // The armed owner reconciles: the intact preimage proves Unpaid and the
        // pending kill merges back under the unchanged deadline.
        let first = settle_due(&f.db, &f.world, (f.owner, started), FUTURE).await;
        ensure!(first.is_err(), "unpaid reconciliation should return to the scheduler");
        f.world.flush_positions().await?;
        let merged = rows(&f, f.owner).await?;
        let rate = crate::config::game().crimewatch.security_gain_npc_kill;
        let expected_gain = before[0].security_gain.merge(crate::npc_security::SecurityGain::for_kill(rate, crate::npc_security::FrozenMultiplier::new(1.0).unwrap()));
        ensure!(merged.len() == 1 && merged[0].kill_count == before[0].kill_count + 1 && merged[0].security_gain == expected_gain, "pending gain was not merged once: {merged:?}");
        let second = settle_due(&f.db, &f.world, (f.owner, started), FUTURE).await;
        second.map_err(|e| anyhow!("settle after cancel: {e:?}"))?;
        ensure!(rows(&f, f.owner).await?.is_empty(), "paid rows remain");
        let balance: String = f.db.query_one("SELECT balance::text AS balance FROM game.characters WHERE character_id=$1", &[&f.owner]).await?.get("balance");
        ensure!(balance == "162000.00", "cancel path paid wrong amount: {balance}");
        let security: f64 = f.db.query_one("SELECT security_status FROM game.characters WHERE character_id=$1", &[&f.owner]).await?.get("security_status");
        let expected_security = 10.0 * expected_gain.fraction();
        ensure!((security - expected_security).abs() <= 1e-9, "merged gain was not paid once: {security}");
        let corporate: String = f.db.query_one("SELECT balance::text AS balance FROM game.corp_wallets WHERE corporation_id=$1 AND account_key=$2", &[&corp, &ACCOUNT_CASH]).await?.get("balance");
        ensure!(corporate == "18000.00", "corporation tax wrong: {corporate}");
        ensure!(journal(&f, f.owner).await?.len() == 1, "cancel path duplicated journals");
        drop(observer);
        drop(blocker);
        let _ = observer_driver.await;
        let _ = blocker_driver.await;
        Ok::<(), anyhow::Error>(())
    }.await;
    if let Some(corp) = owned_corp { cleanup_corp(&f, corp, original).await; }
    cleanup_pools(&f, f.owner).await;
    f.cleanup().await;
    result
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE"]
async fn npc_unarmed_row_contradiction_releases_gate_and_keeps_frozen_owner() -> Result<()> {
    let f = Fixture::real_system().await;
    let started = crate::client::filetime_now();
    let wrong = started + 1;
    let result = async {
        let due = started + 12_000_000_000_i64;
        f.db.execute(
            "INSERT INTO game.npc_bounty_pools
             (character_id,system_id,batch_started_at,due_at,amount,kill_count)
             VALUES ($1,$2,$3,$4,'10.00',1)",
            &[&f.owner, &f.system, &started, &due]).await?;
        let _ = settle_due(&f.db, &f.world, (f.owner, started), started).await;
        let shared = f.world.bounties.character(f.owner);
        let batch = Batch { character_id: f.owner, started_at: started, due_at: due };
        let payout = Arc::new(Payout::new(batch, vec![f.system]));
        {
            let mut meta = lock(shared.meta.lock());
            meta.batch = Some(batch);
            meta.sources.insert(f.system);
            meta.frozen = Some(payout.clone());
        }
        f.db.execute("UPDATE game.npc_bounty_pools SET batch_started_at=$2,due_at=$3 WHERE character_id=$1",
            &[&f.owner, &wrong, &due]).await?;
        let first = Payout::final_checkpoint(&payout, &f.db, &f.world, &shared).await;
        ensure!(first.is_err() && payout.phase() == Phase::Unknown && !payout.armed());
        ensure!(lock(shared.meta.lock()).frozen.as_ref().is_some_and(|p| Arc::ptr_eq(p, &payout)));
        ensure!(!rows(&f, f.owner).await?.is_empty(), "contradictory rows were cleared");
        let gate = tokio::time::timeout(WAIT, f.world.lock_condition_snapshot(f.system)).await
            .map_err(|_| anyhow!("payout retained the source gate"))??;
        drop(gate);
        f.db.execute("UPDATE game.npc_bounty_pools SET batch_started_at=$2 WHERE character_id=$1",
            &[&f.owner, &started]).await?;
        Payout::final_checkpoint(&payout, &f.db, &f.world, &shared).await.map_err(|e| anyhow!("final checkpoint: {e:?}"))?;
        ensure!(payout.phase() == Phase::Prepared, "complete rows did not prepare payout");
        lock(shared.meta.lock()).frozen = None;
        Ok::<(), anyhow::Error>(())
    }.await;
    cleanup_pools(&f, f.owner).await;
    f.cleanup().await;
    result
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE"]
async fn npc_payout_unknown_uses_complete_preimage() -> Result<()> {
    let f = Fixture::real_system().await;
    let other = f.sde.map.systems().find(|s| s.solar_system_id != f.system)
        .map(|s| s.solar_system_id).ok_or_else(|| anyhow!("need two SDE systems"))?;
    let original: i64 = f.db.query_one("SELECT corporation_id FROM game.characters WHERE character_id=$1", &[&f.owner]).await?.get("corporation_id");
    let result = async {
        let corp = corp_fixture(&f, 0.1).await?;
        let t0 = crate::client::filetime_now();
        kill(&f, f.system, f.owner, 11_000_000, t0).await?;
        f.world.system(other).await?;
        kill(&f, other, f.owner, 11_000_000, t0 + 1).await?;
        f.world.flush_positions().await?;
        let started = started(&f, f.owner).await?;
        let config = crate::config::tests::test_database().to_pg_config()?;
        let (observer, observer_connection) = config.connect(tokio_postgres::NoTls).await?;
        let observer_driver = tokio::spawn(observer_connection);
        let (mut blocker, blocker_connection) = config.connect(tokio_postgres::NoTls).await?;
        let blocker_driver = tokio::spawn(blocker_connection);
        let pid: i32 = blocker.query_one("SELECT pg_backend_pid() AS pid", &[]).await?.get("pid");
        let tx = blocker.transaction().await?;
        tx.query_one("SELECT balance FROM game.corp_wallets WHERE corporation_id=$1 AND account_key=$2 FOR UPDATE", &[&corp, &ACCOUNT_CASH]).await?;
        let (db, world) = (f.db.clone(), f.world.clone());
        let task = tokio::spawn(async move { settle_due(&db, &world, (f.owner, started), FUTURE).await });
        tokio::time::timeout(WAIT, async {
            loop {
                let found: bool = observer.query_one("SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE $1 = ANY(pg_blocking_pids(pid))) AS blocked", &[&pid]).await?.get("blocked");
                if found { break; }
                tokio::task::yield_now().await;
            }
            Ok::<(), anyhow::Error>(())
        }).await.map_err(|_| anyhow!("payment did not block on corporation wallet"))??;
        let preimage = rows(&f, f.owner).await?;
        // While the armed transaction waits, one preimage row disappears:
        // delete_batch then fails its exact count, everything rolls back and
        // the leftover set is neither Paid nor Unpaid.
        f.db.execute("DELETE FROM game.npc_bounty_pools WHERE character_id=$1 AND system_id=$2", &[&f.owner, &f.system.min(other)]).await?;
        let release = tx.rollback().await;
        release?;
        let outcome = task.await.map_err(|e| anyhow!("settle task: {e}"))?;
        ensure!(outcome.is_err(), "contradictory row set must not succeed");
        let second = settle_due(&f.db, &f.world, (f.owner, started), FUTURE).await;
        ensure!(second.is_err(), "unknown outcome must stay for the scheduler");
        let remaining = rows(&f, f.owner).await?;
        ensure!(remaining.len() == 1, "rollback wrote or deleted rows: {remaining:?}");
        let balance: String = f.db.query_one("SELECT balance::text AS balance FROM game.characters WHERE character_id=$1", &[&f.owner]).await?.get("balance");
        ensure!(balance == "0.00", "unknown outcome credited a wallet: {balance}");
        drop(observer);
        drop(blocker);
        let _ = observer_driver.await;
        let _ = blocker_driver.await;
        // Restoring the exact saved preimage row lets the armed owner prove
        // Unpaid: the thaw releases the frozen owner and keeps the complete
        // row set for the scheduler's ordinary retry.
        let shared = f.world.bounties.lookup(f.owner).ok_or_else(|| anyhow!("payout owner missing"))?;
        let frozen = lock(shared.meta.lock()).frozen.clone().ok_or_else(|| anyhow!("frozen owner retired while unknown"))?;
        let missing = preimage.iter().find(|row| row.system_id == f.system.min(other)).ok_or_else(|| anyhow!("saved preimage incomplete"))?;
        crate::dao::npc_bounties::save_absolute(&f.db, std::slice::from_ref(missing)).await?;
        f.db.execute("UPDATE game.npc_bounty_pools SET security_gain_log=0.25 WHERE character_id=$1 AND system_id=$2", &[&f.owner, &missing.system_id]).await?;
        let gain_only = settle_due(&f.db, &f.world, (f.owner, started), FUTURE).await;
        ensure!(gain_only.is_err() && frozen.phase() == Phase::Unknown, "gain-only preimage difference was accepted");
        f.db.execute("UPDATE game.npc_bounty_pools SET security_gain_log=$3 WHERE character_id=$1 AND system_id=$2", &[&f.owner, &missing.system_id, &missing.security_gain.storage()]).await?;
        let third = settle_due(&f.db, &f.world, (f.owner, started), FUTURE).await;
        ensure!(third.is_err(), "unpaid classification must return to the scheduler");
        ensure!(frozen.phase() == Phase::Unpaid, "restored preimage did not classify Unpaid");
        ensure!(lock(shared.meta.lock()).frozen.is_none(), "unpaid thaw kept the frozen owner");
        ensure!(rows(&f, f.owner).await? == preimage, "unpaid batch lost preimage rows");
        cleanup_corp(&f, corp, original).await;
        Ok::<(), anyhow::Error>(())
    }.await;
    cleanup_pools(&f, f.owner).await;
    f.cleanup().await;
    result
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE"]
async fn npc_tax_is_current_per_source_aggregate() -> Result<()> {
    let f = Fixture::real_system().await;
    let other = f.sde.map.systems().find(|s| s.solar_system_id != f.system)
        .map(|s| s.solar_system_id).ok_or_else(|| anyhow!("need two SDE systems"))?;
    let original: i64 = f.db.query_one("SELECT corporation_id FROM game.characters WHERE character_id=$1", &[&f.owner]).await?.get("corporation_id");
    let result = async {
        let corp = corp_fixture(&f, 0.1).await?;
        let t0 = crate::client::filetime_now();
        // Two kills aggregating 120_000 in one system are taxed; the same
        // gross split across two source systems stays exempt; exactly 100_000
        // is exempt.
        kill(&f, f.system, f.owner, 6_000_000, t0).await?;
        kill(&f, f.system, f.owner, 6_000_000, t0 + 1).await?;
        f.world.system(other).await?;
        kill(&f, other, f.owner, 6_000_000, t0 + 2).await?;
        f.world.flush_positions().await?;
        let first_started = started(&f, f.owner).await?;
        settle_due(&f.db, &f.world, (f.owner, first_started), FUTURE).await.map_err(|e| anyhow!("settle: {e:?}"))?;
        let corporate: String = f.db.query_one("SELECT balance::text AS balance FROM game.corp_wallets WHERE corporation_id=$1 AND account_key=$2", &[&corp, &ACCOUNT_CASH]).await?.get("balance");
        ensure!(corporate == "12000.00", "per-source tax aggregate wrong: {corporate}");
        let balance: String = f.db.query_one("SELECT balance::text AS balance FROM game.characters WHERE character_id=$1", &[&f.owner]).await?.get("balance");
        ensure!(balance == "168000.00", "net split wrong: {balance}");
        let corp_entries = corp_journal(&f, corp).await?;
        ensure!(corp_entries.len() == 1 && corp_entries[0].0 == REF_CORPORATION_TAX_NPC_BOUNTIES && corp_entries[0].1 == corp && corp_entries[0].2 == "12000.00", "wrong corp tax posting {corp_entries:?}");
        // The tax comes from current payout-time membership and rate.
        f.db.execute("UPDATE game.corporations SET tax_rate=1.0 WHERE corporation_id=$1", &[&corp]).await?;
        kill(&f, f.system, f.owner, 11_000_000, t0 + 3).await?;
        f.world.flush_positions().await?;
        let started = started(&f, f.owner).await?;
        settle_due(&f.db, &f.world, (f.owner, started), FUTURE).await.map_err(|e| anyhow!("settle: {e:?}"))?;
        let corporate: String = f.db.query_one("SELECT balance::text AS balance FROM game.corp_wallets WHERE corporation_id=$1 AND account_key=$2", &[&corp, &ACCOUNT_CASH]).await?.get("balance");
        ensure!(corporate == "122000.00", "current-rate tax wrong: {corporate}");
        // A 100% tax still posts the zero-net personal journal row.
        let personal_entries = journal(&f, f.owner).await?;
        ensure!(personal_entries.iter().any(|(ty, _, _, _, amount, _)| *ty == REF_BOUNTY_PRIZES && amount == "0.00"), "zero-net posting swallowed: {personal_entries:?}");
        cleanup_corp(&f, corp, original).await;
        Ok::<(), anyhow::Error>(())
    }.await;
    cleanup_pools(&f, f.owner).await;
    f.cleanup().await;
    result
}


fn rate_config(rate: f64) -> crate::config::game::GameConfig {
    let mut config = crate::config::game::GameConfig::default();
    config.crimewatch.security_gain_npc_kill = rate;
    config
}

fn rate_identity_head(text: &str) -> Result<(i64, i64, i64)> {
    let mut lines = text.lines();
    let owner = lines.next().ok_or_else(|| anyhow!("rate identity missing owner"))?.parse()?;
    let account = lines.next().ok_or_else(|| anyhow!("rate identity missing account"))?.parse()?;
    let started = lines.next().ok_or_else(|| anyhow!("rate identity missing batch start"))?.parse()?;
    Ok((owner, account, started))
}

/// Remove this persisted batch and its payout job, then the shared owner rows.
/// A batch that drifted to another identity is reported and left in place.
async fn isolate_rate_owner(root: &std::path::Path) -> Result<()> {
    let path = root.join("identity");
    if !path.exists() { return Ok(()); }
    let (owner, account, started) = rate_identity_head(&std::fs::read_to_string(&path)?)?;
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await?;
    let pools = crate::dao::npc_bounties::for_character(&db, owner).await?;
    if let Some(pool) = pools.first() {
        ensure!(pools.len() == 1 && pool.batch.started_at == started, "rate isolation found a different batch for {owner}");
        crate::dao::npc_bounties::delete_batch(&db, pool.batch).await?;
    }
    crate::dao::jobs::cancel(&db, &super::job_id(owner, started)).await?;
    let fixture = crate::services::tests::fixture::Fixture { account, character: owner };
    let captured = crate::services::tests::fixture::capture_fixture_rows(&db, &[fixture], &[]).await?;
    crate::services::tests::fixture::cleanup(&db, fixture).await?;
    crate::services::tests::fixture::assert_fixture_removed(&db, &captured).await?;
    std::fs::remove_dir_all(root)?;
    Ok(())
}
#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE; run this exact test in its own process"]

async fn npc_gain_keeps_acceptance_rate_across_process_restart() -> Result<()> {
    const PHASE: &str = "INFINITY_NPC_GAIN_RATE_PHASE";
    const NAME: &str = "services::bounty::npc::tests::npc_gain_keeps_acceptance_rate_across_process_restart";
    let phase = std::env::var(PHASE).unwrap_or_default();
    if phase.is_empty() {
        let root = std::env::temp_dir().join(format!("npc-gain-rate-{}", std::process::id()));
        std::fs::create_dir_all(&root)?;
        let mut failed = None;
        let child = crate::tests::catch_scenario(async {
            for step in ["record", "settle"] {
                let status = std::process::Command::new(std::env::current_exe().expect("test executable"))
                    .args(["--exact", NAME, "--ignored", "--nocapture"]).env(PHASE, step)
                    .env("INFINITY_NPC_GAIN_RATE_ROOT", &root).status().expect("rate child process");
                if !status.success() { failed = Some(step); break; }
            }
        }).await;
        let mut isolated = Ok(());
        let cleanup = crate::tests::catch_scenario(async { isolated = isolate_rate_owner(&root).await; }).await;
        if let Err(cleanup_panic) = cleanup {
            eprintln!("rate child {failed:?}; cleanup panicked");
            std::panic::resume_unwind(child.err().unwrap_or(cleanup_panic));
        }
        match (child, isolated) {
            (Ok(()), Ok(())) => {
                ensure!(failed.is_none(), "{} process failed", failed.unwrap());
                return Ok(());
            }
            (Ok(()), Err(cleanup)) => return Err(anyhow!("rate child {failed:?} and cleanup failed: {cleanup:#}")),
            (Err(panic), Ok(())) => std::panic::resume_unwind(panic),
            (Err(panic), Err(cleanup)) => {
                eprintln!("rate cleanup failed: {cleanup:#}");
                std::panic::resume_unwind(panic);
            }
        }
    }
    let root = std::path::PathBuf::from(std::env::var("INFINITY_NPC_GAIN_RATE_ROOT")?);
    if phase == "settle" { crate::config::init_game(rate_config(0.2)).expect("settlement configuration"); }
    let database = if phase == "settle" {
        crate::config::tests::schema_downgrade_guard::database_config()
    } else {
        crate::config::tests::test_database()
    };
    let db = crate::db::Db::connect(&database).await?;
    if phase == "record" {
        let f = Fixture::real_system().await;
        let recorded = async {
            let t0 = crate::client::filetime_now();
            std::fs::write(root.join("identity"), format!("{}\n{}\n{}\n", f.owner, f.account, t0))?;
            db.execute("INSERT INTO game.npc_bounty_pools (character_id,system_id,batch_started_at,due_at,amount,kill_count,legacy_kill_count,security_gain_log) VALUES ($1,$2,$3,$4,'1.00',1,1,0)",
                &[&f.owner, &f.system, &t0, &(t0 + 12_000_000_000)]).await?;
            let world2 = crate::space::World::new(db.clone(), f.sde.clone(), 2);
            world2.system(f.system).await?;
            world2.with(f.system, move |s| s.add_npc_bounty(f.owner, 42, cents(100), crate::npc_security::FrozenMultiplier::new(1.0).unwrap(), t0 + 1)).await.unwrap().unwrap();
            world2.flush_positions().await?;
            let row = rows(&f, f.owner).await?.pop().ok_or_else(|| anyhow!("recorded pool missing"))?;
            ensure!(row.legacy_kill_count == 1 && row.kill_count == 2, "restored legacy row was replaced: {row:?}");
            ensure!(row.batch.started_at == t0, "recorded batch start drifted from the persisted identity");
            let accepted_rate = crate::config::game().crimewatch.security_gain_npc_kill;
            ensure!(accepted_rate != 0.2, "acceptance and settlement rates must differ");
            std::fs::write(root.join("identity"), format!("{}\n{}\n{}\n{}\n{}", f.owner, f.account, row.batch.started_at, row.security_gain.storage().unwrap(), f.system))?;
            Ok::<(), anyhow::Error>(())
        }.await;
        if recorded.is_err() { cleanup_pools(&f, f.owner).await; f.cleanup().await; }
        return recorded;
    }
    let text = std::fs::read_to_string(root.join("identity"))?;
    let (owner, _, started) = rate_identity_head(&text)?;
    let stored: f64 = text.lines().nth(3).unwrap().parse()?;
    let accepted = crate::npc_security::SecurityGain::from_storage(Some(stored)).unwrap();
    let settled = async {
        let world = crate::space::World::new(db.clone(), crate::tests::shared_sde(&db).await, 1);
        settle_due(&db, &world, (owner, started), FUTURE).await.map_err(|e| anyhow!("settle: {e:?}"))?;
        let security: f64 = db.query_one("SELECT security_status FROM game.characters WHERE character_id=$1", &[&owner]).await?.get("security_status");
        let expected = 10.0 * crate::npc_security::SecurityGain::for_legacy(0.2, 1).merge(accepted).fraction();
        ensure!((security - expected).abs() <= 1e-9, "mixed settlement repriced the accepted gain: {security} vs {expected}");
        Ok::<(), anyhow::Error>(())
    }.await;
    settled
}
