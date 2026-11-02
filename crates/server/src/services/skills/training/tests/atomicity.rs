use crate::services::skills::completion::{self, KIND};
use crate::services::tests::{client, fixture};
use crate::services::{skills, Call};
use marshal::Value;

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn shared_completion_transaction_rolls_back_after_database_failure() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        fixture::seed_skill(&test_db, f.character).await.unwrap();
        let before = fixture::skill_fingerprint(&test_db, f.character).await.unwrap();
        let (mut c, _peer) = client(test_db.clone(), f.character).await;
        for method in ["GetSkills", "GetSkillQueue"] {
            let kwargs = Value::None;
            let call = Call { service: "skillHandler", method, args: &[], kwargs: &kwargs, bound: true, bind_params: None };
            assert!(skills::dispatch(&mut c, &call).await.unwrap().is_ok());
            assert_eq!(fixture::skill_fingerprint(&test_db, f.character).await.unwrap(), before);
        }
        let job = format!("{KIND}:{}", f.character);
        let before_all = snapshot(&test_db, f.character, &job).await;
        let sde = c.sde.clone();
        let dispatched = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = dispatched.clone();
        let _market = crate::services::market::lock().await;
        let failed: Result<(), crate::services::CallError> = test_db.transaction(|tx, outbox: &mut crate::db::outbox::Outbox<crate::services::effects::online::OnlineChanged>| {
            let sde = sde.clone();
            Box::pin(async move {
                crate::dao::characters::selection::lock_character(tx, f.character).await.map_err(crate::services::db_err)?;
                let now = crate::client::filetime_now();
                let progress = completion::advance_tx(tx, outbox, &sde, f.character, now).await?;
                completion::finish_tx(tx, outbox, &sde, f.character, now, &progress, &[]).await?;
                assert_eq!(progress.completed, vec![(1, 1)]);
                tx.query_one("SELECT 1 / 0", &[]).await.map_err(|error| crate::services::db_err(crate::db::Error::from(error)))?;
                Ok(())
            })
        }, |(), _| async move { flag.store(true, std::sync::atomic::Ordering::SeqCst); Ok(()) }).await;
        fixture::assert_division_by_zero(failed);
        assert!(!dispatched.load(std::sync::atomic::Ordering::SeqCst));
        assert_eq!(snapshot(&test_db, f.character, &job).await, before_all);
        assert_eq!(fixture::skill_fingerprint(&test_db, f.character).await.unwrap(), before);
        assert!(c.outbox.is_empty());
    }).await;
    fixture::cleanup(&db, f).await.unwrap();
    assert_eq!(fixture::remaining(&db, f).await.unwrap(), 0);
    assert!(result.is_ok(), "skill rollback task failed: {result:?}");
}

const SNAPSHOT: &str = "SELECT (SELECT COALESCE(string_agg(row_to_json(s)::text, '' ORDER BY type_id), '') FROM game.skills s WHERE character_id=$1) || '|' || (SELECT COALESCE(string_agg(row_to_json(q)::text, '' ORDER BY position), '') FROM game.skill_queue q WHERE character_id=$1) || '|' || (SELECT COALESCE(string_agg(row_to_json(h)::text, '' ORDER BY history_id), '') FROM game.skill_history h WHERE character_id=$1) || '|' || COALESCE((SELECT row_to_json(j)::text FROM game.jobs j WHERE job_id=$2), '') AS snapshot";

async fn snapshot(db: &crate::db::Db, character: i64, job: &str) -> String {
    db.query_one(SNAPSHOT, &[&character, &job]).await.unwrap().get("snapshot")
}

fn queue(entries: &[(i64, i64, i64)]) -> Value {
    Value::Dict(entries.iter().map(|&(position, skill, level)|
        (Value::Int(position), Value::tuple([Value::Int(skill), Value::Int(level)]))).collect())
}

