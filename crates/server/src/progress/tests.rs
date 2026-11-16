#[cfg(any(feature = "tests-combat", feature = "tests-content", feature = "tests-economy", feature = "tests-gateway", feature = "tests-lua", feature = "tests-skills", feature = "tests-storage"))]
use super::*;
#[cfg(feature = "tests-storage")]
mod prepared;
#[cfg(feature = "tests-storage")]
mod terminal;
#[cfg(feature = "tests-storage")]
use std::sync::atomic::{AtomicUsize, Ordering};

#[cfg(feature = "tests-storage")]
static SEEN: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "tests-storage")]
fn observe(_: &GameEvent) { SEEN.fetch_add(1, Ordering::SeqCst); }
#[cfg(feature = "tests-storage")]
fn fail_observer(_: &GameEvent) { panic!("observer failure"); }

#[cfg(feature = "tests-storage")]
fn handler<'a, 'tx>(tx: &'a Transaction<'tx>, _: &'a mut Outbox, _: &'a Sde, _: &'a EventEnvelope) -> TxFuture<'a, (), ApplyError> {
    Box::pin(async move {
        tx.execute("INSERT INTO progress_test_effects DEFAULT VALUES", &[]).await.map_err(crate::db::Error::from)?;
        Ok(())
    })
}

#[cfg(any(feature = "tests-combat", feature = "tests-content", feature = "tests-economy", feature = "tests-gateway", feature = "tests-lua", feature = "tests-skills", feature = "tests-storage"))]
pub(crate) async fn register_fixture(ex: &(impl crate::db::executor::Executor + ?Sized), registration: &DurableRegistration) {
    ex.execute("INSERT INTO game.progress_subscriptions (consumer_key,generation,activated_at,version,enabled) VALUES ($1,1,1,1,true) ON CONFLICT (consumer_key) DO NOTHING", &[&registration.consumer_key]).await.unwrap();
    let row = ex.query_one("SELECT enabled FROM game.progress_subscriptions WHERE consumer_key=$1", &[&registration.consumer_key]).await.unwrap();
    assert!(row.get::<_, bool>("enabled"), "fixture subscription must be enabled");
    ex.execute("INSERT INTO game.progress_subscription_kinds (consumer_key,kind) SELECT $1,unnest($2::text[]) ON CONFLICT DO NOTHING", &[&registration.consumer_key, &registration.kinds]).await.unwrap();
}

#[cfg(any(feature = "tests-combat", feature = "tests-content", feature = "tests-economy", feature = "tests-gateway", feature = "tests-lua", feature = "tests-skills", feature = "tests-storage"))]
/// Register every declared consumer, read the rows back and install the
/// resulting enabled projection, mirroring the startup reconcile install.
pub(crate) async fn fixture_registry(ex: &(impl crate::db::executor::Executor + ?Sized), registry: Registry) -> Registry {
    if registry.durable().is_empty() { return registry; }
    for registration in registry.durable() { register_fixture(ex, registration).await; }
    let keys: Vec<&str> = registry.durable().iter().map(|registration| registration.consumer_key).collect();
    let rows = crate::dao::progress::subscriptions(ex, &keys).await.unwrap();
    registry.with_subscriptions(&rows).unwrap()
}

#[cfg(feature = "tests-storage")]
async fn foreign_snapshot(ex: &(impl crate::db::executor::Executor + ?Sized), own: &str, event_id: &str) -> (Vec<(String, bool, Vec<String>)>, Vec<(String, String, i32)>) {
    let subscriptions = ex.query("SELECT s.consumer_key,s.enabled,ARRAY(SELECT k.kind FROM game.progress_subscription_kinds k WHERE k.consumer_key=s.consumer_key ORDER BY k.kind) AS kinds FROM game.progress_subscriptions s WHERE s.consumer_key<>$1 ORDER BY s.consumer_key", &[&own]).await.unwrap()
        .into_iter().map(|r| (r.get("consumer_key"), r.get("enabled"), r.get("kinds"))).collect();
    let deliveries = ex.query("SELECT consumer_key,state,attempts FROM game.progress_deliveries WHERE consumer_key<>$1 AND event_id=$2::text::uuid ORDER BY consumer_key", &[&own, &event_id]).await.unwrap()
        .into_iter().map(|r| (r.get("consumer_key"), r.get("state"), r.get("attempts"))).collect();
    (subscriptions, deliveries)
}

#[cfg(feature = "tests-storage")]
#[test]
fn registration_rejects_duplicate_keys_and_kinds() {
    let first = || DurableRegistration { consumer_key: "first", kinds: &["SkillTrained"], mode: DeliveryMode::Direct(handler) };
    assert!(Registry::new(vec![], vec![first(), first()]).is_err());
    assert!(Registry::new(vec![], vec![DurableRegistration { consumer_key: "first", kinds: &["SkillTrained", "SkillTrained"], mode: DeliveryMode::Direct(handler) }]).is_err());
    assert!(Registry::new(vec![], vec![first(), DurableRegistration { consumer_key: "second", kinds: &["SkillTrained"], mode: DeliveryMode::Direct(handler) }]).is_ok());
}

#[cfg(feature = "tests-storage")]
#[test]
fn live_dispatch_filters_kinds_and_isolates_observers() {
    SEEN.store(0, Ordering::SeqCst);
    let registry = Registry::new(vec![fail_observer, observe], vec![]).unwrap();
    publish_live(&registry, &GameEvent::Targeted { char_id: 1, by_ball_id: 2, system_id: 3 });
    publish_live(&registry, &GameEvent::Damaged { char_id: 1, ship_id: 2, attacker_ball_id: 3, system_id: 4 });
    publish_live(&registry, &GameEvent::ModuleCycleEnded { char_id: 1, module_item_id: 2, effect_id: 3, outcome: crate::lua::events::ModuleCycleOutcome::Completed });
    publish_live(&registry, &GameEvent::WarpArrived { char_id: 1, system_id: 2 });
    publish_live(&registry, &GameEvent::CharacterSelected { user_id: 1, char_id: 2 });
    assert_eq!(SEEN.load(Ordering::SeqCst), 4);
}

#[cfg(feature = "tests-storage")]
#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn duplicate_sources_fan_out_and_apply_only_once() {
    crate::tests::init_config();
    let config = crate::config::tests::test_database();
    let db = crate::db::Db::connect(&config).await.unwrap();
    let sde = Sde::load(&db).await.unwrap();
    let (client, connection) = tokio_postgres::connect(&config.url, tokio_postgres::NoTls).await.unwrap();
    let mut wrapper = deadpool_postgres::ClientWrapper::new(client, tokio::spawn(async move { connection.await.unwrap(); }));
    let tx = crate::db::Transaction::from(wrapper.transaction().await.unwrap());
    tx.batch_execute("CREATE TEMP TABLE progress_test_effects (id bigserial) ON COMMIT DROP").await.unwrap();
    let registry = Registry::new(vec![], vec![
        DurableRegistration { consumer_key: "progress-test-one", kinds: &["SkillTrained"], mode: DeliveryMode::Direct(handler) },
        DurableRegistration { consumer_key: "progress-test-two", kinds: &["SkillTrained"], mode: DeliveryMode::Direct(handler) },
        crate::services::skill_plans::progress::registration(),
        crate::services::daily_goals::progress::registration(),
        crate::services::sovereignty::indices::progress::registration(),
    ]).unwrap();
    let event = GameEvent::SkillTrained { char_id: 1, type_id: 2, level: 3 };
    let batch = [crate::dao::progress::JournalEvent {
        source_namespace: format!("test-{}", crate::handshake::random_u64()), source_key: "0".into(),
        schema_version: codec::SCHEMA_VERSION, kind: event.kind().into(), occurred_at: 1, recorded_at: 1,
        payload: codec::encode(&event).unwrap(), context: "{}".into(), origin: "test".into(), ordinal: 0,
        subject_character_id: Some(1),
    }];
    let previous = crate::dao::progress::JournalEvent {
        source_namespace: batch[0].source_namespace.clone(), source_key: "previous".into(),
        schema_version: codec::SCHEMA_VERSION, kind: event.kind().into(), occurred_at: 1, recorded_at: 1,
        payload: codec::encode(&event).unwrap(), context: "{}".into(), origin: "test".into(), ordinal: 0,
        subject_character_id: Some(1),
    };
    let old = crate::dao::progress::insert_batch(&tx, std::slice::from_ref(&previous), &[], &[]).await.unwrap();
    let rows = crate::dao::progress::reconcile(&tx, &registry).await.unwrap();
    let registry = registry.with_subscriptions(&rows).unwrap();
    crate::dao::progress::reconcile(&tx, &registry).await.unwrap();
    assert!(crate::dao::progress::reconcile(&tx, &Registry::default()).await.is_err());
    let old_receipts: i64 = tx.query_one("SELECT count(*) AS count FROM game.progress_deliveries WHERE event_id=$1::text::uuid AND consumer_key IN ('progress-test-one','progress-test-two')", &[&old[0].1]).await.unwrap().get("count");
    assert_eq!(old_receipts, 0);
    let (consumers, kinds) = registry.subscription_arrays();
    let inserted = crate::dao::progress::insert_batch(&tx, &batch, consumers, kinds).await.unwrap();
    let duplicate = crate::dao::progress::insert_batch(&tx, &batch, consumers, kinds).await.unwrap();
    assert!(inserted[0].2);
    assert!(!duplicate[0].2);
    assert_eq!(inserted[0].1, duplicate[0].1);
    let count: i64 = tx.query_one("SELECT count(*) AS count FROM game.progress_deliveries WHERE event_id=$1::text::uuid AND consumer_key IN ('progress-test-one','progress-test-two')", &[&inserted[0].1]).await.unwrap().get("count");
    assert_eq!(count, 2);
    let key = DeliveryKey { consumer_key: "progress-test-one".into(), event_id: inserted[0].1.clone() };
    let mut outbox = Outbox::default();
    assert!(matches!(apply_once_tx(&tx, &mut outbox, &sde, &registry.durable()[0], key.clone(), None, ClaimMode::Blocking).await.unwrap(), ApplyOutcome::Applied));
    assert!(matches!(apply_once_tx(&tx, &mut outbox, &sde, &registry.durable()[0], key, None, ClaimMode::Blocking).await.unwrap(), ApplyOutcome::AlreadyApplied));
    let count: i64 = tx.query_one("SELECT count(*) AS count FROM progress_test_effects", &[]).await.unwrap().get("count");
    assert_eq!(count, 1);
    let pending = DeliveryKey { consumer_key: "progress-test-two".into(), event_id: inserted[0].1.clone() };
    let attempt = crate::dao::progress::due(&tx, 10, 64, &["progress-test-two"]).await.unwrap()
        .into_iter().find(|attempt| attempt.key == pending).expect("pending receipt observation");
    crate::dao::progress::failed(&tx, &attempt, 10, "poison", false, crate::config::game().progress.permanent_retry_limit).await.unwrap();
    assert!(!crate::dao::progress::try_claim(&tx, &pending, 10).await.unwrap());
    let mut next = batch[0].clone();
    next.source_key = "next".into();
    let next = crate::dao::progress::insert_batch(&tx, &[next], consumers, kinds).await.unwrap();
    let foreign_before = foreign_snapshot(&tx, "progress-test-one", &next[0].1).await;
    let due = crate::dao::progress::due(&tx, 10, 64, &["progress-test-one", "progress-test-two"]).await.unwrap();
    let scoped = crate::dao::progress::due(&tx, 10, 64, &["progress-test-one"]).await.unwrap();
    assert!(scoped.iter().all(|attempt| attempt.key.consumer_key == "progress-test-one"));
    assert!(scoped.iter().any(|attempt| attempt.key.event_id == next[0].1));
    assert!(foreign_before.1.iter().any(|(consumer, state, _)| consumer == "progress-test-two" && state == "pending"));
    assert_eq!(foreign_snapshot(&tx, "progress-test-one", &next[0].1).await, foreign_before);
    assert!(due.iter().any(|attempt| attempt.key.event_id == next[0].1));
    assert!(!due.iter().any(|attempt| attempt.key.event_id == pending.event_id && attempt.key.consumer_key == pending.consumer_key));
    let row = tx.query_one("SELECT attempts,next_attempt_at,last_error FROM game.progress_deliveries WHERE consumer_key=$1 AND event_id=$2::text::uuid", &[&pending.consumer_key, &pending.event_id]).await.unwrap();
    assert_eq!(row.get::<_, i32>("attempts"), 1);
    assert_eq!(row.get::<_, String>("last_error"), "poison");
    assert!(crate::dao::progress::try_claim(&tx, &pending, row.get("next_attempt_at")).await.unwrap());
    // A saved projection still fans out after the row changes; only a fresh
    // read-back narrows the enabled scope for later sources and polls.
    let mut another = batch[0].clone();
    another.source_key = "another".into();
    tx.execute("UPDATE game.progress_subscriptions SET enabled=false WHERE consumer_key='progress-test-two'", &[]).await.unwrap();
    let another = crate::dao::progress::insert_batch(&tx, &[another], consumers, kinds).await.unwrap();
    let stale: i64 = tx.query_one("SELECT count(*) AS count FROM game.progress_deliveries WHERE event_id=$1::text::uuid AND consumer_key='progress-test-two'", &[&another[0].1]).await.unwrap().get("count");
    assert_eq!(stale, 1);
    let keys: Vec<&str> = registry.durable().iter().map(|registration| registration.consumer_key).collect();
    let rows = crate::dao::progress::subscriptions(&tx, &keys).await.unwrap();
    let registry = registry.clone().with_subscriptions(&rows).unwrap();
    let (consumers, kinds) = registry.subscription_arrays();
    assert_eq!(registry.enabled_keys(), ["progress-test-one", "personal-skill-plan-milestones", "daily-goals", "sovereignty-indices"].as_slice());
    let mut fresh = batch[0].clone();
    fresh.source_key = "fresh".into();
    let fresh = crate::dao::progress::insert_batch(&tx, &[fresh], consumers, kinds).await.unwrap();
    let created: Vec<String> = tx.query("SELECT consumer_key FROM game.progress_deliveries WHERE event_id=$1::text::uuid ORDER BY consumer_key", &[&fresh[0].1]).await.unwrap().into_iter().map(|row| row.get("consumer_key")).collect();
    assert_eq!(created, ["personal-skill-plan-milestones", "progress-test-one"]);
    assert!(crate::dao::progress::pending_for_character(&tx, "progress-test-two", 1, 64, registry.enabled_keys()).await.unwrap().is_empty());
    assert!(crate::dao::progress::due(&tx, i64::MAX, 64, registry.enabled_keys()).await.unwrap().iter().all(|attempt| attempt.key.consumer_key != "progress-test-two"));
    tx.rollback().await.unwrap();
}

