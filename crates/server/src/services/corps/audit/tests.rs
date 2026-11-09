//! Audit ordering, date boundaries and transaction rollback.
use crate::dao::corporations::audit;
use tokio_postgres::Transaction;

async fn character(tx: &Transaction<'_>) -> i64 {
    let account: i64 = tx.query_one("INSERT INTO game.accounts(user_name,password_hash,role) VALUES ('audit-test-' || nextval('game.accounts_user_id_seq')::text,$1,0) RETURNING user_id", &[&&b"fixture"[..]]).await.unwrap().get("user_id");
    tx.query_one("INSERT INTO game.characters(user_id,name,race_id,bloodline_id,gender_id,corporation_id) VALUES ($1,'audit-test-' || nextval('game.characters_character_id_seq')::text,1,1,1,1) RETURNING character_id", &[&account]).await.unwrap().get("character_id")
}
async fn corporation(tx: &Transaction<'_>, ceo: i64) -> i64 {
    tx.query_one("INSERT INTO game.corporations(name,ticker,ceo_id,creator_id,station_id,created) VALUES ('audit-test-' || nextval('game.corporations_corporation_id_seq')::text,'AT' || currval('game.corporations_corporation_id_seq')::text,$1,$1,1,1) RETURNING corporation_id", &[&ceo]).await.unwrap().get("corporation_id")
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn audit_pages_are_corporation_scoped_and_transactional() {
    crate::tests::init_config();
    let config = crate::config::tests::test_database().to_pg_config().unwrap();
    let (mut client, connection) = config.connect(tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let tx = client.transaction().await.unwrap();
    let actor = character(&tx).await;
    let member = character(&tx).await;
    let outsider = character(&tx).await;
    let corp = corporation(&tx, actor).await;
    let other = corporation(&tx, outsider).await;
    tx.execute("UPDATE game.characters SET corporation_id=$1 WHERE character_id=$2", &[&corp, &member]).await.unwrap();
    audit::event(&tx, member, actor, corp, super::JOINED, 10).await.unwrap();
    audit::event(&tx, member, actor, corp, super::LEFT, 20).await.unwrap();
    audit::event(&tx, member, outsider, other, super::APPLIED, 15).await.unwrap();
    audit::role(&tx, member, corp, actor, 15, 0, 8, false, 1).await.unwrap();
    let events = audit::events(&tx, corp, member, 10, 20, 10).await.unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!((events[0].time, events[0].corporation, events[0].kind), (10, corp, super::JOINED));
    let history = audit::roles(&tx, corp, member, 10, 20, 1).await.unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!((history[0].time, history[0].issuer, history[0].old, history[0].new, history[0].grantable), (15, actor, 0, 8, false));
    assert!(!audit::target_visible(&tx, corp, outsider).await.unwrap());
    tx.rollback().await.unwrap();
    let rows = client.query("SELECT event_id FROM game.corp_member_events WHERE corporation_id=$1 AND character_id=$2", &[&corp, &member]).await.unwrap();
    assert!(rows.is_empty());
}
