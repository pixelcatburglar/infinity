//! Admission lock ordering against persistent corporation and alliance rows.
//! This server-side locking mechanism has no client counterpart; it uses game
//! alliances, corporations, characters and applications, not admission policy.
use std::time::Duration;
use crate::{dao::corporations::registry, db::Db, services::tests::fixture};

/// Server test design: bound both concurrent transactions to detect lock stalls.
const LOCK_TIMEOUT: Duration = Duration::from_secs(15);

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn reversed_admission_lock_inputs_complete_and_missing_alliance_is_tolerated() {
    crate::tests::init_config();
    let config = crate::config::tests::test_database();
    let db = Db::connect(&config).await.unwrap();
    let first = fixture::create(&db).await.unwrap();
    let second = fixture::create(&db).await.unwrap();
    let corp1 = fixture::create_corporation(&db, first.character, 1).await.unwrap();
    let corp2 = fixture::create_corporation(&db, second.character, 1).await.unwrap();
    let alliance1: i64 = db.query_one("INSERT INTO game.alliances (name,short_name,executor_corp_id,creator_corp_id,creator_char_id,created) VALUES ('lock-test-' || nextval('game.alliances_alliance_id_seq')::text, 'L' || currval('game.alliances_alliance_id_seq')::text, $1,$1,$2,1) RETURNING alliance_id", &[&corp1, &first.character]).await.unwrap().get("alliance_id");
    let alliance2: i64 = db.query_one("INSERT INTO game.alliances (name,short_name,executor_corp_id,creator_corp_id,creator_char_id,created) VALUES ('lock-test-' || nextval('game.alliances_alliance_id_seq')::text, 'L' || currval('game.alliances_alliance_id_seq')::text, $1,$1,$2,1) RETURNING alliance_id", &[&corp2, &second.character]).await.unwrap().get("alliance_id");
    db.execute("UPDATE game.corporations SET alliance_id=$2 WHERE corporation_id=$1", &[&corp1, &alliance1]).await.unwrap();
    let missing: i64 = db.query_one("SELECT id FROM generate_series(-1::bigint, -1000000::bigint, -1::bigint) AS id WHERE NOT EXISTS (SELECT 1 FROM game.alliances WHERE alliance_id=id) LIMIT 1", &[]).await.unwrap().get("id");
    db.execute("UPDATE game.corporations SET alliance_id=$2 WHERE corporation_id=$1", &[&corp2, &alliance2]).await.unwrap();

    let result = tokio::spawn(async move {
        let pg = config.to_pg_config().unwrap();
        let (mut client1, connection1) = pg.connect(tokio_postgres::NoTls).await.unwrap();
        let (mut client2, connection2) = pg.connect(tokio_postgres::NoTls).await.unwrap();
        let driver1 = tokio::spawn(connection1);
        let driver2 = tokio::spawn(connection2);
        let tx1 = client1.transaction().await.unwrap();
        let tx2 = client2.transaction().await.unwrap();
        let run1 = async {
            let locked = registry::lock_admission(&tx1, &[alliance1, alliance2, alliance1, missing], &[corp1, corp2], &[first.character, second.character]).await.unwrap();
            tx1.commit().await.unwrap();
            locked
        };
        let run2 = async {
            let locked = registry::lock_admission(&tx2, &[missing, alliance1, alliance2, alliance1], &[corp2, corp1], &[second.character, first.character]).await.unwrap();
            tx2.commit().await.unwrap();
            locked
        };
        let (a, b) = tokio::time::timeout(LOCK_TIMEOUT, async { tokio::join!(run1, run2) }).await.expect("admission locks timed out");
        assert!(a && b);
        drop(client1);
        drop(client2);
        driver1.await.unwrap().unwrap();
        driver2.await.unwrap().unwrap();
    }).await;

    db.execute("UPDATE game.corporations SET alliance_id=NULL WHERE corporation_id=ANY($1)", &[&vec![corp1, corp2]]).await.unwrap();
    db.execute("DELETE FROM game.alliances WHERE alliance_id=ANY($1)", &[&vec![alliance1, alliance2]]).await.unwrap();
    db.execute("DELETE FROM game.corporations WHERE corporation_id=ANY($1)", &[&vec![corp1, corp2]]).await.unwrap();
    fixture::cleanup(&db, first).await.unwrap();
    fixture::cleanup(&db, second).await.unwrap();
    result.unwrap();
}
