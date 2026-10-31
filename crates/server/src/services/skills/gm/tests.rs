use super::*;

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

fn assert_changed_batch(outbox: Outbox, character: i64, expected: &[i32]) {
    use crate::db::outbox::{Effect, Notification};
    let mut batches = Vec::new();
    let mut queues = 0;
    let mut brains = 0;
    for effect in outbox.into_entries() {
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
#[ignore = "requires INFINITY_TEST_DATABASE_URL and an existing trained skill"]
async fn database_set_can_lower_and_remove_is_rolled_back() {
    let config = crate::config::tests::test_database();
    let (mut client, connection) = tokio_postgres::connect(&config.url, tokio_postgres::NoTls).await.expect("connect");
    tokio::spawn(async move { connection.await.expect("connection"); });
    let tx = client.transaction().await.expect("transaction");
    let row = tx.query_one("SELECT character_id, type_id FROM game.skills ORDER BY character_id, type_id LIMIT 1 FOR UPDATE", &[]).await.expect("existing skill");
    let character: i64 = row.get("character_id");
    let type_id: i32 = row.get("type_id");
    training::set_level(&tx, character, type_id, 1, 250).await.expect("set level");
    training::set_level(&tx, character, type_id, 0, 0).await.expect("lower level");
    let row = tx.query_one("SELECT level, skill_points FROM game.skills WHERE character_id = $1 AND type_id = $2", &[&character, &type_id]).await.expect("read skill");
    assert_eq!((row.get::<_, i32>("level"), row.get::<_, i64>("skill_points")), (0, 0));
    let removed = training::remove(&tx, character, &[type_id]).await.expect("remove skill");
    assert_eq!(removed.len(), 1);
    assert_eq!(removed[0].type_id, type_id);
    tx.rollback().await.expect("rollback");
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL, loaded SDE and an active character with two skills"]
async fn database_gm_mutations_materialize_overdue_training_before_queue_changes() {
    let mut config = crate::config::tests::test_database();
    config.schema_on_start = false;
    let db = crate::db::Db::connect(&config).await.expect("database pool");
    let sde = Sde::load(&db).await.expect("loaded SDE");
    let (mut client, connection) = tokio_postgres::connect(&config.url, tokio_postgres::NoTls).await.expect("connect");
    tokio::spawn(async move { connection.await.expect("connection"); });
    let tx = client.transaction().await.expect("transaction");
    let rows = tx.query("SELECT character_id, type_id FROM game.skills WHERE character_id = (SELECT s.character_id FROM game.skills s JOIN game.characters c ON c.character_id = s.character_id WHERE NOT c.deleted GROUP BY s.character_id HAVING count(*) >= 2 ORDER BY s.character_id LIMIT 1) ORDER BY type_id LIMIT 2 FOR UPDATE", &[]).await.expect("existing skills");
    assert_eq!(rows.len(), 2);
    let character: i64 = rows[0].get("character_id");
    let other: i32 = rows[0].get("type_id");
    let target: i32 = rows[1].get("type_id");
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
    let mut outbox = Outbox::default();
    assert!(set_levels(&tx, &mut outbox, &sde, character, &[(target, 0)]).await.is_ok());
    assert_changed_batch(outbox, character, &[other, target]);
    let mut outbox: Outbox = Outbox::default();
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
    assert!(remove(&tx, &mut outbox, &sde, character, &[target]).await.is_ok());
    assert!(queue::list(&tx, character).await.expect("read queue").is_empty());
    assert!(super::super::training::advance_training(&tx, character, filetime_now()).await.is_ok());
    assert!(training::current(&tx, character).await.expect("read skills").iter().all(|s| s.type_id != target));
    training::set_level(&tx, character, other, 0, 0).await.expect("reset skill");
    done.type_id = other;
    queue::insert(&tx, character, &done).await.expect("insert unrelated completed skill");
    assert!(set_levels(&tx, &mut outbox, &sde, character, &[(target, 0)]).await.is_ok());
    let skills = training::current(&tx, character).await.expect("read skills");
    let completed = skills.iter().find(|s| s.type_id == other).expect("completed skill");
    assert_eq!((completed.level, completed.skill_points), (1, 250));
    let race = training::race(&tx, character).await.expect("race query").expect("character race");
    let seed = starting_levels(&sde, race).find(|(id, _)| *id != other).expect("distinct starting skill").0;
    training::remove(&tx, character, &[seed]).await.expect("remove starting skill");
    training::set_level(&tx, character, other, 0, 0).await.expect("reset skill");
    queue::clear(&tx, character).await.expect("clear queue");
    queue::insert(&tx, character, &done).await.expect("insert completed skill");
    let mut starters_outbox = Outbox::default();
    assert!(grant_starters(&tx, &mut starters_outbox, &sde, character).await.is_ok());
    assert_changed_batch(starters_outbox, character, &[other, seed]);
    let skills = training::current(&tx, character).await.expect("read skills");
    let completed = skills.iter().find(|s| s.type_id == other).expect("completed skill");
    assert_eq!((completed.level, completed.skill_points), (1, 250));
    tx.rollback().await.expect("rollback");
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL, loaded SDE and an active character with two skills"]
async fn database_gm_set_queues_only_completed_training_event_once() {
    use crate::db::outbox::Effect;
    use crate::lua::events::GameEvent;

    let mut config = crate::config::tests::test_database();
    config.schema_on_start = false;
    let db = crate::db::Db::connect(&config).await.expect("database pool");
    let sde = Sde::load(&db).await.expect("loaded SDE");
    let (mut client, connection) = tokio_postgres::connect(&config.url, tokio_postgres::NoTls).await.expect("connect");
    tokio::spawn(async move { connection.await.expect("connection"); });
    let tx = client.transaction().await.expect("transaction");
    let rows = tx.query("SELECT character_id, type_id FROM game.skills WHERE character_id = (SELECT s.character_id FROM game.skills s JOIN game.characters c ON c.character_id = s.character_id WHERE NOT c.deleted GROUP BY s.character_id HAVING count(*) >= 2 ORDER BY s.character_id LIMIT 1) ORDER BY type_id LIMIT 2 FOR UPDATE", &[]).await.expect("existing skills");
    assert_eq!(rows.len(), 2);
    let character: i64 = rows[0].get("character_id");
    let trained: i32 = rows[0].get("type_id");
    let granted: i32 = rows[1].get("type_id");
    queue::clear(&tx, character).await.expect("clear queue");
    training::set_level(&tx, character, trained, 1, 250).await.expect("reset trained skill");
    let now = filetime_now();
    let mut done = entry(0, trained, Some(now - 30), Some(now - 20));
    done.to_level = 2;
    done.dest_sp = 1415;
    queue::insert(&tx, character, &done).await.expect("insert completed training");

    let mut outbox: Outbox = Outbox::default();
    assert!(set_levels(&tx, &mut outbox, &sde, character, &[(granted, 3)]).await.is_ok(), "grant level");
    assert!(set_levels(&tx, &mut outbox, &sde, character, &[(granted, 3)]).await.is_ok(), "repeat grant");
    tx.rollback().await.expect("rollback without dispatch");
    let events: Vec<_> = outbox.into_entries().into_iter().filter_map(|effect| match effect {
        Effect::Event(event) => Some(event),
        _ => None,
    }).collect();
    assert_eq!(events.len(), 1);
    assert!(matches!(&events[0], GameEvent::SkillTrained { char_id, type_id, level } if *char_id == character && *type_id == trained && *level == 2));
}
