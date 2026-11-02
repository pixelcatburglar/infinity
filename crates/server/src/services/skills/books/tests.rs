//! Real skillbook settlement and rollback coverage.
use super::*;
use crate::dao::skills::{history, queue};
use crate::services::tests::{client, fixture};
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn batch_refusals_and_database_failure_roll_back_consumption() {
    crate::tests::init_config();
    let _market = crate::services::market::lock().await;
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    let (c, _peer) = client(db.clone(), f.character).await;
    let sde = c.sde.clone();
    let types: Vec<_> = sde.types.iter().filter(|t| t.category_id == Some(CATEGORY_SKILL) && t.published == Some(true) && requirements(&sde, t.type_id).next().is_none()).take(2).map(|t| t.type_id).collect();
    assert_eq!(types.len(), 2);
    let station = 60000001_i64;
    db.execute("UPDATE game.characters SET station_id=$2 WHERE character_id=$1", &[&f.character, &station]).await.unwrap();
    let mut ids = Vec::new();
    for &type_id in &types {
        ids.push(crate::dao::items::transfers::insert(&db, type_id, f.character, station, crate::consts::FLAG_HANGAR, 1).await.unwrap());
    }
    let remote = crate::dao::items::transfers::insert(&db, types[1], f.character, station + 1, crate::consts::FLAG_HANGAR, 1).await.unwrap();
    let refused: Result<(), CallError> = db.transaction(|tx, outbox: &mut Outbox<crate::services::effects::online::OnlineChanged>| {
        let sde = sde.clone(); let first = ids[0];
        Box::pin(async move { inject_tx(tx, outbox, &sde, f.character, None, &[first, remote], filetime_now(), None).await.map(|_| ()) })
    }, |(), _| async move { Ok(()) }).await;
    assert!(refused.is_err());
    assert!(skill_dao::current(&db, f.character).await.unwrap().is_empty());
    db.execute("DELETE FROM game.items WHERE item_id=$1", &[&remote]).await.unwrap();
    let dependent = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SKILL) && t.published == Some(true) && requirements(&sde, t.type_id).any(|(_, level)| level > 0)).expect("SDE skill with prerequisites").type_id;
    let dependent_item = crate::dao::items::transfers::insert(&db, dependent, f.character, station, crate::consts::FLAG_HANGAR, 1).await.unwrap();
    let refused: Result<(), CallError> = db.transaction(|tx, outbox: &mut Outbox<crate::services::effects::online::OnlineChanged>| {
        let sde = sde.clone(); let first = ids[0];
        Box::pin(async move { inject_tx(tx, outbox, &sde, f.character, None, &[first, dependent_item], filetime_now(), None).await.map(|_| ()) })
    }, |(), _| async move { Ok(()) }).await;
    assert!(refused.is_err());
    assert!(skill_dao::current(&db, f.character).await.unwrap().is_empty());
    db.execute("DELETE FROM game.items WHERE item_id=$1", &[&dependent_item]).await.unwrap();
    let now = filetime_now();
    let before = fixture::skill_fingerprint(&db, f.character).await.unwrap();
    let dispatched = Arc::new(AtomicBool::new(false));
    let flag = dispatched.clone();
    let result: Result<(), CallError> = db.transaction(|tx, outbox: &mut Outbox<crate::services::effects::online::OnlineChanged>| {
        let sde = sde.clone(); let ids = ids.clone();
        Box::pin(async move {
            inject_tx(tx, outbox, &sde, f.character, None, &ids, now, None).await?;
            tx.query_one("SELECT 1 / 0", &[]).await.map_err(|error| db_err(crate::db::Error::from(error)))?;
            Ok(())
        })
    }, |(), _| async move { flag.store(true, Ordering::SeqCst); Ok(()) }).await;
    fixture::assert_division_by_zero(result);
    assert!(!dispatched.load(Ordering::SeqCst));
    assert_eq!(fixture::skill_fingerprint(&db, f.character).await.unwrap(), before);
    assert_eq!(db.query_one("SELECT count(*) AS n FROM game.items WHERE owner_id=$1", &[&f.character]).await.unwrap().get::<_, i64>("n"), 2);
    assert!(history::list(&db, f.character, 50).await.unwrap().is_empty());
    let effects = db.transaction(|tx, outbox: &mut Outbox<crate::services::effects::online::OnlineChanged>| {
        let sde = sde.clone(); let ids = ids.clone();
        Box::pin(async move { inject_tx(tx, outbox, &sde, f.character, None, &ids, now, None).await.map(|_| ()) })
    }, |(), outbox| async move { Ok::<_, CallError>({ let entries = outbox.into_entries(); assert!(!entries.iter().any(|effect| matches!(effect, crate::db::outbox::Effect::Operation(_)))); entries }) }).await.unwrap_or_else(|_| panic!("book settlement failed"));
    assert!(!effects.is_empty());
    assert!(!effects.iter().any(|e| matches!(e, crate::db::outbox::Effect::Notification(crate::db::outbox::Notification::Pilot { event, .. } | crate::db::outbox::Notification::DirectClient { event, .. }) if event == "OnNewSkillQueueSaved")));
    assert_eq!(skill_dao::current(&db, f.character).await.unwrap().len(), 2);
    assert_eq!(history::list(&db, f.character, 50).await.unwrap().len(), 2);
    let duplicate = crate::dao::items::transfers::insert(&db, types[0], f.character, station, crate::consts::FLAG_HANGAR, 1).await.unwrap();
    let duplicate_result: Result<(), CallError> = db.transaction(|tx, outbox: &mut Outbox<crate::services::effects::online::OnlineChanged>| {
        let sde = sde.clone();
        Box::pin(async move { inject_tx(tx, outbox, &sde, f.character, None, &[duplicate], now, None).await.map(|_| ()) })
    }, |(), _| async move { Ok(()) }).await;
    assert!(duplicate_result.is_err());
    assert!(db.query_opt("SELECT item_id FROM game.items WHERE item_id=$1", &[&duplicate]).await.unwrap().is_some());
    let retry: Result<(), CallError> = db.transaction(|tx, outbox: &mut Outbox<crate::services::effects::online::OnlineChanged>| {
        let sde = sde.clone(); let ids = ids.clone();
        Box::pin(async move { inject_tx(tx, outbox, &sde, f.character, None, &ids, now, None).await.map(|_| ()) })
    }, |(), _| async move { Ok(()) }).await;
    assert!(retry.is_err());
    assert_eq!(history::list(&db, f.character, 50).await.unwrap().len(), 2);
    db.execute("DELETE FROM game.skill_history WHERE character_id=$1", &[&f.character]).await.unwrap();
    fixture::cleanup(&db, f).await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn start_training_injects_and_schedules_one_level() {
    crate::tests::init_config();
    let _market = crate::services::market::lock().await;
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    let (c, _peer) = client(db.clone(), f.character).await;
    let sde = c.sde.clone();
    let type_id = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SKILL) && t.published == Some(true) && requirements(&sde, t.type_id).next().is_none()).unwrap().type_id;
    let station = 60000001_i64;
    db.execute("UPDATE game.characters SET station_id=$2 WHERE character_id=$1", &[&f.character, &station]).await.unwrap();
    let item = crate::dao::items::transfers::insert(&db, type_id, f.character, station, crate::consts::FLAG_HANGAR, 1).await.unwrap();
    let now = filetime_now();
    let wrong: Result<(), CallError> = db.transaction(|tx, outbox: &mut Outbox<crate::services::effects::online::OnlineChanged>| {
        let sde = sde.clone(); Box::pin(async move { inject_tx(tx, outbox, &sde, f.character, None, &[item], now, Some(station + 1)).await.map(|_| ()) })
    }, |(), _| async move { Ok(()) }).await;
    assert!(wrong.is_err());
    let result: Result<(), CallError> = db.transaction(|tx, outbox: &mut Outbox<crate::services::effects::online::OnlineChanged>| {
        let sde = sde.clone(); Box::pin(async move { inject_tx(tx, outbox, &sde, f.character, None, &[item], now, Some(station)).await.map(|_| ()) })
    }, |(), outbox| async move { assert!(!outbox.into_entries().iter().any(|effect| matches!(effect, crate::db::outbox::Effect::Operation(_)))); Ok(()) }).await;
    assert!(result.is_ok());
    let queued = queue::list(&db, f.character).await.unwrap();
    assert_eq!(queued.len(), 1);
    assert_eq!((queued[0].type_id, queued[0].to_level), (type_id, 1));
    assert!(queued[0].end_time.is_some());
    assert_eq!(history::list(&db, f.character, 50).await.unwrap().len(), 1);
    let job = format!("{}:{}", super::super::completion::KIND, f.character);
    crate::scheduler::cancel(&db, &job).await.unwrap();
    db.execute("DELETE FROM game.skill_history WHERE character_id=$1", &[&f.character]).await.unwrap();
    fixture::cleanup(&db, f).await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn overdue_completion_is_merged_with_book_injection_and_start() {
    use crate::db::outbox::{Effect, Notification};
    use crate::lua::events::GameEvent;
    crate::tests::init_config();
    let _market = crate::services::market::lock().await;
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    for start in [false, true] {
        let f = fixture::create(&db).await.unwrap();
        let (c, _peer) = client(db.clone(), f.character).await;
        let sde = c.sde.clone();
        let types: Vec<_> = sde.types.iter().filter(|t| t.category_id == Some(CATEGORY_SKILL) && t.published == Some(true) && requirements(&sde, t.type_id).next().is_none()).take(2).map(|t| t.type_id).collect();
        assert_eq!(types.len(), 2);
        let (trained, book) = (types[0], types[1]);
        let station = 60000001_i64;
        let now = filetime_now();
        let points = super::super::formulas::sp_for_level(training::skill_type(&sde, trained).rank, 1);
        db.execute("UPDATE game.characters SET station_id=$2 WHERE character_id=$1", &[&f.character, &station]).await.unwrap();
        db.execute("INSERT INTO game.skills (character_id,type_id,level,skill_points) VALUES ($1,$2,0,0)", &[&f.character, &trained]).await.unwrap();
        db.execute("INSERT INTO game.skill_queue (character_id,position,type_id,to_level,start_sp,dest_sp,start_time,end_time) VALUES ($1,0,$2,1,0,$3,1,2)", &[&f.character, &trained, &points]).await.unwrap();
        let booster_type = sde.types.iter().find(|t| t.attribute(crate::consts::dogma::ATTR_BOOSTERNESS).is_some_and(|slot| slot > 0.0)).expect("booster in SDE").type_id;
        let booster: i64 = db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,singleton,brain_activated_at,brain_expires_at) VALUES ($1,$2,$2,$3,true,1,3) RETURNING item_id", &[&booster_type, &f.character, &crate::consts::FLAG_BOOSTER]).await.unwrap().get("item_id");
        db.execute("INSERT INTO game.item_attributes (item_id,attribute_id,value) VALUES ($1,$2,0)", &[&booster, &crate::consts::dogma::ATTR_SKILL_TIME_CONSTANT]).await.unwrap();
        let item = crate::dao::items::transfers::insert(&db, book, f.character, station, crate::consts::FLAG_HANGAR, 1).await.unwrap();
        super::super::completion::reschedule(&db, f.character).await.unwrap_or_else(|_| panic!("scheduling failed"));
        let before = fixture::skill_fingerprint(&db, f.character).await.unwrap();
        let job = format!("{}:{}", super::super::completion::KIND, f.character);
        let jobs_before = db.query_one("SELECT row_to_json(j)::text AS state FROM game.jobs j WHERE job_id=$1", &[&job]).await.unwrap().get::<_, String>("state");
        let dispatched = Arc::new(AtomicBool::new(false));
        let flag = dispatched.clone();
        let failed: Result<(), CallError> = db.transaction(|tx, outbox: &mut Outbox<crate::services::effects::online::OnlineChanged>| {
            let sde = sde.clone(); Box::pin(async move { inject_tx(tx, outbox, &sde, f.character, None, &[item], now, Some(station + 1)).await.map(|_| ()) })
        }, |(), _| async move { flag.store(true, Ordering::SeqCst); Ok(()) }).await;
        assert!(failed.is_err());
        assert!(!dispatched.load(Ordering::SeqCst));
        assert_eq!(fixture::skill_fingerprint(&db, f.character).await.unwrap(), before);
        assert_eq!(db.query_one("SELECT row_to_json(j)::text AS state FROM game.jobs j WHERE job_id=$1", &[&job]).await.unwrap().get::<_, String>("state"), jobs_before);
        assert!(db.query_opt("SELECT item_id FROM game.items WHERE item_id=$1 AND brain_activated_at=1 AND brain_expires_at=3", &[&booster]).await.unwrap().is_some());
        assert!(db.query_opt("SELECT item_id FROM game.item_attributes WHERE item_id=$1 AND attribute_id=$2 AND value=0", &[&booster, &crate::consts::dogma::ATTR_SKILL_TIME_CONSTANT]).await.unwrap().is_some());
        assert!(db.query_opt("SELECT item_id FROM game.items WHERE item_id=$1", &[&item]).await.unwrap().is_some());
        assert!(history::list(&db, f.character, 50).await.unwrap().is_empty());
        let effects = db.transaction(|tx, outbox: &mut Outbox<crate::services::effects::online::OnlineChanged>| {
            let sde = sde.clone(); Box::pin(async move { inject_tx(tx, outbox, &sde, f.character, None, &[item], now, start.then_some(station)).await.map(|_| ()) })
        }, |(), outbox| async move { Ok::<_, CallError>({ let entries = outbox.into_entries(); assert!(!entries.iter().any(|effect| matches!(effect, crate::db::outbox::Effect::Operation(_)))); entries }) }).await.unwrap_or_else(|_| panic!("book settlement failed"));
        let batches: Vec<_> = effects.iter().filter_map(|e| match e { Effect::Notification(Notification::Pilot { char_id, event, args }) if *char_id == f.character && event == "OnServerSkillsChanged" => Some(args), _ => None }).collect();
        assert_eq!(batches.len(), 1);
        let Value::Dict(infos) = &batches[0][0] else { panic!("skills dictionary") };
        assert_eq!(infos.len(), 2);
        for (id, level, sp) in [(trained, 1, points), (book, 0, 0)] {
            assert!(infos.iter().any(|(key, value)| *key == Value::Int(i64::from(id)) && *value == training::skill_info(&sde, id, level, sp, now)));
        }
        assert_eq!(effects.iter().filter(|e| matches!(e, Effect::Notification(Notification::Pilot { char_id, event, .. }) if *char_id == f.character && event == crate::client::BRAIN_REFRESH)).count(), 1);
        assert_eq!(effects.iter().filter(|e| matches!(e, Effect::Notification(Notification::Pilot { char_id, event, .. }) if *char_id == f.character && event == "OnServerBoostersChanged")).count(), 1);
        assert!(db.query_opt("SELECT item_id FROM game.items WHERE item_id=$1", &[&booster]).await.unwrap().is_none());
        assert!(db.query_opt("SELECT item_id FROM game.item_attributes WHERE item_id=$1", &[&booster]).await.unwrap().is_none());
        let events: Vec<_> = effects.iter().filter_map(|e| match e { Effect::Event(GameEvent::SkillTrained { char_id, type_id, level }) => Some((*char_id, *type_id, *level)), _ => None }).collect();
        assert_eq!(events, vec![(f.character, trained, 1)]);
        assert!(db.query_opt("SELECT item_id FROM game.items WHERE item_id=$1", &[&item]).await.unwrap().is_none());
        let rows = queue::list(&db, f.character).await.unwrap();
        assert_eq!(rows.len(), usize::from(start));
        if start { assert_eq!((rows[0].type_id, rows[0].to_level), (book, 1)); }
        let notices: Vec<_> = effects.iter().filter_map(|e| match e {
            Effect::Notification(Notification::Pilot { char_id, event, args }) if *char_id == f.character && event == "OnNewSkillQueueSaved" => Some(args),
            Effect::Notification(Notification::DirectClient { event, args }) if event == "OnNewSkillQueueSaved" => Some(args),
            _ => None,
        }).collect();
        assert_eq!(notices.len(), 1);
        assert_eq!(*notices[0], vec![super::super::skill_handler::queue_value(&rows)]);
        let hist = history::list(&db, f.character, 50).await.unwrap();
        assert_eq!(hist.len(), 2);
        assert!(hist.iter().any(|h| h.event_type_id == super::super::history::TRAINING_COMPLETE && h.absolute_points == points));
        assert!(hist.iter().any(|h| h.event_type_id == super::super::history::INJECTED && h.absolute_points == 0));
        crate::scheduler::cancel(&db, &format!("{}:{}", super::super::completion::KIND, f.character)).await.unwrap();
        db.execute("DELETE FROM game.skill_history WHERE character_id=$1", &[&f.character]).await.unwrap();
        fixture::cleanup(&db, f).await.unwrap();
    }
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn non_skill_book_returns_native_error_and_context() {
    crate::tests::init_config();
    let _market = crate::services::market::lock().await;
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    let (c, _peer) = client(db.clone(), f.character).await;
    let sde = c.sde.clone();
    let type_id = sde.types.iter().find(|t| t.category_id != Some(CATEGORY_SKILL) && t.published == Some(true)).unwrap().type_id;
    let station = 60000001_i64;
    db.execute("UPDATE game.characters SET station_id=$2 WHERE character_id=$1", &[&f.character, &station]).await.unwrap();
    let item = crate::dao::items::transfers::insert(&db, type_id, f.character, station, crate::consts::FLAG_HANGAR, 1).await.unwrap();
    let result: Result<(), CallError> = db.transaction(|tx, outbox: &mut Outbox<crate::services::effects::online::OnlineChanged>| {
        let sde = sde.clone(); Box::pin(async move { inject_tx(tx, outbox, &sde, f.character, None, &[item], filetime_now(), None).await.map(|_| ()) })
    }, |(), _| async move { Ok(()) }).await;
    match result {
        Err(CallError::User { msg, dict }) => {
            assert_eq!(msg, messages::MSG_ITEM_NOT_A_SKILL);
            assert!(dict == vec![("skillName".into(), Value::Int(i64::from(type_id)))]);
        }
        _ => panic!("expected ItemNotASkill"),
    }
    assert!(db.query_opt("SELECT item_id FROM game.items WHERE item_id=$1", &[&item]).await.unwrap().is_some());
    fixture::cleanup(&db, f).await.unwrap();
}
