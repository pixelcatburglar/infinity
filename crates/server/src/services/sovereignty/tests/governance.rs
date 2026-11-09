//! Governance transaction checks with isolated rollback-owned rows.
use crate::{dao::sovereignty::systems, db::outbox::Outbox, services::sovereignty::governance::{self, AdmittedAllianceChange}};

mod linearization;

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database"]
async fn empty_set_recheck_rejects_new_claim_without_publication() {
    let cfg = crate::config::tests::test_database();
    let (mut client, connection) = cfg.to_pg_config().unwrap().connect(tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let tx = client.transaction().await.unwrap();
    let item: i64 = tx.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id) VALUES (1,1,1,0) RETURNING item_id", &[]).await.unwrap().get("item_id");
    let mut outbox: Outbox = Outbox::default();
    governance::apply(&tx, &mut outbox, item, 1, &AdmittedAllianceChange::NoOwnershipChange).await.unwrap();
    assert!(outbox.into_entries().is_empty());
    let initial = systems::lock(&tx, item).await.unwrap();
    systems::replace(&tx, &initial, &systems::SystemChange { hub_item_id: Some(item), claim_corporation_id: Some(item), alliance_id: Some(item), claim_time: Some(1), ownership_epoch: 1 }).await.unwrap().unwrap();
    let mut outbox: Outbox = Outbox::default();
    assert!(governance::apply(&tx, &mut outbox, item, 2, &AdmittedAllianceChange::NoOwnershipChange).await.is_err());
    assert!(outbox.into_entries().is_empty());
    tx.rollback().await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database"]
async fn claim_loss_notice_reaches_offline_owner_once_and_rolls_back() {
    use crate::dao::sovereignty::history::HistoryRow;
    use crate::services::sovereignty::{notifications::{self, HubTransition}, TransitionKind};
    let cfg = crate::config::tests::test_database();
    let (mut client, connection) = cfg.to_pg_config().unwrap().connect(tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let mut tx = client.transaction().await.unwrap();
    let account: i64 = tx.query_one("INSERT INTO game.accounts (user_name,password_hash,role) VALUES ('sov-notice-' || nextval('game.accounts_user_id_seq'),$1,0) RETURNING user_id", &[&&b"fixture"[..]]).await.unwrap().get("user_id");
    let character: i64 = tx.query_one("INSERT INTO game.characters (user_id,name,race_id,bloodline_id,gender_id,corporation_id) VALUES ($1,'sov-notice-' || nextval('game.characters_character_id_seq'),1,1,1,1) RETURNING character_id", &[&account]).await.unwrap().get("character_id");
    let corp: i64 = tx.query_one("INSERT INTO game.corporations (name,ticker,ceo_id,creator_id,station_id,created) VALUES ('sov-notice-' || nextval('game.corporations_corporation_id_seq'),'SN' || currval('game.corporations_corporation_id_seq'),$1,$1,1,1) RETURNING corporation_id", &[&character]).await.unwrap().get("corporation_id");
    tx.execute("UPDATE game.characters SET corporation_id=$1 WHERE character_id=$2", &[&corp, &character]).await.unwrap();
    let namespace = format!("sov-notice-{corp}");
    tx.execute("INSERT INTO game.lua_content_versions (namespace,version,schema_version,compiler_identity,registration,source,data,fingerprint) VALUES ($1,'1',1,'test',$2,$2,$2,$3)", &[&namespace, &&b""[..], &&[0u8; 32][..]]).await.unwrap();
    let mut history = HistoryRow { history_id: 0, system_id: corp, hub_item_id: corp, old_corporation_id: Some(corp), new_corporation_id: None, old_alliance_id: Some(corp), new_alliance_id: None, old_epoch: 1, new_epoch: 2, reason: TransitionKind::Neutralized.as_str().into(), at: 1, actor_id: Some(character), operation_key: format!("fixture:{corp}"), content_namespace: namespace, content_version: "1".into() };
    let save = tx.savepoint("notification_rollback").await.unwrap();
    history.history_id = crate::dao::sovereignty::history::insert(&save, &history).await.unwrap().unwrap();
    assert!(crate::dao::sovereignty::history::insert(&save, &history).await.unwrap().is_none());
    let mut outbox: Outbox = Outbox::default();
    notifications::transition(&save, &mut outbox, &HubTransition { history, before: None, after: None, capital: None, destruct_deadline: None }).await.unwrap();
    let rows = save.query("SELECT type_id,data FROM game.notifications WHERE receiver_id=$1", &[&character]).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get::<_, i32>("type_id"), 42);
    let payload: serde_json::Value = serde_json::from_str(rows[0].get::<_, &str>("data")).unwrap();
    assert_eq!(payload["solarSystemID"], corp);
    save.rollback().await.unwrap();
    assert!(tx.query("SELECT notification_id FROM game.notifications WHERE receiver_id=$1", &[&character]).await.unwrap().is_empty());
    tx.rollback().await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database"]
async fn source_invalidation_is_transactional_and_does_not_create_inactive_sources() {
    use crate::dao::sovereignty::sources;
    let cfg = crate::config::tests::test_database();
    let (mut client, connection) = cfg.to_pg_config().unwrap().connect(tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let mut tx = client.transaction().await.unwrap();
    let account: i64 = tx.query_one("INSERT INTO game.accounts (user_name,password_hash,role) VALUES ('sov-source-' || nextval('game.accounts_user_id_seq'),$1,0) RETURNING user_id", &[&&b"fixture"[..]]).await.unwrap().get("user_id");
    let character: i64 = tx.query_one("INSERT INTO game.characters (user_id,name,race_id,bloodline_id,gender_id,corporation_id) VALUES ($1,'sov-source-' || nextval('game.characters_character_id_seq'),1,1,1,1) RETURNING character_id", &[&account]).await.unwrap().get("character_id");
    let mut empty: Outbox = Outbox::default();
    crate::services::sovereignty::invalidate_sources(&tx, &mut empty, &[character]).await.unwrap();
    assert!(empty.into_entries().is_empty());
    assert!(sources::versions(&tx, &[character]).await.unwrap().is_empty());
    sources::ensure(&tx, character).await.unwrap();
    let save = tx.savepoint("source_rollback").await.unwrap();
    let mut outbox: Outbox = Outbox::default();
    let before = crate::client::filetime_now();
    crate::services::sovereignty::invalidate_sources(&save, &mut outbox, &[character]).await.unwrap();
    let after = crate::client::filetime_now();
    assert_eq!(outbox.into_entries().len(), 1);
    let effective_at = sources::first_change_after(&save, character, 0).await.unwrap().unwrap().effective_at;
    assert!((before..=after).contains(&effective_at));
    save.rollback().await.unwrap();
    assert_eq!(sources::versions(&tx, &[character]).await.unwrap()[0].revision, 0);
    assert!(sources::first_change_after(&tx, character, 0).await.unwrap().is_none());
    tx.rollback().await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database"]
async fn capital_application_preserves_unchanged_cooldown_and_guards_revision() {
    use crate::{dao::sovereignty::policy as dao, lua::content::PolicyPlan, services::sovereignty::policy};
    let cfg = crate::config::tests::test_database();
    let (mut client, connection) = cfg.to_pg_config().unwrap().connect(tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let tx = client.transaction().await.unwrap();
    let item: i64 = tx.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id) VALUES (1,1,1,0) RETURNING item_id", &[]).await.unwrap().get("item_id");
    let initial = dao::lock(&tx, item).await.unwrap();
    let appointed = policy::apply(&tx, &initial, &PolicyPlan::Capital { current_system: item, not_before: 100 }, 10).await.unwrap();
    assert_eq!(appointed.current_capital_system, Some(item));
    let publication = crate::services::sovereignty::CapitalPublication::new(appointed.clone(), initial.current_capital_system);
    assert!(publication.changed);
    assert_eq!(publication.policy.current_capital_system, Some(item));
    assert_eq!(appointed.last_capital_changed_at, Some(10));
    let unchanged = policy::apply(&tx, &appointed, &PolicyPlan::Unchanged, 20).await.unwrap();
    assert_eq!(unchanged, appointed);
    let publication = crate::services::sovereignty::CapitalPublication::new(unchanged, appointed.current_capital_system);
    assert!(!publication.changed);
    let lost = policy::apply(&tx, &appointed, &PolicyPlan::CapitalLost { not_before: 100 }, 30).await.unwrap();
    assert_eq!(lost.current_capital_system, None);
    assert_eq!(lost.capital_change_not_before, Some(100));
    assert_eq!(lost.capital_lost_at, Some(30));
    let publication = crate::services::sovereignty::CapitalPublication::new(lost.clone(), appointed.current_capital_system);
    assert!(publication.changed);
    assert_eq!(publication.policy.current_capital_system, None);
    let publication = crate::services::sovereignty::CapitalPublication::new(lost, None);
    assert!(!publication.changed);
    assert!(policy::apply(&tx, &appointed, &PolicyPlan::CapitalLost { not_before: 100 }, 30).await.is_err());
    tx.rollback().await.unwrap();
}