#[cfg(feature = "tests-storage")]
static ORDER: std::sync::Mutex<Vec<i64>> = std::sync::Mutex::new(Vec::new());
#[cfg(feature = "tests-storage")]
fn observe_order(event: &GameEvent) {
    if let GameEvent::CharacterSelected { user_id, .. } = event {
        ORDER.lock().unwrap().push(*user_id);
    }
}
#[cfg(feature = "tests-storage")]
struct Nested(Outbox);
#[cfg(feature = "tests-storage")]
impl Prepare for Nested {
    fn prepare<'a, 'tx>(&'a mut self, tx: &'a Transaction<'tx>, context: &'a mut PrepareCx) -> TxFuture<'a, (), PrepareError> {
        self.0.prepare(tx, context)
    }
}
#[cfg(feature = "tests-storage")]
impl AfterCommit for Nested {
    fn after_commit(&mut self, registry: &Registry, committed: &HashSet<String>) -> crate::progress::AfterCommitHandle {
        self.0.after_commit(registry, committed)
    }
}

#[cfg(feature = "tests-storage")]
#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database"]
async fn nested_events_publish_in_order_only_after_commit() {
    crate::tests::init_config();
    ORDER.lock().unwrap().clear();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap()
        .with_progress_registry(Registry::new(vec![observe_order], vec![]).unwrap());
    let subject = crate::handshake::rand_i64();
    let (sender, mut notifications) = tokio::sync::mpsc::unbounded_channel();
    let failed: Result<(), CallError> = db.transaction(
        |_, outbox: &mut Outbox<Nested>| {
            let sender = sender.clone();
            Box::pin(async move {
                outbox.event(GameEvent::CharacterSelected { user_id: 1, char_id: subject });
                outbox.notify_sender(sender, "rollback", vec![]);
                Err(CallError::Internal(anyhow::anyhow!("rollback")))
            })
        },
        |(), _| async { Ok(()) },
    ).await;
    assert!(failed.is_err());
    assert!(ORDER.lock().unwrap().is_empty());
    assert!(notifications.try_recv().is_err());
    let invalid: Result<(), CallError> = db.transaction(
        |_, outbox: &mut Outbox| Box::pin(async move {
            let context = crate::lua::events::resources::FactContext { corporation_id: None, alliance_id: None, faction_id: None, ship_id: None, ship_type_id: None, system_id: None, station_id: None, origin: crate::lua::events::resources::FactOrigin::Natural };
            outbox.event(GameEvent::WreckSalvaged { char_id: subject, wreck_id: 1, wreck_type: 1, occurred_at: 0, context });
            Ok(())
        }),
        |(), _| async { Ok(()) },
    ).await;
    assert!(invalid.is_err());
    assert!(ORDER.lock().unwrap().is_empty());
    db.transaction(
        |_, outbox: &mut Outbox<Nested>| Box::pin(async move {
            outbox.event(GameEvent::CharacterSelected { user_id: 1, char_id: subject });
            let mut nested = Outbox::default();
            nested.event(GameEvent::CharacterSelected { user_id: 2, char_id: subject });
            outbox.operation(Nested(nested));
            outbox.event(GameEvent::CharacterSelected { user_id: 3, char_id: subject });
            assert!(ORDER.lock().unwrap().is_empty());
            Ok::<_, CallError>(())
        }),
        |(), _| async { Ok::<_, CallError>(()) },
    ).await.unwrap();
    assert_eq!(*ORDER.lock().unwrap(), vec![1, 2, 3]);
    let rows = db.query("SELECT ordinal FROM game.progress_events WHERE subject_character_id=$1 ORDER BY ordinal", &[&subject]).await.unwrap();
    assert_eq!(rows.iter().map(|row| row.get::<_, i64>("ordinal")).collect::<Vec<_>>(), vec![0, 1, 2]);
    let attempts = AtomicUsize::new(0);
    db.transaction(
        |tx, outbox: &mut Outbox| {
            let first = attempts.fetch_add(1, Ordering::SeqCst) == 0;
            Box::pin(async move {
                outbox.event(GameEvent::CharacterSelected { user_id: 4, char_id: subject });
                if first {
                    tx.batch_execute("DO $$ BEGIN RAISE EXCEPTION 'retry' USING ERRCODE = '40001'; END $$").await.map_err(crate::db::Error::from)?;
                }
                Ok::<_, CallError>(())
            })
        },
        |(), _| async { Ok::<_, CallError>(()) },
    ).await.unwrap();
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    assert_eq!(*ORDER.lock().unwrap(), vec![1, 2, 3, 4]);
    db.execute("DELETE FROM game.progress_deliveries WHERE event_id IN (SELECT event_id FROM game.progress_events WHERE subject_character_id=$1)", &[&subject]).await.unwrap();
    db.execute("DELETE FROM game.progress_events WHERE subject_character_id=$1", &[&subject]).await.unwrap();
}

