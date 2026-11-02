//! Completion event ordering and duplicate suppression for skill training.
use super::*;

mod atomicity;

#[test]
fn malformed_queue_maps_are_refused_without_becoming_aborts() {
    let bad = [
        Value::None,
        Value::Dict(vec![(Value::Int(0), Value::tuple([Value::Int(1)]))]),
        Value::Dict(vec![(Value::Bool(false), Value::tuple([Value::Int(1), Value::Int(1)]))]),
        Value::Dict(vec![(Value::Int(-1), Value::tuple([Value::Int(1), Value::Int(1)]))]),
        Value::Dict(vec![(Value::Int(0), Value::tuple([Value::Int(1), Value::Int(1)])), (Value::Int(0), Value::tuple([Value::Int(2), Value::Int(1)]))]),
    ];
    for value in bad {
        assert!(matches!(parse_queue(&value), Err(CallError::User { msg, .. }) if msg == crate::messages::MSG_QUEUE_INVALID_POSITION));
    }
    assert!(matches!(parse_queue(&Value::Dict(vec![])), Ok(entries) if entries.is_empty()));
    let entries = (0..151).map(|i| (Value::Int(i), Value::tuple([Value::Int(1), Value::Int(1)]))).collect();
    assert!(matches!(parse_queue(&Value::Dict(entries)), Err(CallError::User { msg, .. }) if msg == crate::messages::MSG_QUEUE_TOO_MANY));
}

