use super::*;

#[test]
fn audit_scope_and_payload_are_typed() {
    let payload = serde_json::json!({"increment": "18446744073709551615"});
    let row = AuditRow { audit_id: "00000000-0000-0000-0000-000000000001".into(), actor_id: 1, subject_id: 2, goal_id: "00000000-0000-0000-0000-000000000002".into(), operation: "progress".into(), payload: payload.to_string(), before_state: "{}".into(), after_state: "{}".into(), recorded_at: 1 };
    assert!(matches(&row, 1, 2, &row.goal_id, &payload).unwrap());
    assert!(!matches(&row, 1, 2, &row.goal_id, &serde_json::json!({"increment":"1"})).unwrap());
    assert!(!matches(&row, 2, 2, &row.goal_id, &payload).unwrap());
    assert!(!matches(&row, 1, 3, &row.goal_id, &payload).unwrap());
}

#[tokio::test]
#[ignore = "requires isolated test process, dedicated INFINITY_TEST_DATABASE_URL with SDE, and external flock"]
async fn real_qa_authority_replay_and_cascade() {
    const CHILD: &str = "INFINITY_DAILY_ADMIN_TEST_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let status = std::process::Command::new(std::env::current_exe().unwrap()).args(["--exact", "services::daily_goals::admin::tests::real_qa_authority_replay_and_cascade", "--ignored", "--nocapture"]).env(CHILD, "1").status().unwrap();
        assert!(status.success());
        return;
    }
    use std::sync::Arc;
    use crate::dao::daily_goals::{instances, periods};
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.expect("daily QA requires real SDE"));
    let world = crate::space::World::new(db.clone(), sde.clone(), 1);
    let hub = crate::chat::Hub::new("test", db.clone(), world.clone());
    let nonce = crate::handshake::random_u64();
    let version = format!("qa-test-{nonce}");
    let root = std::env::temp_dir().join(format!("daily-qa-{nonce}"));
    std::fs::create_dir_all(root.join("content/daily_goals/data")).unwrap();
    let registration = include_str!("../../../../lua/daily_goals.lua").replace("content_version = \"1\"", &format!("content_version = \"{version}\""));
    let data = include_str!("../../../../lua/content/daily_goals/data/daily_goals.json").replacen("\"version\": \"1\"", &format!("\"version\": \"{version}\""), 1);
    std::fs::write(root.join("daily_goals.lua"), registration).unwrap();
    std::fs::write(root.join("content/daily_goals/policy.lua"), include_bytes!("../../../../lua/content/daily_goals/policy.lua")).unwrap();
    std::fs::write(root.join("content/daily_goals/data/daily_goals.json"), data).unwrap();
    let mut host = crate::config::Config::default();
    host.lua.enabled = true; host.lua.scripts_dir = root.to_string_lossy().into_owned();
    crate::lua::start(&host, &db, &sde, &world, &hub).await.unwrap();
    let resources = Resources::from_world(&world).unwrap();
    let fixture = crate::services::tests::fixture::create(&db).await.unwrap();
    let identity = Identity { char_id: fixture.character, ship_id: None, corp_id: 0, station_id: None, system_id: None };
    let (sender, _receiver) = tokio::sync::mpsc::unbounded_channel();
    world.register_pilot(fixture.character, sender);
    let mut session = infinity_core::session::OnlineSession { character_id: fixture.character, user_id: Some(fixture.account), session_id: 1, ship_id: None, station_id: None, system_id: None, corporation_id: None, alliance_id: None, faction_id: None, fleet_id: None, role: ROLE_QA, corporation_role: 0 };
    world.set_online_session(fixture.character, Some(session.clone()));
    let mut config = DailyGoalsConfig::default();
    config.qa_account_ids.push(fixture.account);
    assert!(matches!(authorize(&resources, &identity, fixture.account, &[1; UUID_LEN], &config).await, Err(Error::Forbidden)));
    db.execute("UPDATE game.accounts SET role=$2 WHERE user_id=$1", &[&fixture.account, &ROLE_QA]).await.unwrap();
    assert!(matches!(authorize(&resources, &identity, fixture.account, &[1; UUID_LEN], &DailyGoalsConfig::default()).await, Err(Error::Forbidden)));
    assert!(matches!(authorize(&resources, &identity, fixture.account, &[1; UUID_LEN-1], &config).await, Err(Error::Invalid)));
    world.unregister_pilot_generation(fixture.character, None);
    assert!(matches!(authorize(&resources, &identity, fixture.account, &[1; UUID_LEN], &config).await, Err(Error::Forbidden)));
    let (sender, _receiver) = tokio::sync::mpsc::unbounded_channel();
    world.register_pilot(fixture.character, sender);
    world.set_online_session(fixture.character, Some(session.clone()));
    session.user_id = Some(fixture.account + 1); world.set_online_session(fixture.character, Some(session.clone()));
    assert!(matches!(authorize(&resources, &identity, fixture.account, &[1; UUID_LEN], &config).await, Err(Error::Forbidden)));
    session.user_id = Some(fixture.account); world.set_online_session(fixture.character, Some(session));
    let at = periods::bounds(&db, crate::client::filetime_now()).await.unwrap().month_end + 1;
    let assigned = super::super::assignment::ensure_at(&resources, fixture.account, fixture.character, at, false).await.unwrap();
    let goals = instances::period(&db, fixture.character, "day", assigned.bounds.day_start).await.unwrap();
    let ordinary: Vec<_> = goals.iter().filter(|g| g.category == "ordinary").collect();
    let id = &ordinary[0].goal_id;
    let mut notices = crate::gateway::tests::notice_stream(&world, [9; UUID_LEN], fixture.character, 32);
    let (a, b) = tokio::join!(
        progress(&resources, &identity, fixture.account, &[1; UUID_LEN], fixture.character, id, 1, &config, at),
        progress(&resources, &identity, fixture.account, &[1; UUID_LEN], fixture.character, id, 1, &config, at),
    );
    a.unwrap(); b.unwrap();
    let first = instances::get(&db, fixture.character, id).await.unwrap().unwrap();
    assert_eq!(first.progress, 1);
    let first_audit = receipts::audit(&db, &crate::uuid_text::format(&[1; UUID_LEN])).await.unwrap().unwrap();
    assert_eq!((first_audit.actor_id, first_audit.subject_id), (fixture.character, fixture.character));
    assert!(matches(&first_audit, fixture.character, fixture.character, id, &serde_json::json!({"increment":"1"})).unwrap());
    assert!(!matches(&first_audit, fixture.account, fixture.character, id, &serde_json::json!({"increment":"1"})).unwrap());
    progress(&resources, &identity, fixture.account, &[1; UUID_LEN], fixture.character, id, 1, &config, at).await.unwrap();
    assert_eq!(instances::get(&db, fixture.character, id).await.unwrap().unwrap(), first);
    assert!(matches!(progress(&resources, &identity, fixture.account, &[1; UUID_LEN], fixture.character, id, 2, &config, at).await, Err(Error::Invalid)));
    assert!(matches!(progress(&resources, &identity, fixture.account, &[8; UUID_LEN], fixture.character + 1, id, 1, &config, at).await, Err(Error::Forbidden)));
    progress(&resources, &identity, fixture.account, &[4; UUID_LEN], fixture.character, id, 0, &config, at).await.unwrap();
    assert_eq!(instances::get(&db, fixture.character, id).await.unwrap().unwrap(), first);
    progress(&resources, &identity, fixture.account, &[6; UUID_LEN], fixture.character, id, 1, &config, at).await.unwrap();
    let second = instances::get(&db, fixture.character, id).await.unwrap().unwrap();
    assert_eq!(second.progress, 2);
    let planned = planner::prepare(&resources, fixture.account, fixture.character, id, 1, at).await.unwrap();
    let change = Arc::new(planned.change);
    let transaction_config = Arc::new(config.clone());
    let account = fixture.account;
    let character = fixture.character;
    db.execute("UPDATE game.accounts SET role=0 WHERE user_id=$1", &[&fixture.account]).await.unwrap();
    let revoked = db.transaction::<_, _, TxError, Outbox, _, _, _>(|tx, _| {
        let change = Arc::clone(&change);
        let config = Arc::clone(&transaction_config);
        Box::pin(async move {
            change.prelock(tx).await?;
            authority(tx, account, character, &config).await?;
            Ok(())
        })
    }, |(), _| async move { Ok(()) }).await;
    assert!(matches!(revoked, Err(TxError::Domain(Error::Forbidden))));
    assert!(matches!(progress(&resources, &identity, fixture.account, &[7; UUID_LEN], fixture.character, id, 1, &config, at).await, Err(Error::Forbidden)));
    assert!(receipts::audit(&db, &crate::uuid_text::format(&[7; UUID_LEN])).await.unwrap().is_none());
    assert_eq!(instances::get(&db, fixture.character, id).await.unwrap().unwrap(), second);
    db.execute("UPDATE game.accounts SET role=$2 WHERE user_id=$1", &[&fixture.account, &ROLE_QA]).await.unwrap();
    let rollback_id = crate::uuid_text::format(&[5; UUID_LEN]);
    let rollback = AuditRow { audit_id: rollback_id.clone(), actor_id: fixture.character, subject_id: fixture.character, goal_id: id.clone(), operation: "progress".into(), payload: "{\"increment\":\"1\"}".into(), before_state: planned.before, after_state: planned.after, recorded_at: at };
    let rollback = Arc::new(rollback);
    let result = db.transaction::<_, _, TxError, Outbox, _, _, _>(|tx, outbox| {
        let change = Arc::clone(&change);
        let rollback = Arc::clone(&rollback);
        Box::pin(async move {
            change.prelock(tx).await?;
            assert!(receipts::insert_audit(tx, &rollback).await?);
            change.apply(tx, outbox).await?;
            Err::<(), _>(TxError::Domain(Error::Invalid))
        })
    }, |(), _| async move { Ok(()) }).await;
    assert!(matches!(result, Err(TxError::Domain(Error::Invalid))));
    assert!(receipts::audit(&db, &rollback_id).await.unwrap().is_none());
    assert_eq!(instances::get(&db, fixture.character, id).await.unwrap().unwrap(), second);
    while notices.rx.try_recv().is_ok() {}
    progress(&resources, &identity, fixture.account, &[2; UUID_LEN], fixture.character, id, u64::MAX, &config, at).await.unwrap();
    use prost_reflect::ReflectMessage;
    let (_, progressed) = crate::gateway::tests::decode_notice(&notices.rx.try_recv().expect("progressed notice"));
    let (_, completed_notice) = crate::gateway::tests::decode_notice(&notices.rx.try_recv().expect("completed notice"));
    assert_eq!(progressed.descriptor().full_name(), "eve_public.dailygoal.api.ProgressedNotice");
    assert_eq!(completed_notice.descriptor().full_name(), "eve_public.dailygoal.api.CompletedNotice");
    while notices.rx.try_recv().is_ok() {}
    progress(&resources, &identity, fixture.account, &[2; UUID_LEN], fixture.character, id, u64::MAX, &config, at).await.unwrap();
    assert!(notices.rx.try_recv().is_err());
    progress(&resources, &identity, fixture.account, &[3; UUID_LEN], fixture.character, &ordinary[1].goal_id, u64::MAX, &config, at).await.unwrap();
    let completed = instances::get(&db, fixture.character, id).await.unwrap().unwrap();
    assert_eq!(completed.progress, completed.target);
    let bonus = instances::period(&db, fixture.character, "day", assigned.bounds.day_start).await.unwrap().into_iter().find(|g| g.category == "bonus").unwrap();
    assert!(bonus.completed_at.is_some());
    assert!(instances::period(&db, fixture.character, "month", assigned.bounds.month_start).await.unwrap().iter().any(|g| g.progress > 0));
    assert!(notices.rx.try_recv().is_ok());
    let audit = receipts::audit(&db, &crate::uuid_text::format(&[1; UUID_LEN])).await.unwrap().unwrap();
    assert_ne!(audit.before_state, audit.after_state);
    db.execute("DELETE FROM game.daily_goal_admin_audit WHERE subject_id=$1", &[&fixture.character]).await.unwrap();
    db.execute("DELETE FROM game.daily_goal_rewards WHERE goal_id IN (SELECT goal_id FROM game.daily_goal_instances WHERE character_id=$1)", &[&fixture.character]).await.unwrap();
    db.execute("DELETE FROM game.daily_goal_instances WHERE character_id=$1", &[&fixture.character]).await.unwrap();
    db.execute("DELETE FROM game.daily_goal_periods WHERE character_id=$1", &[&fixture.character]).await.unwrap();
    world.unregister_pilot_generation(fixture.character, None);
    crate::services::tests::fixture::cleanup(&db, fixture).await.unwrap();
    std::fs::remove_dir_all(root).unwrap();
}