#[cfg(feature = "tests-storage")]
#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database"]
async fn concurrent_duplicate_sources_return_one_identity() {
    crate::tests::init_config();
    let config = crate::config::tests::test_database();
    let db = crate::db::Db::connect(&config).await.unwrap();
    let event = GameEvent::SkillTrained { char_id: 1, type_id: 2, level: 3 };
    let batch = vec![crate::dao::progress::JournalEvent {
        source_namespace: format!("duplicate-test-{}", crate::handshake::random_u64()),
        source_key: "shared".into(), schema_version: codec::SCHEMA_VERSION, kind: event.kind().into(),
        occurred_at: 1, recorded_at: 1, payload: codec::encode(&event).unwrap(), context: "{}".into(),
        origin: "test".into(), ordinal: 0, subject_character_id: Some(1),
    }];
    async fn insert(url: &str, batch: Vec<crate::dao::progress::JournalEvent>) -> Vec<(String, String, bool)> {
        let (mut client, connection) = tokio_postgres::connect(url, tokio_postgres::NoTls).await.unwrap();
        tokio::spawn(async move { connection.await.unwrap(); });
        let tx = client.transaction().await.unwrap();
        let result = crate::dao::progress::insert_batch(&tx, &batch, &[], &[]).await.unwrap();
        tx.commit().await.unwrap();
        result
    }
    let (first, second) = tokio::join!(insert(&config.url, batch.clone()), insert(&config.url, batch.clone()));
    assert_eq!(first[0].1, second[0].1);
    assert_ne!(first[0].2, second[0].2);
    db.execute("DELETE FROM game.progress_deliveries WHERE event_id=$1::text::uuid", &[&first[0].1]).await.unwrap();
    db.execute("DELETE FROM game.progress_events WHERE event_id=$1::text::uuid", &[&first[0].1]).await.unwrap();
}

