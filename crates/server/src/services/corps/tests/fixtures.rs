//! Transaction-local corporation fixtures and notification observation baselines.
use tokio_postgres::{Client, NoTls, Transaction};

pub(super) async fn notification_marker(db: &crate::db::Db, receivers: &[i64]) -> i64 {
    db.query_one("SELECT COALESCE(MAX(notification_id),0) AS marker FROM game.notifications WHERE receiver_id=ANY($1)", &[&receivers]).await.unwrap().get("marker")
}

pub(super) async fn connect() -> Client {
    let cfg = crate::config::tests::test_database();
    let (client, connection) = cfg.to_pg_config().unwrap().connect(NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    client
}

pub(super) async fn character(tx: &Transaction<'_>) -> i64 {
    let account: i64 = tx.query_one("INSERT INTO game.accounts (user_name,password_hash,role) VALUES ('governance-fixture-' || nextval('game.accounts_user_id_seq')::text,$1,0) RETURNING user_id", &[&&b"fixture"[..]]).await.unwrap().get("user_id");
    tx.query_one("INSERT INTO game.characters (user_id,name,race_id,bloodline_id,gender_id,corporation_id) VALUES ($1,'governance-fixture-' || nextval('game.characters_character_id_seq')::text,1,1,1,1) RETURNING character_id", &[&account]).await.unwrap().get("character_id")
}

pub(super) async fn corporation(tx: &Transaction<'_>, ceo: i64) -> i64 {
    let corp: i64 = tx.query_one("INSERT INTO game.corporations (name,ticker,ceo_id,creator_id,station_id,created) VALUES ('governance-fixture-' || nextval('game.corporations_corporation_id_seq')::text,'GF' || currval('game.corporations_corporation_id_seq')::text,$1,$1,1,1) RETURNING corporation_id", &[&ceo]).await.unwrap().get("corporation_id");
    tx.execute("UPDATE game.characters SET corporation_id=$1,corp_roles=$3 WHERE character_id=$2", &[&corp, &ceo, &infinity_core::consts::ROLE_DIRECTOR]).await.unwrap();
    corp
}
