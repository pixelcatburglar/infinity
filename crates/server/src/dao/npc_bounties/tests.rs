use super::*;
use crate::db::Db;
use crate::isk::IskAmount;

fn pool(batch: Batch, system: i64, amount: &str, kills: i64) -> Pool {
    Pool { batch, system_id: system, amount: IskAmount::parse_decimal(amount).unwrap(), kill_count: kills, legacy_kill_count: 0, security_gain: crate::npc_security::SecurityGain::ZERO }
}

#[tokio::test]
#[ignore = "requires a dedicated PostgreSQL database"]
async fn npc_bounty_absolute_checkpoint_schema_and_rollback() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let seq: i64 = db.query_one("SELECT nextval('game.accounts_user_id_seq') AS id", &[]).await.unwrap().get("id");
    let account = crate::dao::accounts::authentication::register(&db, &format!("bounty-fixture-{seq}"), b"fixture", 0, 0).await.unwrap().user_id;
    let owner: i64 = db.query_one("INSERT INTO game.characters(user_id,name,race_id,bloodline_id,gender_id,corporation_id) VALUES ($1,$2,1,1,1,1) RETURNING character_id", &[&account, &format!("bounty-fixture-{seq}")]).await.unwrap().get("character_id");
    let batch = Batch { character_id: owner, started_at: 100, due_at: 1300 };
    // DDL guards: negative amount or count and a non-forward deadline refuse.
    for (amount, kills) in [(-1i64, 1i64), (1, -1)] {
        assert!(db.execute("INSERT INTO game.npc_bounty_pools (character_id,system_id,batch_started_at,due_at,amount,kill_count) VALUES ($1,1,100,200,$2,$3)", &[&owner, &amount, &kills]).await.is_err());
    }
    for (started, due) in [(200i64, 200i64), (0, 200)] {
        assert!(db.execute("INSERT INTO game.npc_bounty_pools (character_id,system_id,batch_started_at,due_at,amount,kill_count) VALUES ($1,1,$2,$3,1,1)", &[&owner, &started, &due]).await.is_err());
    }
    // Absolute write lands exact cents for every source row.
    save_absolute(&db, &[pool(batch, 2, "10.01", 2), pool(batch, 1, "0.01", 1)]).await.unwrap();
    assert_eq!(for_character(&db, owner).await.unwrap(), vec![pool(batch, 1, "0.01", 1), pool(batch, 2, "10.01", 2)]);
    assert_eq!(in_system(&db, 2).await.unwrap(), vec![pool(batch, 2, "10.01", 2)]);
    assert_eq!(batches(&db).await.unwrap().into_iter().filter(|b| b.character_id == owner).collect::<Vec<_>>(), vec![batch]);
    // An absolute rewrite replaces values; it never accumulates.
    save_absolute(&db, &[pool(batch, 2, "12.34", 3)]).await.unwrap();
    assert_eq!(for_character(&db, owner).await.unwrap()[1].amount.decimal(), "12.34");
    // A conflicting identity, a regressed count or an equal count with a
    // different amount each refuse; the stored row stays untouched.
    for rejected in [
        pool(Batch { character_id: owner, started_at: 101, due_at: 1300 }, 2, "20.00", 4),
        pool(batch, 2, "13.00", 3),
        pool(batch, 2, "12.34", 2),
    ] {
        assert!(save_absolute(&db, &[rejected]).await.is_err());
    }
    assert_eq!(for_character(&db, owner).await.unwrap()[1], pool(batch, 2, "12.34", 3));
    // A partial multi-row write fails inside the caller's transaction: the
    // conflicting row leaves the fresh row unwritten after rollback.
    let conflicting = pool(Batch { character_id: owner, started_at: 101, due_at: 1300 }, 2, "20.00", 4);
    let mixed = db.transaction(|tx, _: &mut ()| { let conflicting = conflicting.clone(); Box::pin(async move {
        save_absolute(tx, &[conflicting, pool(batch, 4, "5.00", 1)]).await?;
        Ok::<_, crate::db::Error>(())
    }) }, |value, ()| async move { Ok(value) }).await;
    assert!(mixed.is_err());
    assert!(for_character(&db, owner).await.unwrap().iter().all(|row| row.system_id != 4));
    assert_eq!(for_character(&db, owner).await.unwrap()[1], pool(batch, 2, "12.34", 3));
    let mut changed_gain = pool(batch, 2, "12.34", 3);
    changed_gain.security_gain = crate::npc_security::SecurityGain::for_kill(0.2, crate::npc_security::FrozenMultiplier::new(1.0).unwrap());
    let gain_only = db.transaction(|tx, _: &mut ()| { let changed_gain = changed_gain.clone(); Box::pin(async move {
        save_absolute(tx, &[changed_gain]).await?;
        Ok::<_, crate::db::Error>(())
    }) }, |value, ()| async move { Ok(value) }).await;
    assert!(gain_only.is_err());
    assert_eq!(for_character(&db, owner).await.unwrap()[1].security_gain, crate::npc_security::SecurityGain::ZERO);
    let mut changed_legacy = pool(batch, 2, "13.00", 4);
    changed_legacy.legacy_kill_count = 1;
    let legacy_only = db.transaction(|tx, _: &mut ()| { let changed_legacy = changed_legacy.clone(); Box::pin(async move {
        save_absolute(tx, &[changed_legacy]).await?;
        Ok::<_, crate::db::Error>(())
    }) }, |value, ()| async move { Ok(value) }).await;
    assert!(legacy_only.is_err());
    let mut full = pool(batch, 2, "12.34", 4);
    full.security_gain = crate::npc_security::SecurityGain::for_kill(1.0, crate::npc_security::FrozenMultiplier::new(1.0).unwrap());
    save_absolute(&db, std::slice::from_ref(&full)).await.unwrap();
    assert_eq!(for_character(&db, owner).await.unwrap()[1], full);
    let mut regressed = full.clone();
    regressed.kill_count = 5;
    regressed.security_gain = crate::npc_security::SecurityGain::for_kill(0.2, crate::npc_security::FrozenMultiplier::new(1.0).unwrap());
    let regression = db.transaction(|tx, _: &mut ()| { let regressed = regressed.clone(); Box::pin(async move {
        save_absolute(tx, &[regressed]).await?;
        Ok::<_, crate::db::Error>(())
    }) }, |value, ()| async move { Ok(value) }).await;
    assert!(regression.is_err());
    assert_eq!(for_character(&db, owner).await.unwrap()[1], full);
    // Pool rows and their payout job roll back together.
    let other = Batch { character_id: owner, started_at: 200, due_at: 1400 };
    let outcome = db.transaction(|tx, _: &mut ()| Box::pin(async move {
        save_absolute(tx, &[pool(other, 3, "1.00", 1)]).await?;
        crate::dao::jobs::ensure_once(tx, &crate::scheduler::Job { id: format!("npc-bounty-rollback-{seq}"), kind: "npc.bounty.payout".into(), due_at: 1, payload: "{}".into() }, 1).await?;
        Err::<(), crate::db::Error>(crate::db::Error::Progress(anyhow::anyhow!("forced rollback")))
    }), |value, ()| async move { Ok(value) }).await;
    assert!(outcome.is_err());
    assert!(for_character(&db, owner).await.unwrap().iter().all(|row| row.system_id != 3));
    assert!(db.query_opt("SELECT job_id FROM game.jobs WHERE job_id=$1", &[&format!("npc-bounty-rollback-{seq}")]).await.unwrap().is_none());
    // Precise delete removes exactly this batch's rows.
    assert_eq!(delete_batch(&db, batch).await.unwrap(), 2);
    assert!(for_character(&db, owner).await.unwrap().is_empty());
    assert_eq!(delete_batch(&db, batch).await.unwrap(), 0);
    db.execute("DELETE FROM game.characters WHERE character_id=$1", &[&owner]).await.unwrap();
    db.execute("DELETE FROM game.accounts WHERE user_id=$1", &[&account]).await.unwrap();
}