#[cfg(feature = "tests-storage")]
#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database"]
async fn progress_schema_applies_to_fresh_and_existing_schemas() {
    let config = crate::config::tests::test_database();
    let (mut client, connection) = tokio_postgres::connect(&config.url, tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let tx = client.transaction().await.unwrap();
    let progress_ddl = include_str!("../../sql/game_schema.sql")
        .split_once("-- Durable progress journal and consumer receipts (dao/progress).").unwrap().1
        .split_once("-- End of durable progress journal and consumer receipts (dao/progress).").unwrap().0;
    for existing in [false, true] {
        let schema = format!("progress_test_{}", crate::handshake::random_u64());
        tx.batch_execute(&format!("CREATE SCHEMA {schema}")).await.unwrap();
        if existing {
            tx.batch_execute(&format!(
                "CREATE TABLE {schema}.progress_subscriptions (consumer_key text PRIMARY KEY, generation bigint NOT NULL, activated_at bigint NOT NULL, version integer NOT NULL, enabled boolean NOT NULL);\
                 CREATE TABLE {schema}.progress_events (event_id uuid PRIMARY KEY DEFAULT gen_random_uuid(), source_namespace text NOT NULL, source_key text NOT NULL, schema_version integer NOT NULL, kind text NOT NULL, occurred_at bigint NOT NULL, recorded_at bigint NOT NULL, payload jsonb NOT NULL, context jsonb NOT NULL, causation text, origin text NOT NULL, ordinal bigint NOT NULL, subject_character_id bigint, UNIQUE (source_namespace, source_key));\
                 CREATE TABLE {schema}.progress_deliveries (consumer_key text NOT NULL REFERENCES {schema}.progress_subscriptions, event_id uuid NOT NULL REFERENCES {schema}.progress_events, state text NOT NULL CONSTRAINT progress_deliveries_state_check CHECK (state IN ('pending','applied')), applied_at bigint, next_attempt_at bigint NOT NULL, attempts integer NOT NULL DEFAULT 0, last_error text, subject_character_id bigint, PRIMARY KEY (consumer_key, event_id));\
                 INSERT INTO {schema}.progress_subscriptions VALUES ('legacy',1,1,1,true);\
                 INSERT INTO {schema}.progress_events (source_namespace,source_key,schema_version,kind,occurred_at,recorded_at,payload,context,origin,ordinal) VALUES ('legacy','pending',1,'K',1,1,'{{}}','{{}}','test',0),('legacy','applied',1,'K',1,1,'{{}}','{{}}','test',0),('legacy','terminal',1,'K',1,1,'{{}}','{{}}','test',0);\
                 INSERT INTO {schema}.progress_deliveries (consumer_key,event_id,state,applied_at,next_attempt_at,attempts,last_error) SELECT 'legacy',event_id,source_key,CASE source_key WHEN 'applied' THEN 9 END,1,CASE source_key WHEN 'pending' THEN 3 ELSE 7 END,CASE source_key WHEN 'pending' THEN 'retry' END FROM {schema}.progress_events WHERE source_key<>'terminal'"
            )).await.unwrap();
        }
        let ddl = progress_ddl.replace("game.", &format!("{schema}."));
        tx.batch_execute(&ddl).await.unwrap();
        tx.batch_execute(&ddl).await.unwrap();
        let tables: i64 = tx.query_one("SELECT count(*) AS count FROM information_schema.tables WHERE table_schema=$1 AND table_name IN ('progress_events','progress_subscriptions','progress_subscription_kinds','progress_deliveries')", &[&schema]).await.unwrap().get("count");
        assert_eq!(tables, 4);
        let index: bool = tx.query_one("SELECT EXISTS (SELECT 1 FROM pg_indexes WHERE schemaname=$1 AND indexname='progress_deliveries_subject_pending') AS present", &[&schema]).await.unwrap().get("present");
        assert!(index);
        if existing {
            let rows = tx.query(&format!("SELECT state,attempts,permanent_rejections,last_error FROM {schema}.progress_deliveries ORDER BY state"), &[]).await.unwrap();
            assert_eq!(rows.len(), 2);
            assert_eq!((rows[0].get::<_, String>("state"), rows[0].get::<_, i32>("attempts"), rows[0].get::<_, i32>("permanent_rejections"), rows[0].get::<_, Option<String>>("last_error")), ("applied".to_owned(), 7, 0, None));
            assert_eq!((rows[1].get::<_, String>("state"), rows[1].get::<_, i32>("attempts"), rows[1].get::<_, i32>("permanent_rejections"), rows[1].get::<_, Option<String>>("last_error")), ("pending".to_owned(), 3, 0, Some("retry".to_owned())));
            tx.execute(&format!("INSERT INTO {schema}.progress_deliveries (consumer_key,event_id,state,next_attempt_at,terminal_at) SELECT 'legacy',event_id,'terminal',1,10 FROM {schema}.progress_events WHERE source_key='terminal'"), &[]).await.unwrap();
        }
    }
    tx.rollback().await.unwrap();
}


#[cfg(feature = "tests-storage")]
fn restart_handler<'a, 'tx>(tx: &'a Transaction<'tx>, _: &'a mut Outbox, _: &'a Sde, _: &'a EventEnvelope) -> TxFuture<'a, (), ApplyError> {
    Box::pin(async move {
        tx.execute("UPDATE game.progress_subscriptions SET version=version+1 WHERE consumer_key='progress-restart-test'", &[]).await.map_err(crate::db::Error::from)?;
        Ok(())
    })
}

#[cfg(feature = "tests-storage")]
#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn pending_receipt_is_polled_after_reconnect() {
    crate::tests::init_config();
    let config = crate::config::tests::test_database();
    let registration = || DurableRegistration { consumer_key: "progress-restart-test", kinds: &["CharacterSelected"], mode: DeliveryMode::Direct(restart_handler) };
    let db = crate::db::Db::connect(&config).await.unwrap();
    let registry = fixture_registry(&db, Registry::new(vec![], vec![registration()]).unwrap()).await;
    let db = db.with_progress_registry(registry);
    let event = GameEvent::CharacterSelected { user_id: 1, char_id: 1 };
    let batch = vec![crate::dao::progress::JournalEvent {
        source_namespace: format!("restart-test-{}", crate::handshake::random_u64()), source_key: "one".into(),
        schema_version: codec::SCHEMA_VERSION, kind: event.kind().into(), occurred_at: 1, recorded_at: 1,
        payload: codec::encode(&event).unwrap(), context: "{}".into(), origin: "test".into(),
        ordinal: 0, subject_character_id: Some(1),
    }];
    let inserted = db.transaction(
        |tx, _: &mut ()| {
            let batch = batch.clone();
            let db = db.clone();
            Box::pin(async move {
                let (consumers, kinds) = db.progress_registry().subscription_arrays();
                crate::dao::progress::insert_batch(tx, &batch, consumers, kinds).await
            })
        },
        |rows, ()| async { Ok(rows) },
    ).await.unwrap();
    drop(db);
    let db = crate::db::Db::connect(&config).await.unwrap();
    let registry = fixture_registry(&db, Registry::new(vec![], vec![registration()]).unwrap()).await;
    let db = db.with_progress_registry(registry);
    let sde = std::sync::Arc::new(Sde::load(&db).await.unwrap());
    let world = crate::space::World::new(db.clone(), sde, 0);
    drain(db.clone(), world, crate::scheduler::Job { id: DRAIN_KIND.into(), kind: DRAIN_KIND.into(), due_at: 1, payload: "{}".into() }).await.unwrap();
    let row = db.query_one("SELECT d.state,s.version FROM game.progress_deliveries d JOIN game.progress_subscriptions s ON s.consumer_key=d.consumer_key WHERE d.consumer_key='progress-restart-test' AND d.event_id=$1::text::uuid", &[&inserted[0].1]).await.unwrap();
    assert_eq!(row.get::<_, String>("state"), "applied");
    assert_eq!(row.get::<_, i32>("version"), 2);
    db.execute("DELETE FROM game.progress_deliveries WHERE event_id=$1::text::uuid", &[&inserted[0].1]).await.unwrap();
    db.execute("DELETE FROM game.progress_events WHERE event_id=$1::text::uuid", &[&inserted[0].1]).await.unwrap();
    db.execute("DELETE FROM game.progress_subscription_kinds WHERE consumer_key='progress-restart-test'", &[]).await.unwrap();
    db.execute("DELETE FROM game.progress_subscriptions WHERE consumer_key='progress-restart-test'", &[]).await.unwrap();
}

#[cfg(any(feature = "tests-combat", feature = "tests-content", feature = "tests-economy"))]
/// A real published sovereignty-indices package on a private host; the
/// activation is committed before callers journal source facts.
pub(crate) struct IndicesFixture {
    pub root: std::path::PathBuf,
    pub policies: crate::lua::RetainedPolicies,
    pub activation: crate::dao::lua_content::activations::ActivationRow,
}
#[cfg(any(feature = "tests-combat", feature = "tests-content", feature = "tests-economy"))]
impl Drop for IndicesFixture {
    fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.root); }
}

