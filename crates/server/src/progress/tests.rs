use super::*;
mod prepared;
use std::sync::atomic::{AtomicUsize, Ordering};

static SEEN: AtomicUsize = AtomicUsize::new(0);
fn observe(_: &GameEvent) { SEEN.fetch_add(1, Ordering::SeqCst); }
fn fail_observer(_: &GameEvent) { panic!("observer failure"); }

fn handler<'a, 'tx>(tx: &'a Transaction<'tx>, _: &'a mut Outbox, _: &'a Sde, _: &'a EventEnvelope) -> TxFuture<'a, (), CallError> {
    Box::pin(async move {
        tx.execute("INSERT INTO progress_test_effects DEFAULT VALUES", &[]).await.map_err(crate::db::Error::from)?;
        Ok(())
    })
}

pub(crate) async fn register_fixture(ex: &(impl crate::db::executor::Executor + ?Sized), registration: &DurableRegistration) {
    ex.execute("INSERT INTO game.progress_subscriptions (consumer_key,generation,activated_at,version,enabled) VALUES ($1,1,1,1,true) ON CONFLICT (consumer_key) DO NOTHING", &[&registration.consumer_key]).await.unwrap();
    let row = ex.query_one("SELECT enabled FROM game.progress_subscriptions WHERE consumer_key=$1", &[&registration.consumer_key]).await.unwrap();
    assert!(row.get::<_, bool>("enabled"), "fixture subscription must be enabled");
    ex.execute("INSERT INTO game.progress_subscription_kinds (consumer_key,kind) SELECT $1,unnest($2::text[]) ON CONFLICT DO NOTHING", &[&registration.consumer_key, &registration.kinds]).await.unwrap();
}

async fn foreign_snapshot(ex: &(impl crate::db::executor::Executor + ?Sized), own: &str, event_id: &str) -> (Vec<(String, bool, Vec<String>)>, Vec<(String, String, i32)>) {
    let subscriptions = ex.query("SELECT s.consumer_key,s.enabled,ARRAY(SELECT k.kind FROM game.progress_subscription_kinds k WHERE k.consumer_key=s.consumer_key ORDER BY k.kind) AS kinds FROM game.progress_subscriptions s WHERE s.consumer_key<>$1 ORDER BY s.consumer_key", &[&own]).await.unwrap()
        .into_iter().map(|r| (r.get("consumer_key"), r.get("enabled"), r.get("kinds"))).collect();
    let deliveries = ex.query("SELECT consumer_key,state,attempts FROM game.progress_deliveries WHERE consumer_key<>$1 AND event_id=$2::text::uuid ORDER BY consumer_key", &[&own, &event_id]).await.unwrap()
        .into_iter().map(|r| (r.get("consumer_key"), r.get("state"), r.get("attempts"))).collect();
    (subscriptions, deliveries)
}

#[test]
fn registration_rejects_duplicate_keys_and_kinds() {
    let first = || DurableRegistration { consumer_key: "first", kinds: &["SkillTrained"], mode: DeliveryMode::Direct(handler) };
    assert!(Registry::new(vec![], vec![first(), first()]).is_err());
    assert!(Registry::new(vec![], vec![DurableRegistration { consumer_key: "first", kinds: &["SkillTrained", "SkillTrained"], mode: DeliveryMode::Direct(handler) }]).is_err());
    assert!(Registry::new(vec![], vec![first(), DurableRegistration { consumer_key: "second", kinds: &["SkillTrained"], mode: DeliveryMode::Direct(handler) }]).is_ok());
}

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

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn duplicate_sources_fan_out_and_apply_only_once() {
    crate::tests::init_config();
    let config = crate::config::tests::test_database();
    let db = crate::db::Db::connect(&config).await.unwrap();
    let sde = Sde::load(&db).await.unwrap();
    let (mut client, connection) = tokio_postgres::connect(&config.url, tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let tx = client.transaction().await.unwrap();
    tx.batch_execute("CREATE TEMP TABLE progress_test_effects (id bigserial) ON COMMIT DROP").await.unwrap();
    let registry = Registry::new(vec![], vec![
        DurableRegistration { consumer_key: "progress-test-one", kinds: &["SkillTrained"], mode: DeliveryMode::Direct(handler) },
        DurableRegistration { consumer_key: "progress-test-two", kinds: &["SkillTrained"], mode: DeliveryMode::Direct(handler) },
        crate::services::skill_plans::progress::registration(),
        crate::services::daily_goals::progress::registration(),
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
    let old = crate::dao::progress::insert_batch(&tx, std::slice::from_ref(&previous)).await.unwrap();
    crate::dao::progress::reconcile(&tx, &registry).await.unwrap();
    crate::dao::progress::reconcile(&tx, &registry).await.unwrap();
    assert!(crate::dao::progress::reconcile(&tx, &Registry::default()).await.is_err());
    let old_receipts: i64 = tx.query_one("SELECT count(*) AS count FROM game.progress_deliveries WHERE event_id=$1::text::uuid AND consumer_key IN ('progress-test-one','progress-test-two')", &[&old[0].1]).await.unwrap().get("count");
    assert_eq!(old_receipts, 0);
    let inserted = crate::dao::progress::insert_batch(&tx, &batch).await.unwrap();
    let duplicate = crate::dao::progress::insert_batch(&tx, &batch).await.unwrap();
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
    crate::dao::progress::failed(&tx, &pending, 10, "poison").await.unwrap();
    assert!(!crate::dao::progress::try_claim(&tx, &pending, 10).await.unwrap());
    let mut next = batch[0].clone();
    next.source_key = "next".into();
    let next = crate::dao::progress::insert_batch(&tx, &[next]).await.unwrap();
    let foreign_before = foreign_snapshot(&tx, "progress-test-one", &next[0].1).await;
    let due = crate::dao::progress::due(&tx, 10, 64, &["progress-test-one", "progress-test-two"]).await.unwrap();
    let scoped = crate::dao::progress::due(&tx, 10, 64, &["progress-test-one"]).await.unwrap();
    assert!(scoped.iter().all(|key| key.consumer_key == "progress-test-one"));
    assert!(scoped.iter().any(|key| key.event_id == next[0].1));
    assert!(foreign_before.1.iter().any(|(consumer, state, _)| consumer == "progress-test-two" && state == "pending"));
    assert_eq!(foreign_snapshot(&tx, "progress-test-one", &next[0].1).await, foreign_before);
    assert!(due.iter().any(|key| key.event_id == next[0].1));
    assert!(!due.iter().any(|key| key.event_id == pending.event_id && key.consumer_key == pending.consumer_key));
    let row = tx.query_one("SELECT attempts,next_attempt_at,last_error FROM game.progress_deliveries WHERE consumer_key=$1 AND event_id=$2::text::uuid", &[&pending.consumer_key, &pending.event_id]).await.unwrap();
    assert_eq!(row.get::<_, i32>("attempts"), 1);
    assert_eq!(row.get::<_, String>("last_error"), "poison");
    assert!(crate::dao::progress::try_claim(&tx, &pending, row.get("next_attempt_at")).await.unwrap());
    tx.rollback().await.unwrap();
}

static ORDER: std::sync::Mutex<Vec<i64>> = std::sync::Mutex::new(Vec::new());
fn observe_order(event: &GameEvent) {
    if let GameEvent::CharacterSelected { user_id, .. } = event {
        ORDER.lock().unwrap().push(*user_id);
    }
}
struct Nested(Outbox);
impl Prepare for Nested {
    fn prepare<'a, 'tx>(&'a mut self, tx: &'a Transaction<'tx>, context: &'a mut PrepareCx) -> TxFuture<'a, (), PrepareError> {
        self.0.prepare(tx, context)
    }
}
impl AfterCommit for Nested {
    fn after_commit(&mut self, registry: &Registry, committed: &HashSet<String>) -> crate::progress::AfterCommitHandle {
        self.0.after_commit(registry, committed)
    }
}

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
        let result = crate::dao::progress::insert_batch(&tx, &batch).await.unwrap();
        tx.commit().await.unwrap();
        result
    }
    let (first, second) = tokio::join!(insert(&config.url, batch.clone()), insert(&config.url, batch.clone()));
    assert_eq!(first[0].1, second[0].1);
    assert_ne!(first[0].2, second[0].2);
    db.execute("DELETE FROM game.progress_deliveries WHERE event_id=$1::text::uuid", &[&first[0].1]).await.unwrap();
    db.execute("DELETE FROM game.progress_events WHERE event_id=$1::text::uuid", &[&first[0].1]).await.unwrap();
}

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
            tx.batch_execute(&format!("CREATE TABLE {schema}.legacy_state (id bigint PRIMARY KEY); INSERT INTO {schema}.legacy_state VALUES (1)")).await.unwrap();
        }
        let ddl = progress_ddl.replace("game.", &format!("{schema}."));
        tx.batch_execute(&ddl).await.unwrap();
        tx.batch_execute(&ddl).await.unwrap();
        let tables: i64 = tx.query_one("SELECT count(*) AS count FROM information_schema.tables WHERE table_schema=$1 AND table_name IN ('progress_events','progress_subscriptions','progress_subscription_kinds','progress_deliveries')", &[&schema]).await.unwrap().get("count");
        assert_eq!(tables, 4);
        let index: bool = tx.query_one("SELECT EXISTS (SELECT 1 FROM pg_indexes WHERE schemaname=$1 AND indexname='progress_deliveries_subject_pending') AS present", &[&schema]).await.unwrap().get("present");
        assert!(index);
        if existing {
            let row = tx.query_one(&format!("SELECT id FROM {schema}.legacy_state"), &[]).await.unwrap();
            assert_eq!(row.get::<_, i64>("id"), 1);
        }
    }
    tx.rollback().await.unwrap();
}


