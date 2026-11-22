//! The startup schema guard end to end: recorded applications, supplied schema
//! variants, refusal of an older recorded schema, and rollback isolation.
use super::*;
mod npc_bounties;
use crate::config::tests::test_database;
use crate::db::{Db, Transaction, GAME_SCHEMA};

async fn rows(ex: &(impl Executor + ?Sized)) -> i64 {
    ex.query_one("SELECT count(*) AS n FROM game.schema_application_history", &[]).await.unwrap().get("n")
}

async fn latest_hash(ex: &(impl Executor + ?Sized)) -> String {
    ex.query_one("SELECT schema_hash AS h FROM game.schema_application_history ORDER BY history_id DESC LIMIT 1", &[]).await.unwrap().get("h")
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL; run this exact test in its own process"]
async fn apply_records_versions_and_blocks_known_nonlatest() {
    let db = Db::connect(&test_database()).await.unwrap();
    let before = rows(&db).await;
    assert!(before > 0, "initialization must record the schema application");
    let guarded = DatabaseConfig { schema_on_start: true, schema_downgrade_guard: true, ..test_database() };
    let db = Db::connect(&guarded).await.unwrap();
    assert_eq!(rows(&db).await, before, "reapplying the same schema appends nothing");
    let mut client = db.pool.get().await.unwrap();
    let tx = Transaction::from(client.transaction().await.unwrap());
    let a = format!("{GAME_SCHEMA}\nCREATE TABLE game.schema_guard_probe (id bigint PRIMARY KEY);");
    let b = format!("{GAME_SCHEMA}\nDROP TABLE game.schema_guard_probe;");
    apply(&tx, &a, &guarded, "build-a").await.unwrap();
    apply(&tx, &b, &guarded, "build-b").await.unwrap();
    assert_eq!(latest_hash(&tx.uncached()).await, crate::hash::sha256_hex(b.as_bytes()));
    // Recorded but superseded: refusal precedes the schema batch, so the probe stays absent.
    let error = apply(&tx, &a, &guarded, "build-a-old").await.unwrap_err();
    assert!(format!("{error:#}").contains("build-b"), "{error:#}");
    assert!(!tx.query_one("SELECT to_regclass('game.schema_guard_probe') IS NOT NULL AS e", &[]).await.unwrap().get::<_, bool>("e"));
    // schema_on_start=false skips hash and history entirely; latest stays B.
    let off = DatabaseConfig { schema_on_start: false, ..guarded.clone() };
    apply(&tx, &a, &off, "ignored").await.unwrap();
    assert_eq!(latest_hash(&tx.uncached()).await, crate::hash::sha256_hex(b.as_bytes()));
    // guard=false does not refuse, but still records the application.
    let unguarded = DatabaseConfig { schema_downgrade_guard: false, ..guarded.clone() };
    apply(&tx, &a, &unguarded, "build-a").await.unwrap();
    assert_eq!(latest_hash(&tx.uncached()).await, crate::hash::sha256_hex(a.as_bytes()));
    // A missing history table is created by the schema batch and then recorded.
    tx.batch_execute("DROP SCHEMA game CASCADE").await.unwrap();
    apply(&tx, GAME_SCHEMA, &guarded, "build-c").await.unwrap();
    assert_eq!(rows(&tx.uncached()).await, 1);
    // A rebuilt binary applying the same schema records again; rerunning it does not.
    apply(&tx, GAME_SCHEMA, &guarded, "build-d").await.unwrap();
    assert_eq!(rows(&tx.uncached()).await, 2);
    apply(&tx, GAME_SCHEMA, &guarded, "build-d").await.unwrap();
    assert_eq!(rows(&tx.uncached()).await, 2);
    tx.rollback().await.unwrap();
    assert_eq!(rows(&db).await, before, "rollback restores the pre-test history");
    Db::connect(&test_database()).await.unwrap();
    assert_eq!(rows(&db).await, before, "schema_on_start=false never writes history");
}
