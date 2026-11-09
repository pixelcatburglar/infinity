//! GM skill mutation tests use owned character fixtures and roll back skill changes.

use super::*;
use marshal::Value;
use crate::client::BRAIN_REFRESH;

fn entry(position: i32, type_id: i32, start: Option<i64>, end: Option<i64>) -> queue::QueueEntry {
    queue::QueueEntry { position, type_id, to_level: 1, start_sp: 0, dest_sp: 100, start_time: start, end_time: end }
}

#[test]
fn removing_active_and_future_skills_preserves_other_queue_timing() {
    let rows = vec![entry(0, 1, Some(10), Some(30)), entry(1, 2, Some(30), Some(50)), entry(2, 3, Some(50), Some(80))];
    let kept = retain_queue(rows, &[1, 3].into_iter().collect(), 20);
    assert_eq!(kept.len(), 1);
    assert_eq!((kept[0].position, kept[0].type_id, kept[0].start_time, kept[0].end_time), (0, 2, Some(20), Some(40)));
}

#[test]
fn removing_paused_entry_preserves_unrelated_paused_entry() {
    let rows = vec![entry(0, 1, None, None), entry(1, 2, None, None)];
    let kept = retain_queue(rows, &[1].into_iter().collect(), 20);
    assert_eq!((kept[0].position, kept[0].type_id, kept[0].start_time), (0, 2, None));
}

