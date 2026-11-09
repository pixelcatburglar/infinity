use super::*;

fn observe_evermarks<'a, 'tx>(
    _: &'a tokio_postgres::Transaction<'tx>, _: &'a mut Outbox, _: &'a crate::sde::Sde,
    event: &'a crate::progress::EventEnvelope,
) -> crate::db::transaction::TxFuture<'a, (), crate::services::CallError> {
    Box::pin(async move {
        assert!(matches!(&event.event, crate::lua::events::GameEvent::EverMarksEarned { .. }));
        Ok(())
    })
}

#[tokio::test]
#[ignore = "requires isolated test process, dedicated INFINITY_TEST_DATABASE_URL with SDE, and external flock"]
async fn real_rewards_replay_and_home_retry() {
    const CHILD: &str = "INFINITY_DAILY_REDEEM_TEST_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "services::daily_goals::redeem::tests::real_rewards_replay_and_home_retry", "--ignored", "--nocapture"])
            .env(CHILD, "1").status().unwrap();
        assert!(status.success());
        return;
    }
    use std::sync::Arc;
    crate::tests::init_config();
    let registration = crate::progress::DurableRegistration { consumer_key: "daily-redeem-test-evermarks", kinds: &["EverMarksEarned"], mode: crate::progress::DeliveryMode::Direct(observe_evermarks) };
    let registry = crate::progress::Registry::new(vec![], vec![registration]).unwrap();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap().with_progress_registry(registry);
    db.execute("INSERT INTO game.progress_subscriptions (consumer_key,generation,activated_at,version,enabled) VALUES ('daily-redeem-test-evermarks',1,1,1,true)", &[]).await.unwrap();
    db.execute("INSERT INTO game.progress_subscription_kinds (consumer_key,kind) VALUES ('daily-redeem-test-evermarks','EverMarksEarned')", &[]).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.expect("daily rewards require real SDE"));
    let world = crate::space::World::new(db.clone(), sde.clone(), 1);
    let hub = crate::chat::Hub::new("test", db.clone(), world.clone());
    let item_type = sde.types.iter().find(|t| t.published == Some(true)).expect("published item type").type_id;
    let station = sde.map.stations().next().expect("NPC station").station_id;
    let nonce = crate::handshake::random_u64();
    let version = format!("redeem-test-{nonce}");
    let root = std::env::temp_dir().join(format!("daily-redeem-{nonce}"));
    std::fs::create_dir_all(root.join("content/daily_goals/data")).unwrap();
    let registration = include_str!("../../../../lua/daily_goals.lua").replace("content_version = \"1\"", &format!("content_version = \"{version}\""));
    let mut content: serde_json::Value = serde_json::from_str(include_str!("../../../../lua/content/daily_goals/data/daily_goals.json")).unwrap();
    content["version"] = version.into();
    for template in content["ordinary"].as_array_mut().unwrap() {
        template["rewards"] = serde_json::json!([
            {"kind":"isk","amount":"1.25"}, {"kind":"evermark","amount":2},
            {"kind":"skill_points","amount":3}, {"kind":"item","type_id":item_type,"amount":4},
            {"kind":"plex","whole_units":5}
        ]);
    }
    std::fs::write(root.join("daily_goals.lua"), registration).unwrap();
    std::fs::write(root.join("content/daily_goals/policy.lua"), include_bytes!("../../../../lua/content/daily_goals/policy.lua")).unwrap();
    std::fs::write(root.join("content/daily_goals/data/daily_goals.json"), serde_json::to_vec(&content).unwrap()).unwrap();
    let mut config = crate::config::Config::default();
    config.lua.enabled = true;
    config.lua.scripts_dir = root.to_string_lossy().into_owned();
    crate::lua::start(&config, &db, &sde, &world, &hub).await.expect("daily Host starts");
    let resources = Resources::from_world(&world).unwrap();
    let fixture = crate::services::tests::fixture::create(&db).await.unwrap();
    db.execute("UPDATE game.characters SET home_station_id=$2, station_id=NULL, balance=0, free_skill_points=0 WHERE character_id=$1", &[&fixture.character, &station]).await.unwrap();
    db.execute("UPDATE game.accounts SET plex_balance=0 WHERE user_id=$1", &[&fixture.account]).await.unwrap();
    let (sender, mut pilot) = tokio::sync::mpsc::unbounded_channel();
    world.register_pilot(fixture.character, sender);
    world.set_online_session(fixture.character, Some(infinity_core::session::OnlineSession {
        character_id: fixture.character, user_id: Some(fixture.account), session_id: 1, ship_id: None, station_id: None,
        system_id: None, corporation_id: None, alliance_id: None, faction_id: None, fleet_id: None, role: 0, corporation_role: 0,
    }));
    let mut notices = crate::gateway::tests::notice_stream(&world, [8; 16], fixture.character, 4);
    let identity = Identity { char_id: fixture.character, ship_id: None, corp_id: 0, station_id: None, system_id: None };
    let now = crate::client::filetime_now();
    let at = crate::dao::daily_goals::periods::bounds(&db, now).await.unwrap().month_end + 1;
    super::super::assignment::ensure_at(&resources, fixture.account, fixture.character, at, false).await.unwrap();
    let views = queries::current(&resources, &identity, fixture.account, at).await.unwrap();
    let goal = views.iter().find(|v| v.goal.category == "ordinary").unwrap();
    let update = instances::ProgressUpdate { progress: goal.goal.target, remainder: crate::lua::events::combat::BinaryAmount { coefficient: "0".into(), exponent: 0 }, completed_at: Some(at), earned_omega: false, paid: false, has_unclaimed: true };
    assert!(instances::update_progress(&db, fixture.character, &goal.goal.goal_id, goal.goal.revision, &update).await.unwrap());
    let id = &goal.goal.goal_id;
    let before = rewards::list(&db, fixture.character, id, false).await.unwrap();
    assert!(matches!(redeem(&resources, &identity, fixture.account, id, true, at).await, Err(Error::Forbidden)));
    assert_eq!(rewards::list(&db, fixture.character, id, false).await.unwrap(), before);
    assert!(notices.rx.try_recv().is_err());
    redeem(&resources, &identity, fixture.account, id, false, at + infinity_core::consts::FILETIME_DAY).await.unwrap();
    let claimed = rewards::list(&db, fixture.character, id, false).await.unwrap();
    assert_eq!(claimed.len(), 5);
    assert!(claimed.iter().all(|r| r.claimed_at.is_some()));
    let item = claimed.iter().find(|r| r.unit == "item").unwrap();
    assert_eq!(item.location_id, Some(station));
    let row = db.query_one("SELECT owner_id,location_id,type_id,quantity FROM game.items WHERE item_id=$1", &[&item.item_id.unwrap()]).await.unwrap();
    assert_eq!(row.get::<_, i64>("owner_id"), fixture.character);
    assert_eq!(row.get::<_, i64>("location_id"), station);
    assert_eq!(row.get::<_, i32>("type_id"), item_type);
    assert_eq!(row.get::<_, i64>("quantity"), 4);
    assert_eq!(standings::lp_balance(&db, fixture.character, CORP_HERALDRY).await.unwrap(), 2);
    assert_eq!(attributes::free_points(&db, fixture.character).await.unwrap(), 3);
    assert_eq!(crate::dao::accounts::plex::balance(&db, fixture.account).await.unwrap(), "5");
    let plex = claimed.iter().find(|r| r.unit == "plex").unwrap();
    let ledger = db.query_one("SELECT kind,delta::text AS delta FROM game.plex_vault_transactions WHERE user_id=$1 AND reference=$2", &[&fixture.account, &plex.plex_reference.unwrap()]).await.unwrap();
    assert_eq!(ledger.get::<_, &str>("kind"), "daily_reward");
    assert_eq!(ledger.get::<_, &str>("delta"), "5");
    let wallet = wallets::balance(&db, fixture.character).await.unwrap();
    assert_eq!(wallet, crate::isk::IskAmount::parse_decimal("1.25").unwrap());
    let fact_count = |kind: &'static str| {
        let db = &db;
        async move { db.query_one("SELECT count(*) AS n FROM game.progress_events WHERE subject_character_id=$1 AND kind=$2", &[&fixture.character, &kind]).await.unwrap().get::<_, i64>("n") }
    };
    assert_eq!(fact_count("EverMarksEarned").await, 1);
    let deliveries: i64 = db.query_one("SELECT count(*) AS n FROM game.progress_deliveries d JOIN game.progress_events e ON e.event_id=d.event_id WHERE e.subject_character_id=$1 AND e.kind='EverMarksEarned' AND d.consumer_key='daily-redeem-test-evermarks'", &[&fixture.character]).await.unwrap().get("n");
    assert_eq!(deliveries, 1);
    assert_eq!(fact_count("LoyaltyPointsEarned").await, 0);
    let mut events = Vec::new();
    while let Ok((name, _)) = pilot.try_recv() { events.push(name); }
    for expected in ["OnAccountChange", "OnLPChange", "OnFreeSkillPointsChanged", "OnItemChange", "OnAurumChangeFromVgs"] { assert!(events.iter().any(|name| name == expected)); }
    assert!(notices.rx.try_recv().is_ok());
    redeem(&resources, &identity, fixture.account, id, true, at + 2 * infinity_core::consts::FILETIME_DAY).await.unwrap();
    assert_eq!(rewards::list(&db, fixture.character, id, false).await.unwrap(), claimed);
    assert!(pilot.try_recv().is_err());
    assert_eq!(fact_count("EverMarksEarned").await, 1);
    assert_eq!(wallets::balance(&db, fixture.character).await.unwrap(), wallet);
    let _ = notices.rx.try_recv();
    let second = views.iter().filter(|v| v.goal.category == "ordinary").nth(1).unwrap();
    assert!(instances::update_progress(&db, fixture.character, &second.goal.goal_id, second.goal.revision, &instances::ProgressUpdate {
        progress: second.goal.target, remainder: crate::lua::events::combat::BinaryAmount { coefficient: "0".into(), exponent: 0 },
        completed_at: Some(at), earned_omega: false, paid: false, has_unclaimed: true,
    }).await.unwrap());
    db.execute("UPDATE game.characters SET free_skill_points=$2 WHERE character_id=$1", &[&fixture.character, &i64::MAX]).await.unwrap();
    let second_before = rewards::list(&db, fixture.character, &second.goal.goal_id, false).await.unwrap();
    assert!(redeem(&resources, &identity, fixture.account, &second.goal.goal_id, false, at).await.is_err());
    assert_eq!(rewards::list(&db, fixture.character, &second.goal.goal_id, false).await.unwrap(), second_before);
    assert_eq!(wallets::balance(&db, fixture.character).await.unwrap(), wallet);
    assert_eq!(standings::lp_balance(&db, fixture.character, CORP_HERALDRY).await.unwrap(), 2);
    assert_eq!(fact_count("EverMarksEarned").await, 1);
    assert!(pilot.try_recv().is_err());
    assert!(notices.rx.try_recv().is_err());
    db.execute("UPDATE game.characters SET free_skill_points=0 WHERE character_id=$1", &[&fixture.character]).await.unwrap();
    let current_station = sde.map.stations().find(|s| s.station_id != station).expect("distinct current NPC station").station_id;
    db.execute("UPDATE game.characters SET station_id=$2 WHERE character_id=$1", &[&fixture.character, &current_station]).await.unwrap();
    world.set_online_session(fixture.character, Some(infinity_core::session::OnlineSession {
        character_id: fixture.character, user_id: Some(fixture.account), session_id: 1, ship_id: None, station_id: Some(current_station),
        system_id: None, corporation_id: None, alliance_id: None, faction_id: None, fleet_id: None, role: 0, corporation_role: 0,
    }));
    let docked = Identity { station_id: Some(current_station), ..identity };
    let third = views.iter().filter(|v| v.goal.category == "ordinary").nth(2).unwrap();
    assert!(instances::update_progress(&db, fixture.character, &third.goal.goal_id, third.goal.revision, &instances::ProgressUpdate {
        progress: third.goal.target, remainder: crate::lua::events::combat::BinaryAmount { coefficient: "0".into(), exponent: 0 },
        completed_at: Some(at), earned_omega: false, paid: false, has_unclaimed: true,
    }).await.unwrap());
    redeem(&resources, &docked, fixture.account, &third.goal.goal_id, true, at).await.unwrap();
    let current_claims = rewards::list(&db, fixture.character, &third.goal.goal_id, false).await.unwrap();
    assert_eq!(current_claims.len(), 5);
    assert!(current_claims.iter().all(|r| r.claimed_at == Some(at)));
    let current_item = current_claims.iter().find(|r| r.unit == "item").unwrap();
    assert_eq!(current_item.location_id, Some(current_station));
    let item_row = db.query_one("SELECT owner_id,location_id,type_id,quantity FROM game.items WHERE item_id=$1", &[&current_item.item_id.unwrap()]).await.unwrap();
    assert_eq!(item_row.get::<_, i64>("owner_id"), fixture.character);
    assert_eq!(item_row.get::<_, i64>("location_id"), current_station);
    assert_eq!(item_row.get::<_, i32>("type_id"), item_type);
    assert_eq!(item_row.get::<_, i64>("quantity"), 4);
    let current_plex = current_claims.iter().find(|r| r.unit == "plex").unwrap();
    let reference = current_plex.plex_reference.unwrap();
    assert_ne!(Some(reference), plex.plex_reference);
    let ledger = db.query_one("SELECT kind,delta::text AS delta FROM game.plex_vault_transactions WHERE user_id=$1 AND reference=$2", &[&fixture.account, &reference]).await.unwrap();
    assert_eq!(ledger.get::<_, &str>("kind"), "daily_reward");
    assert_eq!(ledger.get::<_, &str>("delta"), "5");
    let mut current_events = Vec::new();
    while let Ok((name, _)) = pilot.try_recv() { current_events.push(name); }
    for expected in ["OnAccountChange", "OnLPChange", "OnFreeSkillPointsChanged", "OnItemChange", "OnAurumChangeFromVgs"] { assert!(current_events.iter().any(|name| name == expected)); }
    assert!(notices.rx.try_recv().is_ok());
    let grade = subscriptions::by_user(&db, fixture.account).await.unwrap();
    assert!(subscriptions::set_grade(&db, fixture.account, rules::ALPHA, None, None, grade.revision).await.unwrap());
    let omega_goals: Vec<_> = views.iter().filter(|v| v.goal.kind == "month" && v.snapshot.template.omega_required).collect();
    let locked = omega_goals[0];
    assert!(instances::update_progress(&db, fixture.character, &locked.goal.goal_id, locked.goal.revision, &instances::ProgressUpdate {
        progress: locked.goal.target, remainder: crate::lua::events::combat::BinaryAmount { coefficient: "0".into(), exponent: 0 },
        completed_at: Some(at), earned_omega: false, paid: false, has_unclaimed: true,
    }).await.unwrap());
    assert!(matches!(redeem(&resources, &identity, fixture.account, &locked.goal.goal_id, false, at).await, Err(Error::Forbidden)));
    let grade = subscriptions::by_user(&db, fixture.account).await.unwrap();
    assert!(subscriptions::set_grade(&db, fixture.account, rules::OMEGA, Some(at), Some(at + 1), grade.revision).await.unwrap());
    assert!(matches!(redeem(&resources, &identity, fixture.account, &locked.goal.goal_id, false, at + 1).await, Err(Error::Forbidden)));
    redeem(&resources, &identity, fixture.account, &locked.goal.goal_id, false, at).await.unwrap();
    let earned = omega_goals[1];
    assert!(instances::update_progress(&db, fixture.character, &earned.goal.goal_id, earned.goal.revision, &instances::ProgressUpdate {
        progress: earned.goal.target, remainder: crate::lua::events::combat::BinaryAmount { coefficient: "0".into(), exponent: 0 },
        completed_at: Some(at), earned_omega: true, paid: false, has_unclaimed: true,
    }).await.unwrap());
    let grade = subscriptions::by_user(&db, fixture.account).await.unwrap();
    assert!(subscriptions::set_grade(&db, fixture.account, rules::ALPHA, None, None, grade.revision).await.unwrap());
    redeem(&resources, &identity, fixture.account, &earned.goal.goal_id, false, at + 1).await.unwrap();
    world.unregister_pilot_generation(fixture.character, None);
    db.execute("DELETE FROM game.daily_goal_rewards WHERE goal_id IN (SELECT goal_id FROM game.daily_goal_instances WHERE character_id=$1)", &[&fixture.character]).await.unwrap();
    db.execute("DELETE FROM game.daily_goal_instances WHERE character_id=$1", &[&fixture.character]).await.unwrap();
    db.execute("DELETE FROM game.daily_goal_periods WHERE character_id=$1", &[&fixture.character]).await.unwrap();
    db.execute("DELETE FROM game.plex_vault_transactions WHERE user_id=$1", &[&fixture.account]).await.unwrap();
    db.execute("DELETE FROM game.lp_balances WHERE char_id=$1", &[&fixture.character]).await.unwrap();
    crate::services::tests::fixture::cleanup(&db, fixture).await.unwrap();
    db.execute("DELETE FROM game.progress_subscription_kinds WHERE consumer_key='daily-redeem-test-evermarks'", &[]).await.unwrap();
    db.execute("DELETE FROM game.progress_subscriptions WHERE consumer_key='daily-redeem-test-evermarks'", &[]).await.unwrap();
    std::fs::remove_dir_all(root).unwrap();
}
