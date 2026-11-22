//! NPC bounty generation cutover: old rows become legacy, replay does not
//! reclassify later kills, and a failed schema transaction restores both.
use crate::db::{Db, Transaction, GAME_SCHEMA};

async fn character(db: &Db) -> (i64, i64) {
    let seq: i64 = db.query_one("SELECT nextval('game.accounts_user_id_seq') AS id", &[]).await.unwrap().get("id");
    let account = crate::dao::accounts::authentication::register(db, &format!("gain-schema-{seq}"), b"fixture", 0, 0).await.unwrap().user_id;
    let owner: i64 = db.query_one("INSERT INTO game.characters(user_id,name,race_id,bloodline_id,gender_id,corporation_id) VALUES ($1,$2,1,1,1,1) RETURNING character_id", &[&account, &format!("gain-schema-{seq}")]).await.unwrap().get("character_id");
    (account, owner)
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL; run this exact test in its own process"]
async fn npc_bounty_generation_upgrades_replays_and_rolls_back() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let (account, owner) = character(&db).await;
    let mut client = db.pool.get().await.unwrap();
    let tx = Transaction::from(client.transaction().await.unwrap());
    tx.batch_execute("ALTER TABLE game.npc_bounty_pools DROP CONSTRAINT IF EXISTS npc_bounty_pools_legacy_gain_check, DROP CONSTRAINT IF EXISTS npc_bounty_pools_security_gain_check, DROP CONSTRAINT IF EXISTS npc_bounty_pools_legacy_count_check, DROP COLUMN IF EXISTS security_gain_log, DROP COLUMN IF EXISTS legacy_kill_count").await.unwrap();
    tx.execute("INSERT INTO game.npc_bounty_pools (character_id,system_id,batch_started_at,due_at,amount,kill_count) VALUES ($1,1,100,200,1,4)", &[&owner]).await.unwrap();
    tx.batch_execute(GAME_SCHEMA).await.unwrap();
    let upgraded = tx.query_one("SELECT legacy_kill_count, security_gain_log FROM game.npc_bounty_pools WHERE character_id=$1", &[&owner]).await.unwrap();
    let legacy: i64 = upgraded.get("legacy_kill_count");
    let gain: f64 = upgraded.get("security_gain_log");
    assert_eq!((legacy, gain), (4, 0.0));
    tx.execute("UPDATE game.npc_bounty_pools SET kill_count=5, security_gain_log=0.25 WHERE character_id=$1", &[&owner]).await.unwrap();
    tx.batch_execute(GAME_SCHEMA).await.unwrap();
    let replayed = tx.query_one("SELECT legacy_kill_count, security_gain_log FROM game.npc_bounty_pools WHERE character_id=$1", &[&owner]).await.unwrap();
    let legacy: i64 = replayed.get("legacy_kill_count");
    let gain: f64 = replayed.get("security_gain_log");
    assert_eq!((legacy, gain), (4, 0.25));
    tx.execute("UPDATE game.npc_bounty_pools SET kill_count=6, security_gain_log=NULL WHERE character_id=$1", &[&owner]).await.unwrap();
    let full = tx.query_one("SELECT security_gain_log FROM game.npc_bounty_pools WHERE character_id=$1", &[&owner]).await.unwrap();
    assert!(full.get::<_, Option<f64>>("security_gain_log").is_none());
    for sql in [
        "UPDATE game.npc_bounty_pools SET kill_count=legacy_kill_count, security_gain_log=NULL WHERE character_id=$1",
        "UPDATE game.npc_bounty_pools SET security_gain_log='NaN' WHERE character_id=$1",
    ] {
        tx.batch_execute("SAVEPOINT npc_gain_check").await.unwrap();
        let error = tx.execute(sql, &[&owner]).await.unwrap_err();
        assert_eq!(error.code().map(|code| code.code()), Some("23514"));
        tx.batch_execute("ROLLBACK TO SAVEPOINT npc_gain_check").await.unwrap();
    }
    tx.rollback().await.unwrap();
    let columns: i64 = db.query_one("SELECT count(*) AS n FROM information_schema.columns WHERE table_schema='game' AND table_name='npc_bounty_pools' AND column_name IN ('legacy_kill_count','security_gain_log')", &[]).await.unwrap().get("n");
    assert_eq!(columns, 2);
    assert!(db.query_opt("SELECT 1 FROM game.npc_bounty_pools WHERE character_id=$1", &[&owner]).await.unwrap().is_none());
    db.execute("DELETE FROM game.characters WHERE character_id=$1", &[&owner]).await.unwrap();
    db.execute("DELETE FROM game.accounts WHERE user_id=$1", &[&account]).await.unwrap();
}