fn assert_changed_batch(outbox: Outbox<crate::services::effects::online::OnlineChanged>, character: i64, expected: &[i32]) {
    use crate::db::outbox::{Effect, Notification};
    let mut batches = Vec::new();
    let mut queues = 0;
    let mut brains = 0;
    let (effects, _) = crate::db::outbox::tests::split(outbox);
    for effect in effects {
        if let Effect::Notification(Notification::Pilot { char_id, event, args }) = effect {
            assert_eq!(char_id, character);
            if event == "OnServerSkillsChanged" {
                batches.push(args);
            } else if event == "OnNewSkillQueueSaved" {
                queues += 1;
            } else if event == BRAIN_REFRESH {
                brains += 1;
            }
        }
    }
    assert_eq!(batches.len(), 1);
    assert_eq!(queues, 1);
    assert_eq!(brains, 1);
    let Value::Dict(infos) = &batches[0][0] else { panic!("skills dictionary"); };
    for &type_id in expected {
        assert!(infos.iter().any(|(key, _)| matches!(key, Value::Int(id) if *id == type_id as i64)));
    }
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL"]
async fn database_set_can_lower_and_remove_is_rolled_back() {
    let mut config = crate::config::tests::test_database();
    config.schema_on_start = false;
    let db = crate::db::Db::connect(&config).await.expect("database pool");
    let fixture = crate::services::tests::fixture::create(&db).await.expect("character fixture");
    let character = fixture.character;
    let type_id = 1;
    let test_db = db.clone();
    let result = tokio::spawn(async move {
    training::set_level(&test_db, character, type_id, 1, 250).await.expect("seed skill");
    let (mut client, connection) = tokio_postgres::connect(&config.url, tokio_postgres::NoTls).await.expect("connect");
    tokio::spawn(async move { connection.await.expect("connection"); });
    let tx = client.transaction().await.expect("transaction");
    training::set_level(&tx, character, type_id, 1, 250).await.expect("set level");
    training::set_level(&tx, character, type_id, 0, 0).await.expect("lower level");
    let row = tx.query_one("SELECT level, skill_points FROM game.skills WHERE character_id = $1 AND type_id = $2", &[&character, &type_id]).await.expect("read skill");
    assert_eq!((row.get::<_, i32>("level"), row.get::<_, i64>("skill_points")), (0, 0));
    let removed = training::remove(&tx, character, &[type_id]).await.expect("remove skill");
    assert_eq!(removed.len(), 1);
    assert_eq!(removed[0].type_id, type_id);
    tx.rollback().await.expect("rollback");
    let skills = training::current(&test_db, character).await.expect("read rolled back skill");
    assert!(skills.iter().any(|skill| skill.type_id == type_id && skill.level == 1 && skill.skill_points == 250));
    }).await;
    crate::services::tests::fixture::cleanup(&db, fixture).await.expect("cleanup fixture");
    result.expect("database test");
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL and loaded SDE"]
async fn database_gm_mutations_materialize_overdue_training_before_queue_changes() {
    let mut config = crate::config::tests::test_database();
    config.schema_on_start = false;
    let db = crate::db::Db::connect(&config).await.expect("database pool");
    let sde = std::sync::Arc::new(Sde::load(&db).await.expect("loaded SDE"));
    let fixture = crate::services::tests::fixture::create(&db).await.expect("character fixture");
    let character = fixture.character;
    let test_db = db.clone();
    let result = tokio::spawn(async move {
    let (mut client, connection) = tokio_postgres::connect(&config.url, tokio_postgres::NoTls).await.expect("connect");
    tokio::spawn(async move { connection.await.expect("connection"); });
    let _market = crate::services::market::lock().await;
    let world = crate::space::World::new(test_db, sde.clone(), 1);
    let fences = crate::services::effects::online::capture_online_fences(&world, &[character]).await;
    let tx = client.transaction().await.expect("transaction");
    let types: Vec<_> = sde.types.iter()
        .filter(|t| t.category_id == Some(CATEGORY_SKILL) && t.published == Some(true)
            && t.attribute(infinity_core::consts::dogma::ATTR_SKILL_TIME_CONSTANT) == Some(1.0))
        .take(2).map(|t| t.type_id).collect();
    assert_eq!(types.len(), 2);
    let (other, target) = (types[0], types[1]);
    queue::clear(&tx, character).await.expect("clear queue");
    training::set_level(&tx, character, other, 0, 0).await.expect("reset skill");
    training::set_level(&tx, character, target, 0, 0).await.expect("reset target");
    let now = filetime_now();
    let mut done = entry(0, other, Some(now - 30), Some(now - 20));
    done.dest_sp = 250;
    let mut active = entry(1, target, Some(now - 20), Some(now + 1_000_000_000));
    active.dest_sp = 250;
    let future = entry(2, other, Some(now + 1_000_000_000), Some(now + 2_000_000_000));
    for row in [&done, &active, &future] { queue::insert(&tx, character, row).await.expect("insert queue"); }
    let mut outbox: Outbox<crate::services::effects::online::OnlineChanged> = Outbox::default();
    assert!(set_levels(&tx, &mut outbox, &sde, &fences, character, &[(target, 0)], filetime_now()).await.is_ok());
    assert_changed_batch(outbox, character, &[other, target]);
    let mut outbox: Outbox<crate::services::effects::online::OnlineChanged> = Outbox::default();
    let skills = training::current(&tx, character).await.expect("read skills");
    let completed = skills.iter().find(|s| s.type_id == other).expect("completed skill");
    assert_eq!((completed.level, completed.skill_points), (1, 250));
    let changed = skills.iter().find(|s| s.type_id == target).expect("target skill");
    assert_eq!((changed.level, changed.skill_points), (0, 0));
    let remaining = queue::list(&tx, character).await.expect("remaining queue");
    assert_eq!(remaining.len(), 1);
    assert_eq!((remaining[0].position, remaining[0].type_id), (0, other));
    assert!(remaining[0].start_time.unwrap() >= now);
    queue::clear(&tx, character).await.expect("clear queue");
    done.type_id = target;
    queue::insert(&tx, character, &done).await.expect("insert completed target");
    active.position = 1;
    queue::insert(&tx, character, &active).await.expect("insert future target");
    assert!(remove(&tx, &mut outbox, &sde, &fences, character, &[target], filetime_now()).await.is_ok());
    let (effects, events) = crate::db::outbox::tests::split(outbox);
    let mut removed_notices = 0;
    let mut changed_notices = 0;
    let mut brains = 0;
    let trained = events.iter().filter(|event| matches!(event, crate::lua::events::GameEvent::SkillTrained { char_id, type_id, level } if *char_id == character && *type_id == target && *level == 1)).count();
    for effect in effects {
        match effect {
            crate::db::outbox::Effect::Notification(crate::db::outbox::Notification::Pilot { event, .. }) => {
                if event == "OnServerSkillsRemoved" { removed_notices += 1; }
                if event == "OnServerSkillsChanged" { changed_notices += 1; }
                if event == BRAIN_REFRESH { brains += 1; }
            }
            _ => {}
        }
    }
    assert_eq!((removed_notices, changed_notices, brains, trained), (1, 0, 1, 1));
    let mut outbox: Outbox<crate::services::effects::online::OnlineChanged> = Outbox::default();
    assert!(queue::list(&tx, character).await.expect("read queue").is_empty());
    assert!(completion::advance_tx(&tx, &mut outbox, &sde, character, filetime_now()).await.is_ok());
    assert!(training::current(&tx, character).await.expect("read skills").iter().all(|s| s.type_id != target));
    training::set_level(&tx, character, other, 0, 0).await.expect("reset skill");
    done.type_id = other;
    queue::insert(&tx, character, &done).await.expect("insert unrelated completed skill");
    assert!(set_levels(&tx, &mut outbox, &sde, &fences, character, &[(target, 0)], filetime_now()).await.is_ok());
    let skills = training::current(&tx, character).await.expect("read skills");
    let completed = skills.iter().find(|s| s.type_id == other).expect("completed skill");
    assert_eq!((completed.level, completed.skill_points), (1, 250));
    let race = training::race(&tx, character).await.expect("race query").expect("character race");
    let seed = starting_levels(&sde, race).find(|(id, _)| *id != other).expect("distinct starting skill").0;
    training::remove(&tx, character, &[seed]).await.expect("remove starting skill");
    training::set_level(&tx, character, other, 0, 0).await.expect("reset skill");
    queue::clear(&tx, character).await.expect("clear queue");
    queue::insert(&tx, character, &done).await.expect("insert completed skill");
    let mut starters_outbox: Outbox<crate::services::effects::online::OnlineChanged> = Outbox::default();
    assert!(grant_starters(&tx, &mut starters_outbox, &sde, &fences, character, filetime_now()).await.is_ok());
    assert_changed_batch(starters_outbox, character, &[other, seed]);
    let skills = training::current(&tx, character).await.expect("read skills");
    let completed = skills.iter().find(|s| s.type_id == other).expect("completed skill");
    assert_eq!((completed.level, completed.skill_points), (1, 250));
    let history = crate::dao::skills::history::list(&tx, character, 1000).await.expect("history");
    assert!(history.iter().any(|row| row.skill_type_id == target && row.event_type_id == super::super::history::REMOVED && row.absolute_points == 250));
    assert!(history.iter().any(|row| row.skill_type_id == target && row.event_type_id == super::super::history::GM_GIVE && row.absolute_points == 0));
    assert!(history.iter().any(|row| row.skill_type_id == other && row.event_type_id == super::super::history::TRAINING_COMPLETE && row.absolute_points == 250));
    tx.rollback().await.expect("rollback");
    }).await;
    crate::services::tests::fixture::cleanup(&db, fixture).await.expect("cleanup fixture");
    result.expect("database test");
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL and loaded SDE"]
async fn database_gm_set_queues_only_completed_training_event_once() {
    use crate::lua::events::GameEvent;

    let mut config = crate::config::tests::test_database();
    config.schema_on_start = false;
    let db = crate::db::Db::connect(&config).await.expect("database pool");
    let sde = std::sync::Arc::new(Sde::load(&db).await.expect("loaded SDE"));
    let fixture = crate::services::tests::fixture::create(&db).await.expect("character fixture");
    let character = fixture.character;
    let test_db = db.clone();
    let result = tokio::spawn(async move {
    let (mut client, connection) = tokio_postgres::connect(&config.url, tokio_postgres::NoTls).await.expect("connect");
    tokio::spawn(async move { connection.await.expect("connection"); });
    let _market = crate::services::market::lock().await;
    let world = crate::space::World::new(test_db, sde.clone(), 1);
    let fences = crate::services::effects::online::capture_online_fences(&world, &[character]).await;
    let tx = client.transaction().await.expect("transaction");
    let types: Vec<_> = sde.types.iter()
        .filter(|t| t.category_id == Some(CATEGORY_SKILL) && t.published == Some(true)
            && t.attribute(infinity_core::consts::dogma::ATTR_SKILL_TIME_CONSTANT) == Some(1.0))
        .take(2).map(|t| t.type_id).collect();
    assert_eq!(types.len(), 2);
    let (trained, granted) = (types[0], types[1]);
    queue::clear(&tx, character).await.expect("clear queue");
    training::set_level(&tx, character, trained, 1, 250).await.expect("reset trained skill");
    training::set_level(&tx, character, granted, 0, 0).await.expect("seed granted skill");
    let now = filetime_now();
    let mut done = entry(0, trained, Some(now - 30), Some(now - 20));
    done.to_level = 2;
    done.dest_sp = 1415;
    queue::insert(&tx, character, &done).await.expect("insert completed training");

    let mut outbox: Outbox<crate::services::effects::online::OnlineChanged> = Outbox::default();
    assert!(set_levels(&tx, &mut outbox, &sde, &fences, character, &[(granted, 3)], filetime_now()).await.is_ok(), "grant level");
    assert!(set_levels(&tx, &mut outbox, &sde, &fences, character, &[(granted, 3)], filetime_now()).await.is_ok(), "repeat grant");
    tx.rollback().await.expect("rollback without dispatch");
    let (_, events) = crate::db::outbox::tests::split(outbox);
    assert_eq!(events.iter().filter(|event| matches!(event, GameEvent::SkillTrained { char_id, type_id, level } if *char_id == character && *type_id == trained && *level == 2)).count(), 1);
    assert_eq!(events.iter().filter(|event| matches!(event, GameEvent::SkillStateChanged { char_id } if *char_id == character)).count(), 1);
    assert_eq!(events.len(), 2);
    }).await;
    crate::services::tests::fixture::cleanup(&db, fixture).await.expect("cleanup fixture");
    result.expect("database test");
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn gm_state_events_follow_committed_changes_and_ignore_noops() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = std::sync::Arc::new(Sde::load(&db).await.unwrap());
    let fixture = crate::services::tests::fixture::create(&db).await.unwrap();
    let skill = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SKILL) && t.published == Some(true)).unwrap().type_id;
    let now = filetime_now();
    let _market = crate::services::market::lock().await;
    let world = crate::space::World::new(db.clone(), sde.clone(), 1);
    let fences = crate::services::effects::online::capture_online_fences(&world, &[fixture.character]).await;
    let failed: Result<(), CallError> = db.transaction(|tx, outbox: &mut Outbox<crate::services::effects::online::OnlineChanged>| {
        let sde = sde.clone();
        let fences = fences.clone();
        Box::pin(async move {
            set_levels(tx, outbox, &sde, &fences, fixture.character, &[(skill, 1)], now).await?;
            Err(CallError::Internal(anyhow::anyhow!("rollback")))
        })
    }, |(), _| async { Ok(()) }).await;
    assert!(failed.is_err());
    assert!(crate::db::outbox::tests::events(&db, "SkillStateChanged", fixture.character).await.is_empty());
    for _ in 0..2 {
        db.transaction(|tx, outbox: &mut Outbox<crate::services::effects::online::OnlineChanged>| {
            let sde = sde.clone();
            let fences = fences.clone();
            Box::pin(async move { set_levels(tx, outbox, &sde, &fences, fixture.character, &[(skill, 1)], now).await })
        }, |(), _| async { Ok::<_, CallError>(()) }).await.unwrap();
    }
    assert_eq!(crate::db::outbox::tests::events(&db, "SkillStateChanged", fixture.character).await.len(), 1);
    for _ in 0..2 {
        db.transaction(|tx, outbox: &mut Outbox<crate::services::effects::online::OnlineChanged>| {
            let sde = sde.clone();
            let fences = fences.clone();
            Box::pin(async move { remove(tx, outbox, &sde, &fences, fixture.character, &[skill], now).await })
        }, |(), _| async { Ok::<_, CallError>(()) }).await.unwrap();
    }
    assert_eq!(crate::db::outbox::tests::events(&db, "SkillStateChanged", fixture.character).await.len(), 2);
    db.execute("DELETE FROM game.skill_history WHERE character_id=$1", &[&fixture.character]).await.unwrap();
    crate::services::tests::fixture::cleanup(&db, fixture).await.unwrap();
}
