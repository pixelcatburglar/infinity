use super::*;
use marshal::Value;

#[test]
fn skill_levels_default_and_validate_boundaries() {
    assert!(matches!(level(None), Ok(5)));
    assert!(matches!(level(Some("0")), Ok(0)));
    assert!(matches!(level(Some("5")), Ok(5)));
    assert!(level(Some("-1")).is_err());
    assert!(level(Some("6")).is_err());
    assert!(level(Some("two")).is_err());
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn setattr_targets_other_character_and_requires_gm_role() {
    use crate::services::{Call, tests::{client, fixture}};
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let (gm, target) = (fixture::create(&db).await.unwrap(), fixture::create(&db).await.unwrap());
    let scenario = crate::tests::catch_scenario(async {
    let (mut c, _peer) = client(db.clone(), gm.character).await;
    crate::dao::characters::attributes::initialize_respec(&db, gm.character, 1).await.unwrap();
    crate::dao::characters::attributes::save_bases(&db, gm.character, &[9; 5], &crate::dao::characters::attributes::respec(&db, gm.character).await.unwrap().unwrap()).await.unwrap();
    let (notify, mut notices) = tokio::sync::mpsc::unbounded_channel();
    c.world.register_pilot(target.character, notify);
    let kwargs = Value::Dict(vec![]);
    let line = format!("/setattr 0 {}", target.character);
    for role in [0, infinity_core::consts::GM_ROLES] {
        c.login.role = role;
        match crate::services::slash::tests::support::rpc(&mut c, &line).await {
            Err(CallError::User { msg, .. }) => assert_eq!((role, msg.as_str()), (0, infinity_core::messages::MSG_SLASH_ERROR)),
            Ok(Value::Unicode(text)) => {
                assert_eq!(role, infinity_core::consts::GM_ROLES);
                assert_eq!(text, infinity_core::messages::gm::gm_setattr_done(target.character, 0));
            }
            _ => panic!("unexpected setattr result"),
        }
    }
    let events: Vec<String> = std::iter::from_fn(|| notices.try_recv().ok()).map(|(name, _)| name).collect();
    assert!(events.iter().any(|name| name == "OnRespecInfoChanged"), "target must receive the respec notification: {events:?}");
    let respec = crate::dao::characters::attributes::respec(&db, target.character).await.unwrap().unwrap();
    assert_eq!(respec.bases.as_deref(), Some(&[0; 5][..]));
    assert_eq!(crate::dao::characters::attributes::respec(&db, gm.character).await.unwrap().unwrap().bases.as_deref(), Some(&[9; 5][..]), "setattr must not touch the caller's own bases");
    let empty: &[Value] = &[];
    let read = Call { service: crate::services::skills::SKILL_HANDLER, method: "GetAttributes", args: empty, kwargs: &kwargs, bound: false, bind_params: None };
    let mut reader = c;
    reader.session.apply(vec![("charid", Value::Int(target.character))]);
    let Some(Ok(Value::Dict(attrs))) = crate::services::skills::dispatch(&mut reader, &read).await else { panic!("GetAttributes failed") };
    assert!(attrs.iter().all(|(_, v)| matches!(v, Value::Float(x) if *x == 0.0)), "effective attributes must be Float zeros: {attrs:?}");
    reader.world.unregister_pilot_generation(target.character, None);
    }).await;
    fixture::cleanup(&db, target).await.unwrap();
    fixture::cleanup(&db, gm).await.unwrap();
    if let Err(panic) = scenario { std::panic::resume_unwind(panic); }
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn backintime_resets_only_the_named_target() {
    use crate::services::{tests::{client, fixture}, skills::{formulas::sp_for_level, training::skill_type}};
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let (gm, target) = (fixture::create(&db).await.unwrap(), fixture::create(&db).await.unwrap());
    let scenario = crate::tests::catch_scenario(async {
    let (mut c, _peer) = client(db.clone(), gm.character).await;
    fixture::seed_skill(&db, gm.character).await.unwrap();
    let sentinel = fixture::skill_fingerprint(&db, gm.character).await.unwrap();
    let race = crate::dao::skills::training::race(&db, target.character).await.unwrap().unwrap();
    let mut starters = std::collections::BTreeMap::new();
    for s in &c.sde.characters.race(i64::from(race)).unwrap().skills { starters.entry(s.type_id).or_insert(s.level); }
    assert!(!starters.is_empty(), "the fixture race must have a starter template");
    let extra = c.sde.types.iter().find(|t| t.category_id == Some(infinity_core::consts::CATEGORY_SKILL) && !starters.contains_key(&t.type_id)).unwrap().type_id;
    let raised = *starters.keys().next().unwrap();
    assert_eq!(crate::dao::characters::attributes::grant_free_points(&db, target.character, 250_000).await.unwrap(), Some(250_000));
    crate::dao::skills::training::starting_skill(&db, target.character, extra, 5, 999).await.unwrap();
    crate::dao::skills::training::set_level(&db, target.character, raised, 5, 0).await.unwrap();
    db.execute("INSERT INTO game.skill_queue (character_id, position, type_id, to_level, start_sp, dest_sp, start_time, end_time) VALUES ($1, 0, $2, 1, 0, 100, 0, 1)", &[&target.character, &extra]).await.unwrap();
    let (notify, mut notices) = tokio::sync::mpsc::unbounded_channel();
    c.world.register_pilot(target.character, notify);
    c.login.role = 0;
    let line = format!("/backintime {}", target.character);
    let target_fp = fixture::skill_fingerprint(&db, target.character).await.unwrap();
    assert!(matches!(crate::services::slash::tests::support::rpc(&mut c, &line).await, Err(CallError::User { msg, .. }) if msg == infinity_core::messages::MSG_SLASH_ERROR));
    assert_eq!(fixture::skill_fingerprint(&db, target.character).await.unwrap(), target_fp, "a refused reset must not write trained or queued rows");
    assert_eq!(crate::dao::characters::attributes::free_points(&db, target.character).await.unwrap(), 250_000);
    c.login.role = infinity_core::consts::GM_ROLES;
    let two = format!("/backintime {0} {0}", target.character);
    assert!(matches!(crate::services::slash::tests::support::rpc(&mut c, &two).await, Err(CallError::User { msg, .. }) if msg == infinity_core::messages::MSG_SLASH_ERROR));
    match crate::services::slash::tests::support::rpc(&mut c, &line).await {
        Ok(Value::Unicode(text)) => assert_eq!(text, infinity_core::messages::gm::gm_skill_done("backintime", target.character)),
        other => panic!("unexpected backintime result: {other:?}"),
    }
    let expected: std::collections::BTreeMap<i32, (i32, i64)> = starters.iter().map(|(&t, &l)| (t, (l, sp_for_level(skill_type(&c.sde, t).rank, i64::from(l))))).collect();
    let trained: std::collections::BTreeMap<i32, (i32, i64)> = crate::dao::skills::training::current(&db, target.character).await.unwrap().into_iter().map(|s| (s.type_id, (s.level, s.skill_points))).collect();
    assert_eq!(trained, expected, "the trained set must equal the race starter template");
    assert!(crate::dao::skills::queue::list(&db, target.character).await.unwrap().is_empty());
    assert_eq!(crate::dao::characters::attributes::free_points(&db, target.character).await.unwrap(), 0);
    assert_eq!(fixture::skill_fingerprint(&db, gm.character).await.unwrap(), sentinel, "the caller's own skills must be untouched");
    let events: Vec<String> = std::iter::from_fn(|| notices.try_recv().ok()).map(|(name, _)| name).collect();
    for name in ["OnServerSkillsChanged", "OnServerSkillsRemoved", "OnNewSkillQueueSaved", "OnFreeSkillPointsChanged", crate::client::BRAIN_REFRESH] {
        assert!(events.iter().any(|event| event == name), "the target must receive {name}: {events:?}");
    }
    let name: String = db.query_one("SELECT name FROM game.characters WHERE character_id = $1", &[&target.character]).await.unwrap().get("name");
    for (line, who) in [(format!("/backintime \"{name}\""), target.character), ("/backintime".to_string(), gm.character), ("/backintime me".to_string(), gm.character)] {
        match crate::services::slash::tests::support::rpc(&mut c, &line).await {
            Ok(Value::Unicode(text)) => assert_eq!(text, infinity_core::messages::gm::gm_skill_done("backintime", who), "line: {line}"),
            other => panic!("unexpected {line} result: {other:?}"),
        }
    }
    }).await;
    fixture::cleanup(&db, target).await.unwrap();
    fixture::cleanup(&db, gm).await.unwrap();
    if let Err(panic) = scenario { std::panic::resume_unwind(panic); }
}