#[cfg(any(feature = "tests-combat", feature = "tests-content", feature = "tests-economy"))]
pub(crate) async fn indices_host(db: &crate::db::Db, sde: &std::sync::Arc<Sde>, world: &std::sync::Arc<crate::space::World>) -> IndicesFixture {
    let root = std::env::temp_dir().join(format!("indices-{}-{}", std::process::id(), crate::handshake::random_u64()));
    let scripts = root.join("scripts");
    std::fs::create_dir_all(scripts.join("content/sovereignty_indices/data")).unwrap();
    let version = format!("test-{}-{}", std::process::id(), crate::handshake::random_u64());
    for (target, source) in [
        ("sovereignty_indices.lua", include_str!("../../lua/sovereignty_indices.lua")),
        ("content/sovereignty_indices/policy.lua", include_str!("../../lua/content/sovereignty_indices/policy.lua")),
        ("content/sovereignty_indices/data/policy.json", include_str!("../../lua/content/sovereignty_indices/data/policy.json")),
    ] {
        let source = if target == "sovereignty_indices.lua" {
            source.replace("content_version = '1'", &format!("content_version = '{version}'"))
        } else if target.ends_with("policy.json") {
            source.replace("\"version\": \"1\"", &format!("\"version\": \"{version}\""))
        } else { source.to_owned() };
        std::fs::write(scripts.join(target), source).unwrap();
    }
    let attributes: Vec<i64> = (1..20_000).filter(|id| sde.dogma.attribute(*id).is_some()).map(i64::from).take(3).collect();
    assert_eq!(attributes.len(), 3);
    let band = |level: i64| serde_json::json!({"level":level, "min_points":level as f64 * 1e6, "max_points":(level + 1) as f64 * 1e6, "modifier":1.0});
    let bands: Vec<_> = (0..6).map(band).collect();
    let dataset = serde_json::to_vec(&serde_json::json!({"schema":"infinity-content-dataset","schema_version":1,"client_build":"test","source_resource":"test",
        "records":[{"key":"","data":{"attribute_ids":{"military":attributes[0],"industrial":attributes[1],"strategic":attributes[2]},
        "military_levels":bands,"industrial_levels":bands,"strategic_seconds":[10,20,30,40,50],
        "occupancy":{"base":1.0,"min":1.0,"max":6.0,"capital":2.0,"military":[{"level":1,"bonus":0.0}],"industrial":[{"level":1,"bonus":0.0}],"strategic":[{"level":1,"bonus":0.0}]}}}]})).unwrap();
    let source = include_bytes!("../../lua/content/sovereignty_indices/policy.lua");
    let manifest = serde_json::to_vec(&serde_json::json!({"schema":"infinity-content","schema_version":2,"client_build":"test",
        "sources":{"policy.lua":{"sha256":crate::hash::sha256_hex(source),"bytes":source.len()}},
        "datasets":{"sovereigntyindices":{"path":"datasets/sovereigntyindices.json","records":1,"sha256":crate::hash::sha256_hex(&dataset)}},
    })).unwrap();
    let bundle = root.join("data/test").join(crate::hash::sha256_hex(&manifest));
    std::fs::create_dir_all(bundle.join("datasets")).unwrap();
    std::fs::write(bundle.join("manifest.json"), &manifest).unwrap();
    std::fs::write(bundle.join("datasets/sovereigntyindices.json"), &dataset).unwrap();
    std::fs::write(root.join("data/current"), format!("test/{}\n", crate::hash::sha256_hex(&manifest))).unwrap();
    let hub = crate::chat::Hub::new("test", db.clone(), world.clone());
    let pilots = crate::lua::host::pilots::Pilots::new(db, hub).await.unwrap();
    let resources = crate::lua::host::Resources { db: db.clone(), sde: sde.clone(), world: world.clone(), pilots, store_value_limit_bytes: 65536 };
    let config = crate::config::lua::LuaConfig { scripts_dir: scripts.to_string_lossy().into_owned(),
        content_data_dir: Some(root.join("data").to_string_lossy().into_owned()), ..Default::default() };
    let catalogue = crate::lua::catalogue::Catalogue::load(db, &config, sde).await.unwrap();
    let host = std::sync::Arc::new(crate::lua::reload::Host::new(config, resources, catalogue).await.unwrap());
    let policies = host.policies();
    let activation = policies.activations("sovereignty_indices").unwrap()
        .iter().find(|row| row.version.starts_with(&format!("{version}/"))).expect("fixture activation").clone();
    IndicesFixture { root, policies, activation }
}