fn check_error(result: Result<(), crate::services::CallError>, expected: &str, context: Option<(&str, Value)>) {
    match result {
        Err(crate::services::CallError::User { msg, dict }) => {
            assert_eq!(msg, expected);
            if let Some((key, value)) = context {
                assert!(dict.iter().any(|(k, v)| k == key && v == &value), "missing error context {key}");
            }
        }
        _ => panic!("expected UserError {expected}"),
    }
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn refused_saves_roll_back_completion_history_jobs_and_effects() {
    use super::super::{save_queue, save_queue_tx, MAX_QUEUE_ENTRIES, UE_TYPEID};
    use crate::{consts::{CATEGORY_SKILL, dogma::MAX_SKILL_LEVEL}, db::outbox::Outbox, messages as m, services::CallError};
    use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (c, _peer) = client(test_db.clone(), f.character).await;
        let reqs = |id| crate::services::skills::requirements::requirements(&c.sde, id).collect::<Vec<_>>();
        let mut ids: Vec<_> = c.sde.types.iter().filter(|t| t.category_id == Some(CATEGORY_SKILL)).map(|t| t.type_id).collect();
        ids.sort_unstable();
        let (dependent, required) = ids.iter().find_map(|&id| {
            let r = reqs(id);
            (r.len() == 1 && r[0].1 == 1 && r[0].0 != id && ids.binary_search(&r[0].0).is_ok()).then(|| (id, r[0].0))
        }).expect("SDE needs a skill with one level-one prerequisite");
        let plain: Vec<_> = ids.iter().copied().filter(|id| *id != dependent && *id != required && reqs(*id).is_empty()).take(3).collect();
        assert_eq!(plain.len(), 3, "SDE needs three independent skills");
        let (overdue, skill, trained) = (plain[0], plain[1], plain[2]);
        for (id, level) in [(overdue, 0_i32), (skill, 0), (trained, 1), (dependent, 0), (required, 0)] {
            test_db.execute("INSERT INTO game.skills (character_id, type_id, level, skill_points) VALUES ($1, $2, $3, 0)", &[&f.character, &id, &level]).await.unwrap();
        }
        test_db.execute("INSERT INTO game.skill_queue (character_id, position, type_id, to_level, start_sp, dest_sp, start_time, end_time) VALUES ($1, 0, $2, 1, 0, 100, 0, 1)", &[&f.character, &overdue]).await.unwrap();
        let job = format!("{KIND}:{}", f.character);
        completion::reschedule(&test_db, f.character).await.unwrap_or_else(|_| panic!("seed job"));
        let before = snapshot(&test_db, f.character, &job).await;
        let skill = i64::from(skill);
        let pair = Value::tuple([Value::Int(skill), Value::Int(1)]);
        let malformed = vec![
            Value::None,
            Value::Dict(vec![(Value::Int(0), Value::tuple([Value::Int(skill)]))]),
            Value::Dict(vec![(Value::Int(-1), pair.clone())]),
            Value::Dict(vec![(Value::str("0"), Value::tuple([Value::Int(skill), Value::Int(1)]))]),
            Value::Dict(vec![(Value::Int(0), Value::tuple([Value::Int(skill), Value::None]))]),
            Value::Dict(vec![(Value::Int(0), pair.clone()), (Value::Int(0), pair.clone())]),
            Value::Dict(vec![(Value::Int(1), pair)]),
        ];
        for input in malformed {
            check_error(save_queue(&crate::services::identity::Identity::of_client(&c), &c.db, &c.sde, &input, true, true, |n| crate::services::effects::online::dispatch_notification(n, &c.world, &c.notify_tx, c.session.int("charid"))).await, m::MSG_QUEUE_INVALID_POSITION, None);
            assert_eq!(snapshot(&test_db, f.character, &job).await, before);
            assert!(c.outbox.is_empty());
        }
        let unlearned = ids.iter().copied().find(|id| ![overdue, skill as i32, trained, dependent, required].contains(id)).unwrap();
        let dep = i64::from(dependent);
        let req = i64::from(required);
        let cases = vec![
            (vec![(0, i64::from(i32::MAX), 1)], m::MSG_QUEUE_SKILL_NOT_UPLOADED, None),
            (vec![(0, i64::from(unlearned), 1)], m::MSG_QUEUE_SKILL_NOT_UPLOADED, None),
            (vec![(0, skill, 0)], m::MSG_QUEUE_PREVIOUSLY_TRAINED, None),
            (vec![(0, skill, i64::from(MAX_SKILL_LEVEL) + 1)], m::MSG_QUEUE_PAST_MAXIMUM, Some(("typeName", Value::tuple([Value::Int(UE_TYPEID), Value::Int(skill)])))),
            (vec![(0, i64::from(trained), 1)], m::MSG_QUEUE_PREVIOUSLY_TRAINED, None),
            (vec![(0, skill, 1), (1, skill, 2), (2, skill, 1)], m::MSG_QUEUE_ALREADY_PRESENT, None),
            (vec![(0, skill, 2)], m::MSG_QUEUE_LEVELS_OUT_OF_ORDER, None),
            (vec![(0, skill, 1), (1, skill, 3)], m::MSG_QUEUE_LEVELS_OUT_OF_ORDER, None),
            (vec![(0, skill, 1), (1, skill, 1)], m::MSG_QUEUE_ALREADY_PRESENT, None),
            (vec![(0, dep, 1)], m::MSG_QUEUE_BEFORE_REQUIREMENTS, None),
            (vec![(0, dep, 1), (1, req, 1)], m::MSG_QUEUE_AFTER_DEPENDENTS, None),
            ((0..=MAX_QUEUE_ENTRIES).map(|i| (i as i64, skill, 1)).collect(), m::MSG_QUEUE_TOO_MANY, Some(("num", Value::Int(MAX_QUEUE_ENTRIES as i64)))),
        ];
        for (entries, expected, context) in cases {
            let dispatched = Arc::new(AtomicBool::new(false));
            let flag = dispatched.clone();
            let sde = c.sde.clone();
            let _market = crate::services::market::lock().await;
            let result: Result<(), CallError> = test_db.transaction(|tx, outbox: &mut Outbox<crate::services::effects::online::OnlineChanged>| {
                let sde = sde.clone();
                let entries = entries.clone();
                Box::pin(async move {
                    save_queue_tx(tx, outbox, &sde, f.character, entries, true, true, crate::client::filetime_now(), Default::default(), &[]).await?;
                    Ok(())
                })
            }, |(), _outbox| async move { flag.store(true, Ordering::SeqCst); Ok(()) }).await;
            check_error(result, expected, context);
            assert!(!dispatched.load(Ordering::SeqCst), "{expected} dispatched");
            assert!(c.outbox.is_empty());
            assert_eq!(snapshot(&test_db, f.character, &job).await, before, "{expected} mutated state");
        }
    }).await;
    let job = format!("{KIND}:{}", f.character);
    crate::scheduler::cancel(&db, &job).await.unwrap();
    db.execute("DELETE FROM game.skill_history WHERE character_id=$1", &[&f.character]).await.unwrap();
    fixture::cleanup(&db, f).await.unwrap();
    assert!(result.is_ok(), "queue refusal task failed: {result:?}");
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn saves_150_real_skill_levels_and_refuses_151_without_mutation() {
    use super::super::{save_queue, MAX_QUEUE_ENTRIES};
    use crate::{consts::{CATEGORY_SKILL, dogma::MAX_SKILL_LEVEL}, messages as m};
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (c, _peer) = client(test_db.clone(), f.character).await;
        let mut ids: Vec<_> = c.sde.types.iter().filter(|t| t.category_id == Some(CATEGORY_SKILL)).map(|t| t.type_id).collect();
        ids.sort_unstable();
        let count = MAX_QUEUE_ENTRIES / MAX_SKILL_LEVEL as usize;
        assert!(ids.len() > count);
        let selected = &ids[..count];
        for &id in &ids {
            let level = if selected.binary_search(&id).is_ok() { 0_i32 } else { MAX_SKILL_LEVEL };
            test_db.execute("INSERT INTO game.skills (character_id, type_id, level, skill_points) VALUES ($1, $2, $3, 0)", &[&f.character, &id, &level]).await.unwrap();
        }
        let mut pending = selected.to_vec();
        let mut ordered = Vec::new();
        while !pending.is_empty() {
            let index = pending.iter().position(|&id| crate::services::skills::requirements::requirements(&c.sde, id)
                .all(|(required, _)| !pending.contains(&required))).expect("selected SDE skills must have acyclic prerequisites");
            ordered.push(pending.remove(index));
        }
        let mut entries = Vec::new();
        for id in ordered {
            for level in 1..=MAX_SKILL_LEVEL {
                entries.push((entries.len() as i64, i64::from(id), i64::from(level)));
            }
        }
        assert_eq!(entries.len(), MAX_QUEUE_ENTRIES);
        save_queue(&crate::services::identity::Identity::of_client(&c), &c.db, &c.sde, &queue(&entries), true, false, |n| crate::services::effects::online::dispatch_notification(n, &c.world, &c.notify_tx, c.session.int("charid"))).await.unwrap_or_else(|_| panic!("150-entry queue rejected"));
        let rows = crate::dao::skills::queue::list(&test_db, f.character).await.unwrap();
        assert_eq!(rows.len(), MAX_QUEUE_ENTRIES);
        let active = crate::dao::skills::queue::active(&test_db, f.character).await.unwrap().unwrap();
        for (row, &(position, type_id, level)) in rows.iter().zip(&entries) {
            assert_eq!((i64::from(row.position), i64::from(row.type_id), i64::from(row.to_level)), (position, type_id, level));
        }
        let job = format!("{KIND}:{}", f.character);
        let due: i64 = test_db.query_one("SELECT due_at FROM game.jobs WHERE job_id=$1", &[&job]).await.unwrap().get("due_at");
        assert_eq!(Some(due), active.end_time);
        let before = snapshot(&test_db, f.character, &job).await;
        entries.push((entries.len() as i64, i64::from(ids[count]), 1));
        check_error(save_queue(&crate::services::identity::Identity::of_client(&c), &c.db, &c.sde, &queue(&entries), true, true, |n| crate::services::effects::online::dispatch_notification(n, &c.world, &c.notify_tx, c.session.int("charid"))).await, m::MSG_QUEUE_TOO_MANY, Some(("num", Value::Int(MAX_QUEUE_ENTRIES as i64))));
        assert_eq!(snapshot(&test_db, f.character, &job).await, before);
        assert!(c.outbox.is_empty());
    }).await;
    let job = format!("{KIND}:{}", f.character);
    crate::scheduler::cancel(&db, &job).await.unwrap();
    db.execute("DELETE FROM game.skill_history WHERE character_id=$1", &[&f.character]).await.unwrap();
    fixture::cleanup(&db, f).await.unwrap();
    assert!(result.is_ok(), "queue capacity task failed: {result:?}");
}
