//! Remap validation and persistence regressions.
use super::*;
use super::super::completion;
use crate::client::BRAIN_REFRESH;
fn values(values: [i64; 5]) -> Value {
    Value::Dict(CHAR_ATTRIBUTES.into_iter().zip(values).map(|(id, value)| (Value::Int(id), Value::Int(value))).collect())
}
async fn use_real_skill(db: &crate::db::Db, character: i64, sde: &crate::sde::Sde) -> i32 {
    let type_id = sde.types.iter().find(|t| t.category_id == Some(infinity_core::consts::CATEGORY_SKILL) && super::super::requirements::requirements(sde, t.type_id).next().is_none()).expect("SDE must contain a skill without prerequisites").type_id;
    db.execute("UPDATE game.skills SET type_id=$2 WHERE character_id=$1", &[&character, &type_id]).await.unwrap();
    db.execute("UPDATE game.skill_queue SET type_id=$2 WHERE character_id=$1", &[&character, &type_id]).await.unwrap();
    type_id
}

#[test]
fn exact_integer_attributes_and_total_are_required() {
    assert_eq!(parse(&values([17, 17, 17, 21, 27])), Ok([17, 17, 17, 21, 27]));
    assert_eq!(parse(&values([16, 18, 17, 21, 27])), Err(Invalid::Low));
    assert_eq!(parse(&values([28, 17, 17, 20, 17])), Err(Invalid::High));
    assert_eq!(parse(&values([17, 17, 17, 20, 27])), Err(Invalid::Misallocated));
    let mut duplicate = match values([17, 17, 17, 21, 27]) { Value::Dict(pairs) => pairs, _ => unreachable!() };
    duplicate[1].0 = Value::Int(CHAR_ATTRIBUTES[0]);
    assert_eq!(parse(&Value::Dict(duplicate)), Err(Invalid::Malformed));
    let mut unknown = match values([17, 17, 17, 21, 27]) { Value::Dict(pairs) => pairs, _ => unreachable!() };
    unknown[0].0 = Value::Int(999);
    assert_eq!(parse(&Value::Dict(unknown)), Err(Invalid::Malformed));
    let mut float = match values([17, 17, 17, 21, 27]) { Value::Dict(pairs) => pairs, _ => unreachable!() };
    float[0].1 = Value::Float(f64::NAN);
    assert_eq!(parse(&Value::Dict(float)), Err(Invalid::Malformed));
    for (reason, expected) in [(Invalid::Low, infinity_core::messages::MSG_RESPEC_ATTRIBUTES_TOO_LOW), (Invalid::High, infinity_core::messages::MSG_RESPEC_ATTRIBUTES_TOO_HIGH), (Invalid::Misallocated, infinity_core::messages::MSG_RESPEC_ATTRIBUTES_MISALLOCATED)] {
        match invalid_error(reason) { CallError::User { msg, dict } => { assert_eq!(msg, expected); assert!(dict.is_empty()); }, _ => panic!("expected user error") }
    }
}

