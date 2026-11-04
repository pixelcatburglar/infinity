//! Real free-point settlement, queue retiming and failure coverage.
use super::*;
use crate::services::tests::{client, fixture};
use crate::db::outbox::{Effect, Notification};
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn free_points_debit_retimes_queue_and_rolls_back_after_database_failure() {
    crate::tests::init_config();
    let _market = crate::services::market::lock().await;
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    let (c, _peer) = client(db.clone(), f.character).await;
    let sde = c.sde.clone();
    let type_id = sde.types.iter().find(|t| t.category_id == Some(crate::consts::CATEGORY_SKILL) && t.published == Some(true) && super::super::requirements::requirements(&sde, t.type_id).next().is_none()).expect("skill without prerequisites").type_id;
    let points = super::super::formulas::sp_for_level(training::skill_type(&sde, type_id).rank, 1);
    let now = filetime_now();
    db.execute("UPDATE game.characters SET free_skill_points=$2 WHERE character_id=$1", &[&f.character, &(points * 3)]).await.unwrap();
    db.execute("INSERT INTO game.skills (character_id,type_id,level,skill_points) VALUES ($1,$2,0,0)", &[&f.character, &type_id]).await.unwrap();
    db.execute("INSERT INTO game.skill_queue (character_id,position,type_id,to_level,start_sp,dest_sp,start_time,end_time) VALUES ($1,0,$2,1,0,$3,NULL,NULL),($1,1,$2,2,$3,$4,NULL,NULL)", &[&f.character, &type_id, &points, &super::super::formulas::sp_for_level(training::skill_type(&sde, type_id).rank, 2)]).await.unwrap();
    let before = fixture::skill_fingerprint(&db, f.character).await.unwrap();
    let dispatched = Arc::new(AtomicBool::new(false));
    let flag = dispatched.clone();
    let failed: Result<(), CallError> = db.transaction(|tx, outbox: &mut crate::db::outbox::Outbox<crate::services::effects::online::OnlineChanged>| {
        let sde = sde.clone(); Box::pin(async move {
            apply_tx(tx, outbox, &sde, f.character, &[(type_id, MAX_SKILL_LEVEL)], Some(&[(type_id, points)]), now).await?;
            tx.query_one("SELECT 1 / 0", &[]).await.map_err(|error| db_err(crate::db::Error::from(error)))?;
            Ok(())
        })
    }, |(), _| async move { flag.store(true, Ordering::SeqCst); Ok(()) }).await;
    fixture::assert_division_by_zero(failed);
    assert!(!dispatched.load(Ordering::SeqCst));
    assert_eq!(attributes::free_points(&db, f.character).await.unwrap(), points * 3);
    assert_eq!(fixture::skill_fingerprint(&db, f.character).await.unwrap(), before);
    assert!(history::list(&db, f.character, 50).await.unwrap().is_empty());
    let effects = db.transaction(|tx, outbox: &mut crate::db::outbox::Outbox<crate::services::effects::online::OnlineChanged>| {
        let sde = sde.clone(); Box::pin(async move { apply_tx(tx, outbox, &sde, f.character, &[(type_id, MAX_SKILL_LEVEL)], Some(&[(type_id, points)]), now).await.map(|_| ()) })
    }, |(), outbox| async move { Ok::<_, CallError>({ let entries = outbox.into_entries(); assert!(!entries.iter().any(|effect| matches!(effect, crate::db::outbox::Effect::Operation(_)))); entries }) }).await.unwrap_or_else(|_| panic!("free point settlement failed"));
    assert_eq!(attributes::free_points(&db, f.character).await.unwrap(), points * 2);
    let skill = skill_dao::current(&db, f.character).await.unwrap().pop().unwrap();
    assert_eq!((skill.level, skill.skill_points), (1, points));
    let rows = queue::list(&db, f.character).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].to_level, 2);
    assert_eq!(history::list(&db, f.character, 50).await.unwrap().len(), 1);
    assert!(effects.iter().any(|e| matches!(e, Effect::Notification(Notification::DirectClient { event, .. }) if event == "OnFreeSkillPointsChanged")));
    assert!(!effects.iter().any(|e| matches!(e, Effect::Event(crate::lua::events::GameEvent::SkillTrained { .. }))));
    for amount in [0, -1, points * 3, i64::MAX] {
        let refused: Result<(), CallError> = db.transaction(|tx, outbox: &mut crate::db::outbox::Outbox<crate::services::effects::online::OnlineChanged>| {
            let sde = sde.clone(); Box::pin(async move { apply_tx(tx, outbox, &sde, f.character, &[(type_id, MAX_SKILL_LEVEL)], Some(&[(type_id, amount)]), now).await.map(|_| ()) })
        }, |(), _| async move { Ok(()) }).await;
        assert!(refused.is_err());
    }
    assert_eq!(attributes::free_points(&db, f.character).await.unwrap(), points * 2);
    assert_eq!(history::list(&db, f.character, 50).await.unwrap().len(), 1);
    db.execute("DELETE FROM game.skill_history WHERE character_id=$1", &[&f.character]).await.unwrap();
    fixture::cleanup(&db, f).await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn overdue_completion_is_merged_with_free_points() {
    use crate::lua::events::GameEvent;
    crate::tests::init_config();
    let _market = crate::services::market::lock().await;
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    let (c, _peer) = client(db.clone(), f.character).await;
    let sde = c.sde.clone();
    let types: Vec<_> = sde.types.iter().filter(|t| t.category_id == Some(crate::consts::CATEGORY_SKILL) && t.published == Some(true) && super::super::requirements::requirements(&sde, t.type_id).next().is_none()).take(2).map(|t| t.type_id).collect();
    assert_eq!(types.len(), 2);
    let (trained, target) = (types[0], types[1]);
    let trained_sp = super::super::formulas::sp_for_level(training::skill_type(&sde, trained).rank, 1);
    let points = super::super::formulas::sp_for_level(training::skill_type(&sde, target).rank, 1);
    let now = filetime_now();
    db.execute("UPDATE game.characters SET free_skill_points=$2 WHERE character_id=$1", &[&f.character, &(points * 2)]).await.unwrap();
    db.execute("INSERT INTO game.skills (character_id,type_id,level,skill_points) VALUES ($1,$2,0,0),($1,$3,0,0)", &[&f.character, &trained, &target]).await.unwrap();
    db.execute("INSERT INTO game.skill_queue (character_id,position,type_id,to_level,start_sp,dest_sp,start_time,end_time) VALUES ($1,0,$2,1,0,$3,1,2)", &[&f.character, &trained, &trained_sp]).await.unwrap();
    let booster_type = sde.types.iter().find(|t| t.attribute(crate::consts::dogma::ATTR_BOOSTERNESS).is_some_and(|slot| slot > 0.0)).expect("booster in SDE").type_id;
    let booster: i64 = db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,singleton,brain_activated_at,brain_expires_at) VALUES ($1,$2,$2,$3,true,1,3) RETURNING item_id", &[&booster_type, &f.character, &crate::consts::FLAG_BOOSTER]).await.unwrap().get("item_id");
    db.execute("INSERT INTO game.item_attributes (item_id,attribute_id,value) VALUES ($1,$2,0)", &[&booster, &crate::consts::dogma::ATTR_SKILL_TIME_CONSTANT]).await.unwrap();
    super::super::completion::reschedule(&db, f.character).await.unwrap_or_else(|_| panic!("scheduling failed"));
    let before = fixture::skill_fingerprint(&db, f.character).await.unwrap();
    let job = format!("{}:{}", super::super::completion::KIND, f.character);
    let jobs_before = db.query_one("SELECT row_to_json(j)::text AS state FROM game.jobs j WHERE job_id=$1", &[&job]).await.unwrap().get::<_, String>("state");
    let dispatched = Arc::new(AtomicBool::new(false));
    let flag = dispatched.clone();
    let failed: Result<(), CallError> = db.transaction(|tx, outbox: &mut Outbox<crate::services::effects::online::OnlineChanged>| {
        let sde = sde.clone(); Box::pin(async move { apply_tx(tx, outbox, &sde, f.character, &[(target, MAX_SKILL_LEVEL)], Some(&[(target, 0)]), now).await.map(|_| ()) })
    }, |(), _| async move { flag.store(true, Ordering::SeqCst); Ok(()) }).await;
    assert!(failed.is_err());
    assert!(!dispatched.load(Ordering::SeqCst));
    assert_eq!(fixture::skill_fingerprint(&db, f.character).await.unwrap(), before);
    assert_eq!(db.query_one("SELECT row_to_json(j)::text AS state FROM game.jobs j WHERE job_id=$1", &[&job]).await.unwrap().get::<_, String>("state"), jobs_before);
    assert_eq!(attributes::free_points(&db, f.character).await.unwrap(), points * 2);
    assert!(db.query_opt("SELECT item_id FROM game.items WHERE item_id=$1 AND brain_activated_at=1 AND brain_expires_at=3", &[&booster]).await.unwrap().is_some());
    assert!(db.query_opt("SELECT item_id FROM game.item_attributes WHERE item_id=$1 AND attribute_id=$2 AND value=0", &[&booster, &crate::consts::dogma::ATTR_SKILL_TIME_CONSTANT]).await.unwrap().is_some());
    assert!(history::list(&db, f.character, 50).await.unwrap().is_empty());
    let effects = db.transaction(|tx, outbox: &mut Outbox<crate::services::effects::online::OnlineChanged>| {
        let sde = sde.clone(); Box::pin(async move { apply_tx(tx, outbox, &sde, f.character, &[(target, MAX_SKILL_LEVEL)], Some(&[(target, points)]), now).await.map(|_| ()) })
    }, |(), outbox| async move { Ok::<_, CallError>({ let entries = outbox.into_entries(); assert!(!entries.iter().any(|effect| matches!(effect, crate::db::outbox::Effect::Operation(_)))); entries }) }).await.unwrap_or_else(|_| panic!("free points settlement failed"));
    let batches: Vec<_> = effects.iter().filter_map(|e| match e { Effect::Notification(Notification::Pilot { char_id, event, args }) if *char_id == f.character && event == "OnServerSkillsChanged" => Some(args), _ => None }).collect();
    assert_eq!(batches.len(), 1);
    let Value::Dict(infos) = &batches[0][0] else { panic!("skills dictionary") };
    assert_eq!(infos.len(), 2);
    for (id, sp) in [(trained, trained_sp), (target, points)] {
        assert!(infos.iter().any(|(key, value)| *key == Value::Int(i64::from(id)) && *value == super::super::view::incremental_skill_info(&sde, id, Some(1), Some(sp), None, now)));
    }
    assert_eq!(effects.iter().filter(|e| matches!(e, Effect::Notification(Notification::Pilot { char_id, event, .. }) if *char_id == f.character && event == crate::client::BRAIN_REFRESH)).count(), 1);
    assert_eq!(effects.iter().filter(|e| matches!(e, Effect::Notification(Notification::Pilot { char_id, event, .. }) if *char_id == f.character && event == "OnServerBoostersChanged")).count(), 1);
    assert_eq!(effects.iter().filter(|e| match e {
        Effect::Notification(Notification::Pilot { char_id, event, .. }) => *char_id == f.character && event == "OnNewSkillQueueSaved",
        Effect::Notification(Notification::DirectClient { event, .. }) => event == "OnNewSkillQueueSaved",
        _ => false,
    }).count(), 1);
    assert!(db.query_opt("SELECT item_id FROM game.items WHERE item_id=$1", &[&booster]).await.unwrap().is_none());
    assert!(db.query_opt("SELECT item_id FROM game.item_attributes WHERE item_id=$1", &[&booster]).await.unwrap().is_none());
    let events: Vec<_> = effects.iter().filter_map(|e| match e { Effect::Event(GameEvent::SkillTrained { char_id, type_id, level }) => Some((*char_id, *type_id, *level)), _ => None }).collect();
    assert_eq!(events, vec![(f.character, trained, 1)]);
    assert_eq!(attributes::free_points(&db, f.character).await.unwrap(), points);
    assert!(queue::list(&db, f.character).await.unwrap().is_empty());
    let hist = history::list(&db, f.character, 50).await.unwrap();
    assert_eq!(hist.len(), 2);
    assert!(hist.iter().any(|h| h.event_type_id == super::super::history::TRAINING_COMPLETE && h.absolute_points == trained_sp));
    assert!(hist.iter().any(|h| h.event_type_id == super::super::history::FREE_POINTS_USED && h.absolute_points == points));
    crate::scheduler::cancel(&db, &format!("{}:{}", super::super::completion::KIND, f.character)).await.unwrap();
    db.execute("DELETE FROM game.skill_history WHERE character_id=$1", &[&f.character]).await.unwrap();
    fixture::cleanup(&db, f).await.unwrap();
}
#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn free_points_queue_order_and_partial_final_entry_match_preview_and_apply() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    let (mut c, _peer) = client(db.clone(), f.character).await;
    let sde = c.sde.clone();
    let types: Vec<_> = sde.types.iter()
        .filter(|t| t.category_id == Some(crate::consts::CATEGORY_SKILL) && t.published == Some(true) && super::super::requirements::requirements(&sde, t.type_id).next().is_none())
        .take(2).map(|t| t.type_id).collect();
    assert_eq!(types.len(), 2);
    let first = super::super::formulas::sp_for_level(training::skill_type(&sde, types[0]).rank, 1);
    let second = super::super::formulas::sp_for_level(training::skill_type(&sde, types[1]).rank, 1);
    let free = first + second / 2;
    db.execute("UPDATE game.characters SET free_skill_points=$2 WHERE character_id=$1", &[&f.character, &free]).await.unwrap();
    db.execute("INSERT INTO game.skills (character_id,type_id,level,skill_points) VALUES ($1,$2,0,0),($1,$3,0,0)", &[&f.character, &types[0], &types[1]]).await.unwrap();
    db.execute("INSERT INTO game.skill_queue (character_id,position,type_id,to_level,start_sp,dest_sp,start_time,end_time) VALUES ($1,0,$2,1,0,$3,NULL,NULL),($1,1,$4,1,0,$5,NULL,NULL)", &[&f.character, &types[0], &first, &types[1], &second]).await.unwrap();
    let targets = vec![(types[0], 1), (types[1], 1)];
    let preview = preview(&mut c, f.character, targets.clone()).await.unwrap();
    let Value::Dict(preview) = preview else { panic!("free-point preview dictionary") };
    assert_eq!(preview.len(), 2);
    assert_eq!(preview.iter().find(|(id, _)| *id == Value::Int(i64::from(types[0]))).map(|(_, v)| v), Some(&Value::Int(first)));
    assert_eq!(preview.iter().find(|(id, _)| *id == Value::Int(i64::from(types[1]))).map(|(_, v)| v), Some(&Value::Int(second / 2)));
    let result = apply_targets(&mut c, f.character, targets, None).await.unwrap();
    assert_eq!(result, Value::Int(0));
    assert_eq!(attributes::free_points(&db, f.character).await.unwrap(), 0);
    let rows = queue::list(&db, f.character).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].type_id, types[1]);
    fixture::cleanup(&db, f).await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn free_points_repeated_type_at_two_target_levels_is_accumulated_once() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    let (mut c, _peer) = client(db.clone(), f.character).await;
    let sde = c.sde.clone();
    let type_id = sde.types.iter().find(|t| t.category_id == Some(crate::consts::CATEGORY_SKILL) && t.published == Some(true) && super::super::requirements::requirements(&sde, t.type_id).next().is_none()).unwrap().type_id;
    let rank = training::skill_type(&sde, type_id).rank;
    let needed = super::super::formulas::sp_for_level(rank, 2);
    db.execute("UPDATE game.characters SET free_skill_points=$2 WHERE character_id=$1", &[&f.character, &needed]).await.unwrap();
    db.execute("INSERT INTO game.skills (character_id,type_id,level,skill_points) VALUES ($1,$2,0,0)", &[&f.character, &type_id]).await.unwrap();
    let targets = vec![(type_id, 1), (type_id, 2)];
    let Value::Dict(preview) = preview(&mut c, f.character, targets.clone()).await.unwrap() else { panic!("free-point preview dictionary") };
    assert_eq!(preview, vec![(Value::Int(i64::from(type_id)), Value::Int(needed))]);
    assert_eq!(apply_targets(&mut c, f.character, targets, None).await.unwrap(), Value::Int(0));
    let skill = skill_dao::current(&db, f.character).await.unwrap().pop().unwrap();
    assert_eq!(skill.level, 2);
    assert_eq!(skill.skill_points, needed);
    fixture::cleanup(&db, f).await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn free_points_preview_includes_points_from_in_training_entry() {
    crate::tests::init_config();
    let _market = crate::services::market::lock().await;
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    let (mut c, _peer) = client(db.clone(), f.character).await;
    let sde = c.sde.clone();
    let type_id = sde.types.iter().find(|t| t.category_id == Some(crate::consts::CATEGORY_SKILL) && t.published == Some(true) && super::super::requirements::requirements(&sde, t.type_id).next().is_none()).unwrap().type_id;
    let rank = training::skill_type(&sde, type_id).rank;
    let target = super::super::formulas::sp_for_level(rank, 1);
    let now = filetime_now();
    db.execute("UPDATE game.characters SET free_skill_points=$2 WHERE character_id=$1", &[&f.character, &target]).await.unwrap();
    db.execute("INSERT INTO game.skills (character_id,type_id,level,skill_points) VALUES ($1,$2,0,0)", &[&f.character, &type_id]).await.unwrap();
    db.execute("INSERT INTO game.skill_queue (character_id,position,type_id,to_level,start_sp,dest_sp,start_time,end_time) VALUES ($1,0,$2,1,0,$3,$4,$5)", &[&f.character, &type_id, &target, &(now - crate::client::filetime_ticks_from_seconds(1)), &(now + crate::client::filetime_ticks_from_seconds(60))]).await.unwrap();
    let Value::Dict(preview) = preview(&mut c, f.character, vec![(type_id, 1)]).await.unwrap() else { panic!("free-point preview dictionary") };
    assert_eq!(preview.len(), 1);
    let points = preview[0].1.as_int().expect("preview points");
    assert_eq!(preview[0].0, Value::Int(i64::from(type_id)));
    assert!(points > 0 && points <= target);
    fixture::cleanup(&db, f).await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn free_points_apply_result_and_distribution_equal_preview() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    let (mut c, _peer) = client(db.clone(), f.character).await;
    let sde = c.sde.clone();
    let type_id = sde.types.iter().find(|t| t.category_id == Some(crate::consts::CATEGORY_SKILL) && t.published == Some(true) && super::super::requirements::requirements(&sde, t.type_id).next().is_none()).unwrap().type_id;
    let rank = training::skill_type(&sde, type_id).rank;
    let points = super::super::formulas::sp_for_level(rank, 1);
    db.execute("UPDATE game.characters SET free_skill_points=$2 WHERE character_id=$1", &[&f.character, &points]).await.unwrap();
    db.execute("INSERT INTO game.skills (character_id,type_id,level,skill_points) VALUES ($1,$2,0,0)", &[&f.character, &type_id]).await.unwrap();
    let targets = vec![(type_id, 1)];
    let Value::Dict(preview) = preview(&mut c, f.character, targets.clone()).await.unwrap() else { panic!("free-point preview dictionary") };
    assert_eq!(preview, vec![(Value::Int(i64::from(type_id)), Value::Int(points))]);
    assert_eq!(apply_targets(&mut c, f.character, targets, None).await.unwrap(), Value::Int(0));
    assert_eq!(attributes::free_points(&db, f.character).await.unwrap(), 0);
    fixture::cleanup(&db, f).await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn free_points_apply_refuses_selected_skill_that_is_not_injected() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    let (mut c, _peer) = client(db.clone(), f.character).await;
    let type_id = c.sde.types.iter().find(|t| t.category_id == Some(crate::consts::CATEGORY_SKILL) && t.published == Some(true)).unwrap().type_id;
    let name = c.sde.types.get(type_id).and_then(|t| t.name.clone()).unwrap();
    db.execute("UPDATE game.characters SET free_skill_points=1 WHERE character_id=$1", &[&f.character]).await.unwrap();
    let error = apply_targets(&mut c, f.character, vec![(type_id, 1)], None).await.unwrap_err();
    assert!(matches!(error, CallError::User { ref msg, ref dict } if msg == crate::messages::MSG_CANNOT_APPLY_FREE_POINTS_DO_NOT_HAVE_SKILL && dict.iter().any(|(key, value)| key == "skillName" && *value == Value::unicode(name.clone()))));
    fixture::cleanup(&db, f).await.unwrap();
}


#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn free_points_zero_balance_returns_empty_preview_dictionary() {
    crate::tests::init_config();
    let _market = crate::services::market::lock().await;
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    let (mut c, _peer) = client(db.clone(), f.character).await;
    let sde = c.sde.clone();
    let type_id = sde.types.iter().find(|t| t.category_id == Some(crate::consts::CATEGORY_SKILL) && t.published == Some(true) && super::super::requirements::requirements(&sde, t.type_id).next().is_none()).unwrap().type_id;
    db.execute("INSERT INTO game.skills (character_id,type_id,level,skill_points) VALUES ($1,$2,0,0)", &[&f.character, &type_id]).await.unwrap();
    let Value::Dict(preview) = preview(&mut c, f.character, vec![(type_id, 1)]).await.unwrap() else { panic!("free-point preview dictionary") };
    assert!(preview.is_empty());
    fixture::cleanup(&db, f).await.unwrap();
}
