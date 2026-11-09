//! Durable milestone consumer behavior against the progress journal and real database.
use crate::{dao::{progress as journal, skills::{plan_milestones, plans}}, db::outbox::Outbox, lua::events::GameEvent, progress::{self, ApplyOutcome, DeliveryKey}, services::tests::fixture};

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn milestone_delivery_ack_and_snapshot_rollback_retry_and_reordering() {
    crate::tests::init_config();
    let config = crate::config::tests::test_database();
    let db = crate::db::Db::connect(&config).await.unwrap();
    let sde = crate::sde::Sde::load(&db).await.unwrap();
    let owner = fixture::create(&db).await.unwrap();
    let skill = sde.types.iter().find(|t| super::validation::valid_skill(&sde, t.type_id) && sde.skill_plans.requirements(t.type_id).is_empty()).unwrap().type_id;
    let rank = crate::services::skills::training::skill_type(&sde, skill).rank;
    let points = crate::services::skills::formulas::sp_for_level(rank, 1);
    let (mut client, connection) = tokio_postgres::connect(&config.url, tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let tx = client.transaction().await.unwrap();
    let plan = [crate::handshake::random_u64().to_be_bytes(), crate::handshake::random_u64().to_be_bytes()].concat();
    let milestone = [crate::handshake::random_u64().to_be_bytes(), crate::handshake::random_u64().to_be_bytes()].concat();
    plans::create(&tx, &plan, owner.character, "plan", "", &[skill], &[1]).await.unwrap();
    plan_milestones::create(&tx, &milestone, &plan, "skill", skill, Some(1)).await.unwrap();
    tx.execute("INSERT INTO game.skills (character_id,type_id,level,skill_points) VALUES ($1,$2,0,0)", &[&owner.character, &skill]).await.unwrap();
    super::progress::reconcile_tx(&tx, &sde, owner.character, None).await.unwrap();
    let registration = super::progress::registration();
    tx.execute("INSERT INTO game.progress_subscriptions (consumer_key,generation,activated_at,version,enabled) VALUES ($1,1,0,1,true) ON CONFLICT (consumer_key) DO NOTHING", &[&registration.consumer_key]).await.unwrap();
    for kind in registration.kinds {
        tx.execute("INSERT INTO game.progress_subscription_kinds (consumer_key,kind) VALUES ($1,$2) ON CONFLICT DO NOTHING", &[&registration.consumer_key, kind]).await.unwrap();
    }
    let namespace = format!("milestone-test-{}", crate::handshake::random_u64());
    let events = [GameEvent::SkillTrained { char_id: owner.character, type_id: skill, level: 1 }, GameEvent::SkillStateChanged { char_id: owner.character }];
    let mut keys = Vec::new();
    for (index, event) in events.iter().enumerate() {
        let entry = journal::JournalEvent { source_namespace: namespace.clone(), source_key: index.to_string(), schema_version: progress::codec::SCHEMA_VERSION, kind: event.kind().into(), occurred_at: 1, recorded_at: 1, payload: progress::codec::encode(event).unwrap(), context: "{}".into(), origin: "test".into(), ordinal: index as i64, subject_character_id: Some(owner.character) };
        let inserted = journal::insert_batch(&tx, &[entry]).await.unwrap();
        keys.push(DeliveryKey { consumer_key: registration.consumer_key.into(), event_id: inserted[0].1.clone() });
    }
    tx.execute("UPDATE game.skills SET level=1, skill_points=$3 WHERE character_id=$1 AND type_id=$2", &[&owner.character, &skill, &points]).await.unwrap();
    tx.batch_execute("SAVEPOINT before_delivery").await.unwrap();
    assert!(matches!(progress::apply_once_tx(&tx, &mut Outbox::default(), &sde, &registration, keys[1].clone(), None, progress::ClaimMode::Blocking).await.unwrap(), ApplyOutcome::Applied));
    let snapshot = tx.query_one("SELECT completed FROM game.skill_plan_milestone_progress WHERE character_id=$1 AND milestone_id=$2", &[&owner.character, &milestone]).await.unwrap();
    assert!(snapshot.get::<_, bool>("completed"));
    tx.batch_execute("ROLLBACK TO SAVEPOINT before_delivery").await.unwrap();
    let snapshot = tx.query_one("SELECT completed FROM game.skill_plan_milestone_progress WHERE character_id=$1 AND milestone_id=$2", &[&owner.character, &milestone]).await.unwrap();
    assert!(!snapshot.get::<_, bool>("completed"));
    let receipt = tx.query_one("SELECT state FROM game.progress_deliveries WHERE consumer_key=$1 AND event_id=$2::text::uuid", &[&keys[1].consumer_key, &keys[1].event_id]).await.unwrap();
    assert_eq!(receipt.get::<_, String>("state"), "pending");
    assert!(matches!(progress::apply_once_tx(&tx, &mut Outbox::default(), &sde, &registration, keys[1].clone(), None, progress::ClaimMode::Blocking).await.unwrap(), ApplyOutcome::Applied));
    assert!(matches!(progress::apply_once_tx(&tx, &mut Outbox::default(), &sde, &registration, keys[1].clone(), None, progress::ClaimMode::Blocking).await.unwrap(), ApplyOutcome::AlreadyApplied));
    tx.execute("UPDATE game.skills SET level=0, skill_points=0 WHERE character_id=$1 AND type_id=$2", &[&owner.character, &skill]).await.unwrap();
    assert!(matches!(progress::apply_once_tx(&tx, &mut Outbox::default(), &sde, &registration, keys[0].clone(), None, progress::ClaimMode::Blocking).await.unwrap(), ApplyOutcome::Applied));
    let snapshot = tx.query_one("SELECT completed,trained_sp FROM game.skill_plan_milestone_progress WHERE character_id=$1 AND milestone_id=$2", &[&owner.character, &milestone]).await.unwrap();
    assert!(!snapshot.get::<_, bool>("completed"));
    assert_eq!(snapshot.get::<_, i64>("trained_sp"), 0);
    for key in &keys {
        let receipt = tx.query_one("SELECT state FROM game.progress_deliveries WHERE consumer_key=$1 AND event_id=$2::text::uuid", &[&key.consumer_key, &key.event_id]).await.unwrap();
        assert_eq!(receipt.get::<_, String>("state"), "applied");
    }
    tx.rollback().await.unwrap();
    fixture::cleanup(&db, owner).await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn recovery_reconciles_a_missed_skill_state_event() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = std::sync::Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let owner = fixture::create(&db).await.unwrap();
    let skill = sde.types.iter().find(|t| super::validation::valid_skill(&sde, t.type_id) && sde.skill_plans.requirements(t.type_id).is_empty()).unwrap().type_id;
    let points = crate::services::skills::formulas::sp_for_level(crate::services::skills::training::skill_type(&sde, skill).rank, 1);
    let plan = [crate::handshake::random_u64().to_be_bytes(), crate::handshake::random_u64().to_be_bytes()].concat();
    let milestone = [crate::handshake::random_u64().to_be_bytes(), crate::handshake::random_u64().to_be_bytes()].concat();
    plans::create(&db, &plan, owner.character, "plan", "", &[skill], &[1]).await.unwrap();
    plan_milestones::create(&db, &milestone, &plan, "skill", skill, Some(1)).await.unwrap();
    db.execute("INSERT INTO game.skills(character_id,type_id,level,skill_points) VALUES ($1,$2,0,0)", &[&owner.character, &skill]).await.unwrap();
    super::recovery::character(&db, &sde, owner.character).await.unwrap();
    db.execute("UPDATE game.skills SET level=1,skill_points=$3 WHERE character_id=$1 AND type_id=$2", &[&owner.character, &skill, &points]).await.unwrap();
    super::recovery::character(&db, &sde, owner.character).await.unwrap();
    let row = db.query_one("SELECT completed,trained_sp FROM game.skill_plan_milestone_progress WHERE character_id=$1 AND milestone_id=$2", &[&owner.character, &milestone]).await.unwrap();
    assert!(row.get::<_, bool>("completed"));
    assert_eq!(row.get::<_, i64>("trained_sp"), points);
    plans::delete(&db, owner.character, &plan).await.unwrap();
    fixture::cleanup(&db, owner).await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn stored_cyclic_target_delivery_is_invalid_and_acknowledged() {
    crate::tests::init_config();
    let config = crate::config::tests::test_database();
    let db = crate::db::Db::connect(&config).await.unwrap();
    let sde = crate::sde::Sde::load(&db).await.unwrap();
    let owner = fixture::create(&db).await.unwrap();
    // Native SDE Polaris skill, an unpublished self-loop.
    let cyclic = 9955;
    let ty = sde.types.get(cyclic).expect("Polaris skill in SDE");
    assert_eq!(ty.published, Some(false));
    assert!(sde.skill_plans.is_cyclic(cyclic));
    let (mut client, connection) = tokio_postgres::connect(&config.url, tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let tx = client.transaction().await.unwrap();
    let plan = [crate::handshake::random_u64().to_be_bytes(), crate::handshake::random_u64().to_be_bytes()].concat();
    let milestone = [crate::handshake::random_u64().to_be_bytes(), crate::handshake::random_u64().to_be_bytes()].concat();
    plans::create(&tx, &plan, owner.character, "legacy", "", &[], &[]).await.unwrap();
    plan_milestones::create(&tx, &milestone, &plan, "skill", cyclic, Some(1)).await.unwrap();
    crate::dao::skills::plan_progress::upsert(&tx, owner.character, &[crate::dao::skills::plan_progress::Snapshot { milestone_id: milestone.clone(), required_sp: 0, trained_sp: 0, queued_sp: 0, completed: false, valid: true }], 1).await.unwrap();
    let registration = super::progress::registration();
    tx.execute("INSERT INTO game.progress_subscriptions (consumer_key,generation,activated_at,version,enabled) VALUES ($1,1,0,1,true) ON CONFLICT (consumer_key) DO NOTHING", &[&registration.consumer_key]).await.unwrap();
    for kind in registration.kinds {
        tx.execute("INSERT INTO game.progress_subscription_kinds (consumer_key,kind) VALUES ($1,$2) ON CONFLICT DO NOTHING", &[&registration.consumer_key, kind]).await.unwrap();
    }
    let event = GameEvent::SkillStateChanged { char_id: owner.character };
    let entry = journal::JournalEvent { source_namespace: format!("milestone-cycle-test-{}", crate::handshake::random_u64()), source_key: "0".into(), schema_version: progress::codec::SCHEMA_VERSION, kind: event.kind().into(), occurred_at: 1, recorded_at: 1, payload: progress::codec::encode(&event).unwrap(), context: "{}".into(), origin: "test".into(), ordinal: 0, subject_character_id: Some(owner.character) };
    let inserted = journal::insert_batch(&tx, &[entry]).await.unwrap();
    let key = DeliveryKey { consumer_key: registration.consumer_key.into(), event_id: inserted[0].1.clone() };
    assert!(matches!(progress::apply_once_tx(&tx, &mut Outbox::default(), &sde, &registration, key.clone(), None, progress::ClaimMode::Blocking).await.unwrap(), ApplyOutcome::Applied));
    let row = tx.query_one("SELECT valid,completed,required_sp,trained_sp,queued_sp FROM game.skill_plan_milestone_progress WHERE character_id=$1 AND milestone_id=$2", &[&owner.character, &milestone]).await.unwrap();
    assert!(!row.get::<_, bool>("valid"));
    assert!(!row.get::<_, bool>("completed"));
    for field in ["required_sp", "trained_sp", "queued_sp"] { assert_eq!(row.get::<_, i64>(field), 0); }
    let receipt = tx.query_one("SELECT state FROM game.progress_deliveries WHERE consumer_key=$1 AND event_id=$2::text::uuid", &[&key.consumer_key, &key.event_id]).await.unwrap();
    assert_eq!(receipt.get::<_, String>("state"), "applied");
    assert!(matches!(progress::apply_once_tx(&tx, &mut Outbox::default(), &sde, &registration, key, None, progress::ClaimMode::Blocking).await.unwrap(), ApplyOutcome::AlreadyApplied));
    tx.rollback().await.unwrap();
    fixture::cleanup(&db, owner).await.unwrap();
}