#[test]
fn shuffled_wire_attributes_follow_remap_order() {
    let value = Value::Dict(vec![
        (Value::Int(168), Value::Int(25)),
        (Value::Int(165), Value::Int(18)),
        (Value::Int(167), Value::Int(20)),
        (Value::Int(164), Value::Int(17)),
        (Value::Int(166), Value::Int(19)),
    ]);
    assert_eq!(parse(&value), Ok([17, 18, 19, 20, 25]));
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn entitlement_persists_and_paused_queue_remains_paused() {
    use crate::{db::Db, services::tests::{client, fixture}};
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    fixture::seed_skill(&db, f.character).await.unwrap();
    db.execute("UPDATE game.skill_queue SET start_time=NULL, end_time=NULL WHERE character_id=$1", &[&f.character]).await.unwrap();
    let (mut c, _peer) = client(db.clone(), f.character).await;
    use_real_skill(&db, f.character, &c.sde).await;
    assert!(apply(&mut c, f.character, &values([17, 17, 17, 20, 27])).await.is_err());
    assert!(attributes::respec(&db, f.character).await.unwrap().is_none());
    get(&mut c, f.character).await.unwrap_or_else(|_| panic!("initialize remap"));
    let initial = attributes::respec(&db, f.character).await.unwrap().unwrap().free_respecs;
    apply(&mut c, f.character, &values([17, 17, 17, 21, 27])).await.unwrap_or_else(|_| panic!("timed remap"));
    let first = attributes::respec(&db, f.character).await.unwrap().unwrap();
    assert_eq!(first.free_respecs, initial);
    assert_eq!(first.bases.as_deref(), Some(&[17, 17, 17, 21, 27][..]));
    let deadline = first.next_timed_respec;
    let effective = super::super::attributes::character_attributes_db(&db, &c.sde, f.character).await.unwrap_or_else(|_| panic!("effective attributes"));
    for (id, base) in CHAR_ATTRIBUTES.into_iter().zip([17, 17, 17, 21, 27]) { assert_eq!(effective[&id], f64::from(base)); }
    let rows = queue::list(&db, f.character).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert!(rows[0].start_time.is_none() && rows[0].end_time.is_none());
    if initial > 0 {
        apply(&mut c, f.character, &values([18, 17, 17, 20, 27])).await.unwrap_or_else(|_| panic!("bonus remap"));
        let second = attributes::respec(&db, f.character).await.unwrap().unwrap();
        assert_eq!(second.free_respecs, initial - 1);
        assert_eq!(second.next_timed_respec, deadline);
        get(&mut c, f.character).await.unwrap_or_else(|_| panic!("read persisted remap"));
        assert_eq!(attributes::respec(&db, f.character).await.unwrap().unwrap().free_respecs, initial - 1);
        db.execute("UPDATE game.character_respec SET free_respecs=0 WHERE character_id=$1", &[&f.character]).await.unwrap();
        match apply(&mut c, f.character, &values([17, 17, 17, 21, 27])).await {
            Err(CallError::User { msg, dict }) => {
                assert_eq!(msg, infinity_core::messages::MSG_RESPEC_TOO_SOON);
                assert!(matches!(dict.as_slice(), [(key, Value::Int(value))] if key == "nextTime" && Some(*value) == deadline));
            }
            _ => panic!("expected timed entitlement refusal"),
        }
        let refused = attributes::respec(&db, f.character).await.unwrap().unwrap();
        assert_eq!(refused.next_timed_respec, deadline);
        assert_eq!(refused.bases.as_deref(), Some(&[18, 17, 17, 20, 27][..]));
    }
    fixture::cleanup(&db, f).await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn active_queue_banks_progress_and_reschedules() {
    use crate::{db::Db, services::tests::{client, fixture}};
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    fixture::seed_skill(&db, f.character).await.unwrap();
    let now = filetime_now();
    db.execute("UPDATE game.skill_queue SET start_time=$2, end_time=$3 WHERE character_id=$1", &[&f.character, &(now - FILETIME_DAY), &(now + FILETIME_DAY)]).await.unwrap();
    let (mut c, _peer) = client(db.clone(), f.character).await;
    use_real_skill(&db, f.character, &c.sde).await;
    apply(&mut c, f.character, &values([17, 17, 17, 21, 27])).await.unwrap_or_else(|_| panic!("remap active queue"));
    let skill = skill_dao::current(&db, f.character).await.unwrap();
    assert!(skill[0].skill_points >= 50);
    let row = queue::active(&db, f.character).await.unwrap().unwrap();
    assert!(row.start_sp >= 50);
    let id = format!("{}:{}", completion::KIND, f.character);
    let job = db.query_one("SELECT due_at FROM game.jobs WHERE job_id=$1", &[&id]).await.unwrap();
    assert_eq!(Some(job.get::<_, i64>("due_at")), row.end_time);
    crate::scheduler::cancel(&db, &id).await.unwrap();
    fixture::cleanup(&db, f).await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn remap_rolls_back_after_staged_writes_and_batches_overdue_completion() {
    use crate::{db::{Db, outbox::{Effect, Notification}}, lua::events::GameEvent, services::tests::{client, fixture}};
    use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    let scenario = crate::tests::catch_scenario(async {
    fixture::seed_skill(&db, f.character).await.unwrap();
    let now = filetime_now();
    db.execute("INSERT INTO game.skill_queue (character_id, position, type_id, to_level, start_sp, dest_sp, start_time, end_time) VALUES ($1, 1, 1, 2, 100, 1415, $2, $3)", &[&f.character, &(now - FILETIME_DAY), &(now + FILETIME_DAY)]).await.unwrap();
    let (c, _peer) = client(db.clone(), f.character).await;
    let type_id = use_real_skill(&db, f.character, &c.sde).await;
    attributes::initialize_respec(&db, f.character, 1).await.unwrap();
    let id = format!("{}:{}", completion::KIND, f.character);
    completion::reschedule(&db, f.character).await.unwrap_or_else(|_| panic!("seed completion job"));
    let snapshot_sql = "SELECT (SELECT row_to_json(r)::text FROM game.character_respec r WHERE character_id=$1) || '|' || (SELECT COALESCE(string_agg(row_to_json(s)::text, '' ORDER BY type_id), '') FROM game.skills s WHERE character_id=$1) || '|' || (SELECT COALESCE(string_agg(row_to_json(q)::text, '' ORDER BY position), '') FROM game.skill_queue q WHERE character_id=$1) || '|' || (SELECT COALESCE(string_agg(row_to_json(h)::text, '' ORDER BY history_id), '') FROM game.skill_history h WHERE character_id=$1) || '|' || (SELECT COALESCE(row_to_json(j)::text, '') FROM game.jobs j WHERE job_id=$2) AS snapshot";
    let before: String = db.query_one(snapshot_sql, &[&f.character, &id]).await.unwrap().get("snapshot");
    let sde = c.sde.clone();
    let fences = crate::services::effects::online::capture_online_fences(&c.world, &[f.character]).await;
    let dispatched = Arc::new(AtomicBool::new(false));
    let flag = dispatched.clone();
    let _market = crate::services::market::lock().await;
    let result: Result<Value, CallError> = db.transaction(|tx, outbox: &mut Outbox<crate::services::effects::online::OnlineChanged>| {
        let sde = sde.clone(); let fences = fences.clone();
        Box::pin(async move {
            let value = apply_tx(tx, outbox, &sde, &fences, f.character, &[17, 17, 17, 21, 27], now).await?;
            let staged = attributes::respec(tx, f.character).await.map_err(db_err)?.expect("staged remap");
            assert_eq!(staged.bases.as_deref(), Some(&[17, 17, 17, 21, 27][..]));
            tx.query_one("SELECT 1 / 0", &[]).await.map_err(|error| db_err(crate::db::Error::from(error)))?;
            Ok(value)
        })
    }, |value, _outbox| async move { flag.store(true, Ordering::SeqCst); Ok(value) }).await;
    match result {
        Err(CallError::Internal(error)) => match error.downcast_ref::<crate::db::Error>() {
            Some(crate::db::Error::Postgres(error)) => assert_eq!(error.code(), Some(&tokio_postgres::error::SqlState::DIVISION_BY_ZERO)),
            _ => panic!("expected a PostgreSQL division-by-zero error"),
        },
        _ => panic!("expected a PostgreSQL division-by-zero error"),
    }
    assert!(!dispatched.load(Ordering::SeqCst));
    let after: String = db.query_one(snapshot_sql, &[&f.character, &id]).await.unwrap().get("snapshot");
    assert_eq!(before, after);
    let effects = db.transaction(|tx, outbox: &mut Outbox<crate::services::effects::online::OnlineChanged>| {
        let sde = sde.clone(); let fences = fences.clone();
        Box::pin(async move { apply_tx(tx, outbox, &sde, &fences, f.character, &[17, 17, 17, 21, 27], now).await })
    }, |_, outbox| async move { Ok::<_, CallError>({ let entries = outbox.into_entries(); assert!(!entries.iter().any(|effect| matches!(effect, crate::db::outbox::Effect::Operation(_)))); entries }) }).await.unwrap_or_else(|_| panic!("remap transaction"));
    let (mut changed, mut brains) = (0, 0);
    let trained = crate::db::outbox::tests::events(&db, "SkillTrained", f.character).await.into_iter().filter(|event| matches!(event, GameEvent::SkillTrained { char_id, type_id: trained_type, level: 1 } if *char_id == f.character && *trained_type == type_id)).count();
    for effect in effects {
        match effect {
            Effect::Notification(Notification::Pilot { char_id, event, .. }) if char_id == f.character => {
                if event == "OnServerSkillsChanged" { changed += 1; }
                if event == BRAIN_REFRESH { brains += 1; }
            }
            _ => {}
        }
    }
    assert_eq!((changed, brains, trained), (1, 1, 1));
    assert!(queue::active(&db, f.character).await.unwrap().is_some());
    }).await;
    let id = format!("{}:{}", completion::KIND, f.character);
    crate::scheduler::cancel(&db, &id).await.unwrap();
    db.execute("DELETE FROM game.skill_history WHERE character_id=$1", &[&f.character]).await.unwrap();
    fixture::cleanup(&db, f).await.unwrap();
    if let Err(panic) = scenario { std::panic::resume_unwind(panic); }
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn stale_entitlement_write_is_refused() {
    use crate::{db::Db, services::tests::fixture};
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    let state = attributes::initialize_respec(&db, f.character, 1).await.unwrap();
    db.execute("UPDATE game.character_respec SET free_respecs=2 WHERE character_id=$1", &[&f.character]).await.unwrap();
    assert!(!attributes::save_respec(&db, f.character, &[17, 17, 17, 21, 27], &state, 0, 1, 2).await.unwrap());
    let current = attributes::respec(&db, f.character).await.unwrap().unwrap();
    assert_eq!(current.free_respecs, 2);
    assert!(current.bases.is_none());
    fixture::cleanup(&db, f).await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn remap_banks_overdue_booster_expiry_at_its_real_time() {
    use infinity_core::{consts::{dogma::ATTR_BOOSTERNESS, dogma::ATTR_SKILL_TIME_CONSTANT, FILETIME_MINUTE, FLAG_BOOSTER}};
    use crate::{services::tests::{client, fixture}};
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    let scenario = crate::tests::catch_scenario(async {
    let (c, _peer) = client(db.clone(), f.character).await;
    let fences = crate::services::effects::online::capture_online_fences(&c.world, &[f.character]).await;
    let existing = skill_dao::current(&db, f.character).await.unwrap();
    let now = filetime_now();
    let start = now - 20 * FILETIME_MINUTE;
    let expiry = now - 10 * FILETIME_MINUTE;
    let mut exercised = false;
    'types: for data in c.sde.types.iter() {
        if data.attribute(ATTR_BOOSTERNESS).is_none() { continue; }
        for entry in data.effects() {
            let Some(effect) = c.sde.dogma.effect(entry.effect_id) else { continue };
            if effect.effect_category.unwrap_or(0) != 0 { continue; }
            for modifier in effect.modifiers() {
                if !CHAR_ATTRIBUTES.contains(&(modifier.modified as i64)) || modifier.modified == modifier.modifying { continue; }
                let item: i64 = db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,singleton,brain_activated_at,brain_expires_at,brain_effect_ids) VALUES ($1,$2,$2,$3,true,$4,$5,ARRAY[$6::integer]) RETURNING item_id", &[&data.type_id, &f.character, &FLAG_BOOSTER, &start, &expiry, &effect.effect_id]).await.unwrap().get("item_id");
                db.execute("INSERT INTO game.item_attributes (item_id,attribute_id,value) VALUES ($1,$2,10)", &[&item, &modifier.modifying]).await.unwrap();
                let old = super::super::attributes::character_attributes_at(&db, &c.sde, f.character, expiry - 1).await.unwrap_or_else(|_| panic!("active attributes"));
                let new = super::super::attributes::character_attributes_at(&db, &c.sde, f.character, expiry).await.unwrap_or_else(|_| panic!("expired attributes"));
                for skill in c.sde.types.iter().filter(|s| s.attribute(ATTR_SKILL_TIME_CONSTANT).is_some() && !existing.iter().any(|r| r.type_id == s.type_id) && super::super::requirements::requirements(&c.sde, s.type_id).next().is_none()) {
                    let dest = super::super::formulas::sp_for_level(training::skill_type(&c.sde, skill.type_id).rank, 1);
                    let mut row = queue::QueueEntry { position: 0, type_id: skill.type_id, to_level: 1, start_sp: 0, dest_sp: dest, start_time: None, end_time: None };
                    let mut slower = row.clone();
                    let Ok(old_end) = training::schedule(&c.sde, &mut row, &old, start) else { continue };
                    let Ok(new_end) = training::schedule(&c.sde, &mut slower, &new, start) else { continue };
                    if old_end <= now || new_end <= old_end { continue; }
                    skill_dao::starting_skill(&db, f.character, skill.type_id, 0, 0).await.unwrap();
                    queue::insert(&db, f.character, &row).await.unwrap();
                    let projected = super::super::timeline::plan_tx(&db, &c.sde, f.character, now).await.unwrap_or_else(|_| panic!("project timeline"));
                    let expected = projected.trained_points().unwrap_or_else(|_| panic!("project points")) - existing.iter().map(|s| s.skill_points).sum::<i64>();
                    let _market = crate::services::market::lock().await;
                    let effects = db.transaction(|tx, outbox: &mut Outbox<crate::services::effects::online::OnlineChanged>| {
                        let sde = c.sde.clone(); let fences = fences.clone();
                        Box::pin(async move { apply_tx(tx, outbox, &sde, &fences, f.character, &[17, 17, 17, 21, 27], now).await })
                    }, |_, outbox| async move { Ok::<_, CallError>({ let entries = outbox.into_entries(); assert!(!entries.iter().any(|effect| matches!(effect, crate::db::outbox::Effect::Operation(_)))); entries }) }).await.unwrap_or_else(|_| panic!("remap"));
                    let saved = skill_dao::current(&db, f.character).await.unwrap();
                    assert_eq!(saved.iter().find(|s| s.type_id == skill.type_id).unwrap().skill_points, expected);
                    let active = queue::active(&db, f.character).await.unwrap().unwrap();
                    assert_eq!((active.start_sp, active.start_time), (expected, Some(now)));
                    assert!(crate::dao::items::brain::get(&db, f.character, item).await.unwrap().is_none());
                    assert_eq!(effects.iter().filter(|e| matches!(e, crate::db::outbox::Effect::Notification(crate::db::outbox::Notification::Pilot { event, .. }) if event == BRAIN_REFRESH)).count(), 1);
                    exercised = true;
                    break;
                }
                if exercised { break 'types; }
                db.execute("DELETE FROM game.items WHERE item_id=$1", &[&item]).await.unwrap();
            }
        }
    }
    assert!(exercised, "no compatible training booster and skill in test SDE");
    }).await;
    crate::scheduler::cancel(&db, &format!("{}:{}", completion::KIND, f.character)).await.unwrap();
    db.execute("DELETE FROM game.skill_history WHERE character_id=$1", &[&f.character]).await.unwrap();
    fixture::cleanup(&db, f).await.unwrap();
    if let Err(panic) = scenario { std::panic::resume_unwind(panic); }
}
