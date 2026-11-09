//! Book injection and training with unmet SDE prerequisites.
use super::*;
use crate::services::{Call, skills};

async fn snapshot(db: &crate::db::Db, character: i64) -> String {
    db.query_one("SELECT (SELECT COALESCE(string_agg(row_to_json(i)::text, '' ORDER BY item_id), '') FROM game.items i WHERE owner_id=$1) || '|' || (SELECT COALESCE(string_agg(row_to_json(s)::text, '' ORDER BY type_id), '') FROM game.skills s WHERE character_id=$1) || '|' || (SELECT COALESCE(string_agg(row_to_json(q)::text, '' ORDER BY position), '') FROM game.skill_queue q WHERE character_id=$1) || '|' || (SELECT COALESCE(string_agg(row_to_json(h)::text, '' ORDER BY history_id), '') FROM game.skill_history h WHERE character_id=$1) AS state", &[&character]).await.unwrap().get("state")
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn prerequisite_book_injection_and_queue_agreement() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (mut c, _peer) = client(test_db.clone(), f.character).await;
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        c.notify_tx = sender.clone();
        let effective = super::super::super::effective::snapshot_at(&test_db, &c.sde, f.character, crate::client::filetime_now()).await.unwrap();
        let skill = c.sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SKILL) && t.published == Some(true) && effective.get(t.type_id).and_then(|entry| entry.trained.as_ref()).is_none() && requirements(&c.sde, t.type_id).any(|(id, level)| effective.level(id) < level)).expect("published untrained skill with unmet prerequisites in SDE").type_id;
        assert!(requirements(&c.sde, skill).any(|(id, level)| effective.level(id) < level));
        let station = 60000001_i64;
        test_db.execute("UPDATE game.characters SET station_id=$2 WHERE character_id=$1", &[&f.character, &station]).await.unwrap();
        let item = crate::dao::items::transfers::insert(&test_db, skill, f.character, station, infinity_core::consts::FLAG_HANGAR, 1).await.unwrap();
        let kwargs = Value::None;
        let args = [Value::list([Value::Int(item)])];
        let call = Call { service: "dogmaIM", method: "InjectSkillIntoBrain", args: &args, kwargs: &kwargs, bound: true, bind_params: None };
        assert!(crate::services::dogma::dispatch(&mut c, &call).await.is_ok(), "plain injection must not require trained prerequisites");
        assert!(test_db.query_opt("SELECT item_id FROM game.items WHERE item_id=$1", &[&item]).await.unwrap().is_none());
        let rows = skill_dao::current(&test_db, f.character).await.unwrap();
        assert!(rows.iter().any(|row| row.type_id == skill && row.level == 0 && row.skill_points == 0));
        let mut found = false;
        while let Ok((event, args)) = receiver.try_recv() {
            if event == "OnServerSkillsChanged" {
                let Value::Dict(infos) = &args[0] else { panic!("skill snapshot dictionary") };
                let now = args[2].as_int().unwrap();
                assert!(infos.iter().any(|(id, value)| *id == Value::Int(i64::from(skill)) && *value == super::super::super::view::incremental_skill_info(&c.sde, skill, Some(0), Some(0), None, now)));
                found = true;
            }
        }
        assert!(found, "missing injected skill snapshot");
        c.outbox.clear();
        let before = snapshot(&test_db, f.character).await;
        let queue = Value::Dict(vec![(Value::Int(0), Value::tuple([Value::Int(i64::from(skill)), Value::Int(1)]))]);
        let args = [queue];
        let call = Call { service: "skillHandler", method: "SaveNewQueue", args: &args, kwargs: &kwargs, bound: true, bind_params: None };
        match skills::dispatch(&mut c, &call).await.unwrap() {
            Err(CallError::User { msg, .. }) => assert_eq!(msg, "QueueCannotPlaceSkillBeforeRequirements"),
            _ => panic!("expected prerequisite queue refusal"),
        }
        assert_eq!(snapshot(&test_db, f.character).await, before);
        assert!(c.outbox.is_empty());
        assert!(receiver.try_recv().is_err());
    }).await;
    db.execute("DELETE FROM game.skill_history WHERE character_id=$1", &[&f.character]).await.unwrap();
    crate::scheduler::cancel(&db, &format!("{}:{}", super::super::super::completion::KIND, f.character)).await.unwrap();
    fixture::cleanup(&db, f).await.unwrap();
    assert!(result.is_ok(), "prerequisite injection task failed: {result:?}");
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn prerequisite_start_training_rolls_back() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (mut c, _peer) = client(test_db.clone(), f.character).await;
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        c.notify_tx = sender.clone();
        let effective = super::super::super::effective::snapshot_at(&test_db, &c.sde, f.character, crate::client::filetime_now()).await.unwrap();
        let skill = c.sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SKILL) && t.published == Some(true) && effective.get(t.type_id).and_then(|entry| entry.trained.as_ref()).is_none() && requirements(&c.sde, t.type_id).any(|(id, level)| effective.level(id) < level)).expect("published untrained skill with unmet prerequisites in SDE").type_id;
        assert!(requirements(&c.sde, skill).any(|(id, level)| effective.level(id) < level));
        let station = 60000001_i64;
        test_db.execute("UPDATE game.characters SET station_id=$2 WHERE character_id=$1", &[&f.character, &station]).await.unwrap();
        let item = crate::dao::items::transfers::insert(&test_db, skill, f.character, station, infinity_core::consts::FLAG_HANGAR, 1).await.unwrap();
        let before = snapshot(&test_db, f.character).await;
        let kwargs = Value::None;
        let args = [Value::Int(item), Value::Int(station)];
        let call = Call { service: "skillHandler", method: "CharStartTrainingSkill", args: &args, kwargs: &kwargs, bound: true, bind_params: None };
        match skills::dispatch(&mut c, &call).await.unwrap() {
            Err(CallError::User { msg, dict }) => {
                assert_eq!(msg, "QueueCannotPlaceSkillBeforeRequirements");
                assert!(dict.is_empty());
            }
            _ => panic!("expected prerequisite queue refusal"),
        }
        assert_eq!(snapshot(&test_db, f.character).await, before);
        assert!(c.outbox.is_empty());
        assert!(receiver.try_recv().is_err());
    }).await;
    db.execute("DELETE FROM game.skill_history WHERE character_id=$1", &[&f.character]).await.unwrap();
    crate::scheduler::cancel(&db, &format!("{}:{}", super::super::super::completion::KIND, f.character)).await.unwrap();
    fixture::cleanup(&db, f).await.unwrap();
    assert!(result.is_ok(), "prerequisite training task failed: {result:?}");
}
