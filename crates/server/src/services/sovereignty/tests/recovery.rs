//! Recovery enumerates and invalidates only previously activated sources.
use crate::dao::sovereignty::sources;

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database"]
async fn recovery_source_enumeration_and_rollback() {
    let cfg = crate::config::tests::test_database();
    let (mut client, connection) = cfg.to_pg_config().unwrap().connect(tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let mut tx = client.transaction().await.unwrap();
    let user: i64 = tx.query_one("INSERT INTO game.accounts (user_name,password_hash,role) VALUES ('sov-' || nextval('game.accounts_user_id_seq'),$1,0) RETURNING user_id", &[&&b"fixture"[..]]).await.unwrap().get("user_id");
    let character: i64 = tx.query_one("INSERT INTO game.characters (user_id,name,race_id,bloodline_id,gender_id,corporation_id) VALUES ($1,'sov-' || nextval('game.characters_character_id_seq'),1,1,1,1) RETURNING character_id", &[&user]).await.unwrap().get("character_id");
    assert!(!sources::all_character_ids(&tx).await.unwrap().contains(&character));
    let initial = sources::ensure(&tx, character).await.unwrap();
    assert!(sources::all_character_ids(&tx).await.unwrap().contains(&character));
    let save = tx.savepoint("recovery_rollback").await.unwrap();
    let changes = sources::invalidate(&save, &[character], 10).await.unwrap();
    assert_eq!(changes[0].revision, initial.revision + 1);
    save.rollback().await.unwrap();
    assert_eq!(sources::versions(&tx, &[character]).await.unwrap()[0], initial);
    tx.rollback().await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database"]
async fn cancelled_claimed_job_cannot_ack_its_replacement() {
    use crate::{dao::jobs as dao, scheduler::Job, services::sovereignty::jobs};
    let cfg = crate::config::tests::test_database();
    let (mut client, connection) = cfg.to_pg_config().unwrap().connect(tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let tx = client.transaction().await.unwrap();
    let item: i64 = tx.query_one("SELECT nextval('game.items_item_id_seq') AS item_id", &[]).await.unwrap().get("item_id");
    let id = jobs::destruct_id(item);
    let old = Job { id: id.clone(), kind: jobs::DESTRUCT_KIND.into(), due_at: 0, payload: "old".into() };
    dao::schedule(&tx, &old, 0).await.unwrap();
    let claims = dao::claim(&tx, 0, 1, 1000, &[]).await.unwrap();
    let token = claims.iter().find(|claim| claim.job.id == id).expect("fixture job was not claimed").token;
    let replacement = Job { payload: "replacement".into(), due_at: 1, ..old };
    dao::schedule(&tx, &replacement, 0).await.unwrap();
    assert_eq!(dao::ack(&tx, &id, token).await.unwrap(), 0);
    assert_eq!(dao::cancel(&tx, &id).await.unwrap(), 1);
    assert!(dao::lock_job(&tx, &id).await.unwrap().is_none());
    tx.rollback().await.unwrap();
}