#[test]
fn queue_wire_preserves_optional_filetimes() {
    let rows = [
        queue_dao::QueueEntry { position: 0, type_id: 42, to_level: 1, start_sp: 10, dest_sp: 20, start_time: None, end_time: None },
        queue_dao::QueueEntry { position: 1, type_id: 43, to_level: 2, start_sp: 20, dest_sp: 30, start_time: Some(132_000_000_000_000_000), end_time: Some(132_000_000_600_000_000) },
    ];
    let expected = Value::List(vec![
        keyval([("queuePosition", Value::Int(0)), ("trainingTypeID", Value::Int(42)), ("trainingToLevel", Value::Int(1)), ("trainingStartSP", Value::Int(10)), ("trainingDestinationSP", Value::Int(20)), ("trainingStartTime", Value::None), ("trainingEndTime", Value::None)]),
        keyval([("queuePosition", Value::Int(1)), ("trainingTypeID", Value::Int(43)), ("trainingToLevel", Value::Int(2)), ("trainingStartSP", Value::Int(20)), ("trainingDestinationSP", Value::Int(30)), ("trainingStartTime", Value::Int(132_000_000_000_000_000)), ("trainingEndTime", Value::Int(132_000_000_600_000_000))]),
    ]);
    assert_eq!(super::super::skill_handler::queue_value(&rows), expected);
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn queue_validation_checks_real_skills_and_capacity() {
    use crate::services::tests::{client, fixture};
    use crate::messages as m;
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    let (c, _peer) = client(db.clone(), f.character).await;
    let ids: Vec<_> = c.sde.types.iter().filter(|t| t.category_id == Some(CATEGORY_SKILL)).take(31).map(|t| t.type_id).collect();
    assert_eq!(ids.len(), 31);
    let current: HashMap<_, _> = ids.iter().map(|&id| (id, (0, 1))).collect();
    let skill = i64::from(ids[0]);
    let error = |entries: &[(i64, i64, i64)]| match validate_queue(&c.sde, entries, &current) {
        Err(CallError::User { msg, .. }) => msg,
        _ => panic!("expected queue refusal"),
    };
    assert_eq!(error(&[(0, i64::from(i32::MAX), 1)]), m::MSG_QUEUE_SKILL_NOT_UPLOADED);
    assert_eq!(error(&[(0, skill, 0)]), m::MSG_QUEUE_PREVIOUSLY_TRAINED);
    assert_eq!(error(&[(0, skill, i64::from(MAX_SKILL_LEVEL) + 1)]), m::MSG_QUEUE_PAST_MAXIMUM);
    assert_eq!(error(&[(0, skill, 2)]), m::MSG_QUEUE_LEVELS_OUT_OF_ORDER);
    assert_eq!(error(&[(0, skill, 1), (1, skill, 1)]), m::MSG_QUEUE_ALREADY_PRESENT);
    let mut entries = Vec::new();
    for &id in &ids[..30] {
        for level in 1..=MAX_SKILL_LEVEL {
            entries.push((entries.len() as i64, i64::from(id), i64::from(level)));
        }
    }
    assert!(validate_queue(&c.sde, &entries, &current).is_ok());
    entries.push((entries.len() as i64, i64::from(ids[30]), 1));
    assert_eq!(error(&entries), m::MSG_QUEUE_TOO_MANY);
    let (dependent, required, required_level) = c.sde.types.iter().filter(|t| t.category_id == Some(CATEGORY_SKILL)).find_map(|t| {
        let mut requirements = super::super::requirements::requirements(&c.sde, t.type_id);
        let (required, level) = requirements.next()?;
        (requirements.next().is_none() && required != t.type_id && level == 1 && c.sde.types.get(required).is_some_and(|r| r.category_id == Some(CATEGORY_SKILL))).then_some((t.type_id, required, level))
    }).expect("SDE must contain a level-one skill prerequisite");
    let prerequisite_state = HashMap::from([(dependent, (0, 0)), (required, (0, 1))]);
    let dependent_entry = (0, i64::from(dependent), 1);
    assert!(matches!(validate_queue(&c.sde, &[dependent_entry], &prerequisite_state), Err(CallError::User { msg, .. }) if msg == m::MSG_QUEUE_BEFORE_REQUIREMENTS));
    let later = [(0, i64::from(dependent), 1), (1, i64::from(required), i64::from(required_level))];
    assert!(matches!(validate_queue(&c.sde, &later, &prerequisite_state), Err(CallError::User { msg, .. }) if msg == m::MSG_QUEUE_AFTER_DEPENDENTS));
    fixture::cleanup(&db, f).await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn queue_replacement_and_abort_bank_progress_and_update_deadline() {
    use crate::services::tests::{client, fixture};
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    fixture::seed_skill(&db, f.character).await.unwrap();
    let now = filetime_now();
    db.execute("UPDATE game.skill_queue SET start_time=$2, end_time=$3 WHERE character_id=$1", &[&f.character, &(now - crate::consts::FILETIME_MINUTE), &(now + crate::consts::FILETIME_MINUTE)]).await.unwrap();
    let (c, _peer) = client(db.clone(), f.character).await;
    let type_id = c.sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SKILL) && super::super::requirements::requirements(&c.sde, t.type_id).next().is_none()).expect("SDE must contain a skill without prerequisites").type_id;
    db.execute("UPDATE game.skills SET type_id=$2 WHERE character_id=$1", &[&f.character, &type_id]).await.unwrap();
    db.execute("UPDATE game.skill_queue SET type_id=$2 WHERE character_id=$1", &[&f.character, &type_id]).await.unwrap();
    let replacement = Value::Dict(vec![(Value::Int(0), Value::tuple([Value::Int(i64::from(type_id)), Value::Int(1)]))]);
    save_queue(&Identity::of_client(&c), &c.db, &c.sde, &replacement, true, true, |n| online::dispatch_notification(n, &c.world, &c.notify_tx, c.session.int("charid"))).await.unwrap_or_else(|_| panic!("failed to replace skill queue"));
    let skills = training_dao::current(&db, f.character).await.unwrap();
    assert!(skills[0].skill_points >= 50);
    let id = format!("{}:{}", super::super::completion::KIND, f.character);
    let row = db.query_one("SELECT due_at FROM game.jobs WHERE job_id=$1", &[&id]).await.unwrap();
    let queued = queue_dao::active(&db, f.character).await.unwrap().unwrap();
    assert_eq!(Some(row.get::<_, i64>("due_at")), queued.end_time);
    save_queue(&Identity::of_client(&c), &c.db, &c.sde, &Value::Dict(vec![]), false, false, |n| online::dispatch_notification(n, &c.world, &c.notify_tx, c.session.int("charid"))).await.unwrap_or_else(|_| panic!("failed to abort skill queue"));
    assert!(queue_dao::list(&db, f.character).await.unwrap().is_empty());
    assert!(db.query_opt("SELECT job_id FROM game.jobs WHERE job_id=$1", &[&id]).await.unwrap().is_none());
    assert!(training_dao::current(&db, f.character).await.unwrap()[0].skill_points >= 50);
    fixture::cleanup(&db, f).await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn queue_save_completion_notifies_once_and_rollback_dispatches_nothing() {
    use crate::db::outbox::{Effect, Notification};
    use crate::lua::events::GameEvent;
    use crate::services::tests::{client, fixture};
    use std::sync::{Arc, atomic::{AtomicBool, Ordering}};

    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let _market = crate::services::market::lock().await;
    for notify in [true, false] {
        let f = fixture::create(&db).await.unwrap();
        fixture::seed_skill(&db, f.character).await.unwrap();
        let before = fixture::skill_fingerprint(&db, f.character).await.unwrap();
        let (c, _peer) = client(db.clone(), f.character).await;
        let sde = c.sde.clone();
        let dispatched = Arc::new(AtomicBool::new(false));
        let flag = dispatched.clone();
        let result: Result<(), CallError> = db.transaction(|tx, outbox: &mut Outbox<crate::services::effects::online::OnlineChanged>| {
            let sde = sde.clone();
            Box::pin(async move {
                save_queue_tx(tx, outbox, &sde, f.character, vec![], false, notify, filetime_now(), Default::default(), &[]).await?;
                Err(CallError::Internal(anyhow::anyhow!("force rollback")))
            })
        }, |(), _outbox| async move {
            flag.store(true, Ordering::SeqCst);
            Ok(())
        }).await;
        assert!(result.is_err());
        assert!(!dispatched.load(Ordering::SeqCst));
        assert_eq!(fixture::skill_fingerprint(&db, f.character).await.unwrap(), before);
        assert!(crate::dao::skills::history::list(&db, f.character, 50).await.unwrap().is_empty());

        let effects = db.transaction(|tx, outbox: &mut Outbox<crate::services::effects::online::OnlineChanged>| {
            let sde = sde.clone();
            Box::pin(async move {
                save_queue_tx(tx, outbox, &sde, f.character, vec![], false, notify, filetime_now(), Default::default(), &[]).await?;
                Ok(())
            })
        }, |(), outbox| async move { Ok::<_, CallError>({ let entries = outbox.into_entries(); assert!(!entries.iter().any(|effect| matches!(effect, crate::db::outbox::Effect::Operation(_)))); entries }) }).await.unwrap_or_else(|_| panic!("failed to save skill queue and collect effects"));
        let mut changed = 0;
        let mut trained = 0;
        let mut brains = 0;
        let mut queues = 0;
        for effect in effects {
            match effect {
                Effect::Notification(Notification::Pilot { char_id, event, .. }) if char_id == f.character => {
                    if event == "OnServerSkillsChanged" { changed += 1; }
                    if event == crate::client::BRAIN_REFRESH { brains += 1; }
                    if event == "OnNewSkillQueueSaved" { queues += 1; }
                }
                Effect::Event(GameEvent::SkillTrained { char_id, type_id: 1, level: 1 }) if char_id == f.character => trained += 1,
                _ => {}
            }
        }
        assert_eq!((changed, trained, brains, queues), (1, 1, 1, 1));
        fixture::cleanup(&db, f).await.unwrap();
    }
}