#[cfg(any(feature = "tests-combat", feature = "tests-economy"))]
/// Apply only the explicitly selected fixture receipts through the real path.
pub(crate) async fn drain_indices(db: &crate::db::Db, sde: &std::sync::Arc<Sde>, world: &std::sync::Arc<crate::space::World>, policies: crate::lua::RetainedPolicies, event_ids: &[String]) {
    let registration = crate::services::sovereignty::indices::progress::registration();
    let resources = crate::progress::ContentProgressResources { db: db.clone(), sde: sde.clone(), world: world.clone(), policies };
    for event_id in event_ids {
            let key = DeliveryKey { consumer_key: registration.consumer_key.into(), event_id: event_id.clone() };
            let prepared = match crate::services::sovereignty::indices::progress::prepare(resources.clone(), key.clone()).await.unwrap() {
                crate::progress::PrepareOutcome::Ready(plan) => Some(plan),
                crate::progress::PrepareOutcome::AlreadyApplied => None,
            };
            let (sde, world) = (sde.clone(), world.clone());
            let result: Result<(), crate::progress::ApplyError> = db.transaction(
                |tx, outbox: &mut crate::db::outbox::Outbox| {
                    let (prepared, sde, key) = (prepared.clone(), sde.clone(), key.clone());
                    Box::pin(async move {
                        crate::progress::apply_once_tx(tx, outbox, &sde, &registration, key, prepared.as_deref(), crate::progress::ClaimMode::Blocking).await?;
                        Ok::<_, crate::progress::ApplyError>(())
                    })
                },
                |(), outbox| async move { crate::db::outbox::dispatch_world((), outbox, &world).map_err(crate::progress::ApplyError::Call) },
            ).await;
            result.unwrap();
    }
}