fn restart_handler<'a, 'tx>(tx: &'a Transaction<'tx>, _: &'a mut Outbox, _: &'a Sde, _: &'a EventEnvelope) -> TxFuture<'a, (), CallError> {
    Box::pin(async move {
        tx.execute("UPDATE game.progress_subscriptions SET version=version+1 WHERE consumer_key='progress-restart-test'", &[]).await.map_err(crate::db::Error::from)?;
        Ok(())
    })
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn pending_receipt_is_polled_after_reconnect() {
    crate::tests::init_config();
    let config = crate::config::tests::test_database();
    let registration = || DurableRegistration { consumer_key: "progress-restart-test", kinds: &["CharacterSelected"], mode: DeliveryMode::Direct(restart_handler) };
    let db = crate::db::Db::connect(&config).await.unwrap()
        .with_progress_registry(Registry::new(vec![], vec![registration()]).unwrap());
    register_fixture(&db, &registration()).await;
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
            Box::pin(async move { crate::dao::progress::insert_batch(tx, &batch).await })
        },
        |rows, ()| async { Ok(rows) },
    ).await.unwrap();
    drop(db);
    let db = crate::db::Db::connect(&config).await.unwrap()
        .with_progress_registry(Registry::new(vec![], vec![registration()]).unwrap());
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
