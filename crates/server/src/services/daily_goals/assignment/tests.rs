use super::*;
use crate::lua::content::daily_goals::model::{Career, Method, Reward};

#[test]
fn frozen_rewards_preserve_all_units_and_stable_asset_identity() {
    let template = Template { category: Category::Ordinary, omega_required: true, key: "test".into(), target: 1, career: Career::None, method: Method::SpaceJump, rewards: vec![Reward::Isk { amount: crate::isk::IskAmount::parse_decimal("1.25").unwrap() }, Reward::Evermark { amount: 2 }, Reward::SkillPoints { amount: 3 }, Reward::Item { type_id: 4, amount: 5 }, Reward::Plex { whole_units: 6 }] };
    let slot = "ordinary:0";
    let goal = GoalRow { goal_id: ids::goal(1, 2, "v", slot, 0), character_id: 1, kind: "day".into(), period_start: 2, period_end: 3, slot_key: slot.into(), generation: 0, namespace: "daily_goals".into(), version: "v".into(), category: "ordinary".into(), definition: String::new(), target: 1, progress: 0, remainder: BinaryAmount { coefficient: "0".into(), exponent: 0 }, completed_at: None, earned_omega: false, paid: false, voided_at: None, has_unclaimed: false, revision: 0 };
    let rows = frozen::rewards(&goal, &template).unwrap();
    assert_eq!(rows, frozen::rewards(&goal, &template).unwrap());
    assert_eq!(rows.iter().map(|r| r.unit.as_str()).collect::<Vec<_>>(), ["isk", "lp", "sp", "item", "plex"]);
    assert_eq!(rows[0].amount_isk, Some(crate::isk::IskAmount::parse_decimal("1.25").unwrap()));
    assert_eq!(rows[1].issuer_id, Some(infinity_core::consts::CORP_HERALDRY));
    assert_eq!(rows[2].amount_units, Some(3));
    assert_eq!((rows[3].type_id, rows[3].amount_units), (Some(4), Some(5)));
    assert_eq!(rows[4].amount_units, Some(6));
    assert!(rows.iter().all(|r| r.omega_required));
    assert_eq!(rows.iter().map(|r| &r.asset_id).collect::<std::collections::HashSet<_>>().len(), rows.len());
    let mut malformed = goal.clone();
    malformed.definition = "{".into();
    assert!(frozen::validate(&malformed).is_err());
    let snapshot = super::super::GoalSnapshot { template, messages: super::super::MessageRefs { name: 1, description: 2, help: 3 }, assigner: 1 };
    malformed.definition = serde_json::to_string(&snapshot).unwrap();
    let decoded = frozen::validate(&malformed).unwrap();
    assert_eq!((decoded.messages.name, decoded.messages.description, decoded.messages.help), (1, 2, 3));
}

#[tokio::test]
#[ignore = "requires isolated test process, dedicated INFINITY_TEST_DATABASE_URL with SDE, and external flock"]
async fn real_host_assigns_thirteen_once_under_concurrent_ensure() {
    const CHILD: &str = "INFINITY_DAILY_ASSIGNMENT_TEST_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "services::daily_goals::assignment::tests::real_host_assigns_thirteen_once_under_concurrent_ensure", "--ignored", "--nocapture"])
            .env(CHILD, "1").status().unwrap();
        assert!(status.success());
        return;
    }
    use std::sync::Arc;
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.expect("daily assignments require real SDE"));
    let world = crate::space::World::new(db.clone(), sde.clone(), 1);
    let hub = crate::chat::Hub::new("test", db.clone(), world.clone());
    let id = crate::handshake::random_u64();
    let version = format!("assignment-test-{id}");
    let root = std::env::temp_dir().join(format!("daily-assignment-{id}"));
    std::fs::create_dir_all(root.join("content/daily_goals/data")).unwrap();
    let registration = include_str!("../../../../lua/daily_goals.lua").replace("content_version = \"1\"", &format!("content_version = \"{version}\""));
    let data = include_str!("../../../../lua/content/daily_goals/data/daily_goals.json").replacen("\"version\": \"1\"", &format!("\"version\": \"{version}\""), 1);
    std::fs::write(root.join("daily_goals.lua"), registration).unwrap();
    std::fs::write(root.join("content/daily_goals/policy.lua"), include_bytes!("../../../../lua/content/daily_goals/policy.lua")).unwrap();
    std::fs::write(root.join("content/daily_goals/data/daily_goals.json"), data).unwrap();
    let mut config = crate::config::Config::default();
    config.lua.enabled = true;
    config.lua.scripts_dir = root.to_string_lossy().into_owned();
    crate::lua::start(&config, &db, &sde, &world, &hub).await.expect("daily Host must start with real authored content");
    let resources = Resources::from_world(&world).unwrap();
    let fixture = crate::services::tests::fixture::create(&db).await.unwrap();
    let (pilot, _pilot_rx) = tokio::sync::mpsc::unbounded_channel();
    world.register_pilot(fixture.character, pilot);
    world.set_online_session(fixture.character, Some(infinity_core::session::OnlineSession {
        character_id: fixture.character, user_id: Some(fixture.account), session_id: 1, ship_id: None, station_id: None,
        system_id: None, corporation_id: None, alliance_id: None, faction_id: None, fleet_id: None, role: 0, corporation_role: 0,
    }));
    let mut notices = crate::gateway::tests::notice_stream(&world, [7; 16], fixture.character, 4);
    let activations = crate::dao::lua_content::activations::list(&db, super::super::DAILY_GOALS_NAMESPACE).await.unwrap();
    let mut earliest = i64::MAX;
    for (index, row) in activations.iter().enumerate() {
        let bounds = periods::bounds(&db, row.published_at).await.unwrap();
        let (day, month) = if index == 0 {
            (bounds.day_start, bounds.month_start)
        } else {
            (bounds.day_end, bounds.month_end)
        };
        earliest = earliest.min(day).min(month);
    }
    assert!(!activations.is_empty());
    let before_activation = ensure_at(&resources, fixture.account, fixture.character, earliest.checked_sub(1).unwrap(), false).await.unwrap();
    assert!(!before_activation.created);
    assert!(before_activation.day.is_none() && before_activation.month.is_none());
    let now = crate::client::filetime_now();
    let at = periods::bounds(&db, now).await.unwrap().month_end + 1;
    let (a, b) = tokio::join!(ensure_at(&resources, fixture.account, fixture.character, at, true), ensure_at(&resources, fixture.account, fixture.character, at, true));
    let (a, b) = (a.unwrap(), b.unwrap());
    assert_ne!(a.created, b.created);
    let frame = notices.rx.try_recv().expect("created assignment must dispatch a notice after commit");
    let (_, payload) = crate::gateway::tests::decode_notice(&frame);
    use prost_reflect::ReflectMessage;
    assert_eq!(payload.descriptor().full_name(), "eve_public.dailygoal.api.CurrentGoalsNotice");
    assert!(periods::get(&db, fixture.character, "day", a.bounds.day_start).await.unwrap().is_some());
    assert!(notices.rx.try_recv().is_err());
    let identity = crate::services::identity::Identity { char_id: fixture.character, ship_id: None, corp_id: 0, station_id: None, system_id: None };
    let first = queries::current(&resources, &identity, fixture.account, at).await.unwrap();
    let second = queries::current(&resources, &identity, fixture.account, at).await.unwrap();
    let notified = if a.created { a.current.as_ref().unwrap() } else { b.current.as_ref().unwrap() };
    assert_eq!(notified.iter().map(|v| &v.goal.goal_id).collect::<Vec<_>>(), first.iter().map(|v| &v.goal.goal_id).collect::<Vec<_>>());
    assert!(if a.created { b.current.is_none() } else { a.current.is_none() });
    assert_eq!(first.len(), 13);
    assert_eq!(first.iter().map(|v| (&v.goal.goal_id, &v.goal.definition, &v.rewards)).collect::<Vec<_>>(), second.iter().map(|v| (&v.goal.goal_id, &v.goal.definition, &v.rewards)).collect::<Vec<_>>());
    let mut duplicate = first[0].goal.clone();
    duplicate.definition = "{}".into();
    assert!(!instances::insert(&db, &duplicate).await.unwrap());
    assert_eq!(instances::get(&db, fixture.character, &duplicate.goal_id).await.unwrap().unwrap().definition, first[0].goal.definition);
    let mut duplicate_period = a.day.as_ref().unwrap().clone();
    duplicate_period.version = "different".into();
    assert!(!periods::insert(&db, &duplicate_period).await.unwrap());
    assert_eq!(periods::get(&db, fixture.character, "day", duplicate_period.period_start).await.unwrap().unwrap(), *a.day.as_ref().unwrap());
    let asset = first.iter().flat_map(|view| &view.rewards).next().expect("assigned goals must have rewards");
    assert!(!rewards::insert(&db, fixture.character, asset).await.unwrap());
    assert!(rewards::list(&db, fixture.character, &asset.goal_id, false).await.unwrap().contains(asset));
    let fractional = BinaryAmount { coefficient: "1".into(), exponent: -1 };
    let update = instances::ProgressUpdate { progress: 0, remainder: fractional.clone(), completed_at: None, earned_omega: false, paid: false, has_unclaimed: false };
    assert!(instances::update_progress(&db, fixture.character, &first[0].goal.goal_id, 0, &update).await.unwrap());
    let mut changed_sde = crate::sde::Sde::load(&db).await.unwrap();
    changed_sde.contribution_methods = Default::default();
    let mut changed_resources = resources.clone();
    changed_resources.sde = Arc::new(changed_sde);
    assert!(frozen::snapshot(&changed_resources.sde, &first[0].snapshot.template).is_err());
    let reopened = queries::get(&changed_resources, &identity, fixture.account, &first[0].goal.goal_id, at).await.unwrap();
    assert_eq!((reopened.snapshot.messages.name, reopened.snapshot.messages.description, reopened.snapshot.messages.help), (first[0].snapshot.messages.name, first[0].snapshot.messages.description, first[0].snapshot.messages.help));
    let fetched = queries::get(&resources, &identity, fixture.account, &first[0].goal.goal_id, at).await.unwrap();
    assert_eq!(fetched.goal.goal_id, first[0].goal.goal_id);
    assert!(matches!(queries::get(&resources, &identity, fixture.account + 1, &first[0].goal.goal_id, at).await, Err(Error::Forbidden)));
    let empty = queries::history(&resources, &identity, fixture.account, 0, "", 100, at).await.unwrap();
    assert!(empty.ids.is_empty() && empty.next_token.is_empty());
    queries::current(&resources, &identity, fixture.account, at).await.unwrap();
    assert!(notices.rx.try_recv().is_err());
    let unchanged = instances::get(&db, fixture.character, &first[0].goal.goal_id).await.unwrap().unwrap();
    assert_eq!(unchanged.remainder, fractional);
    assert_eq!(unchanged.definition, first[0].goal.definition);
    assert_eq!(unchanged.revision, 1);
    assert!(!ensure_at(&resources, fixture.account, fixture.character, at, false).await.unwrap().created);
    let later = at + 3 * infinity_core::consts::FILETIME_DAY;
    let later_outcome = ensure_at(&resources, fixture.account, fixture.character, later, false).await.unwrap();
    assert!(later_outcome.created);
    assert_eq!(later_outcome.month.as_ref().unwrap().version, a.month.as_ref().unwrap().version);
    assert_eq!(later_outcome.month.as_ref().unwrap().period_start, a.month.as_ref().unwrap().period_start);
    let count: i64 = db.query_one("SELECT count(*) AS n FROM game.daily_goal_periods WHERE character_id=$1 AND kind='day'", &[&fixture.character]).await.unwrap().get("n");
    assert_eq!(count, 2);
    let mut all = instances::period(&db, fixture.character, "day", a.bounds.day_start).await.unwrap();
    all.extend(instances::period(&db, fixture.character, "month", a.bounds.month_start).await.unwrap());
    all.extend(instances::period(&db, fixture.character, "day", later_outcome.bounds.day_start).await.unwrap());
    for day in 4..=6 {
        let occurrence = at + day * infinity_core::consts::FILETIME_DAY;
        let outcome = ensure_at(&resources, fixture.account, fixture.character, occurrence, false).await.unwrap();
        all.extend(instances::period(&db, fixture.character, "day", outcome.bounds.day_start).await.unwrap());
    }
    assert_eq!(all.len(), 33);
    for (index, goal) in all.iter().enumerate() {
        let update = instances::ProgressUpdate { progress: goal.target, remainder: BinaryAmount { coefficient: "0".into(), exponent: 0 }, completed_at: Some(at + index as i64), earned_omega: false, paid: false, has_unclaimed: true };
        assert!(instances::update_progress(&db, fixture.character, &goal.goal_id, goal.revision, &update).await.unwrap());
    }
    let page = queries::history(&resources, &identity, fixture.account, 25, "", 100, at).await.unwrap();
    assert_eq!(page.ids.len(), 25);
    assert!(!page.next_token.is_empty());
    let claimed = instances::get(&db, fixture.character, &page.ids[5]).await.unwrap().unwrap();
    assert!(instances::set_unclaimed(&db, fixture.character, &page.ids[5], claimed.revision, false).await.unwrap());
    let rest = queries::history(&resources, &identity, fixture.account, 25, &page.next_token, 100, at).await.unwrap();
    assert_eq!(rest.ids.len(), 8);
    assert!(rest.next_token.is_empty());
    assert!(rest.ids.iter().all(|id| !page.ids.contains(id)));
    assert!(matches!(queries::history(&resources, &identity, fixture.account, 101, "", 100, at).await, Err(Error::Invalid)));
    assert!(matches!(queries::history(&resources, &identity, fixture.account, 1, "malformed", 100, at).await, Err(Error::Invalid)));
    assert!(matches!(queries::history(&resources, &identity, fixture.account + 1, 25, &page.next_token, 100, at).await, Err(Error::Forbidden)));
    let collision_at = at + 7 * infinity_core::consts::FILETIME_DAY;
    let collision_bounds = periods::bounds(&db, collision_at).await.unwrap();
    let mut history = activation::load(&resources).await.unwrap();
    let token = activation::resolve(&resources, &mut history, "day", collision_bounds.day_start, collision_bounds.day_end).await.unwrap().unwrap();
    let planned = prepare(&resources, &token, fixture.character, collision_at).unwrap();
    let mut conflicting = all[0].clone();
    conflicting.goal_id = planned.goals[0].0.goal_id.clone();
    conflicting.slot_key = "collision".into();
    assert!(instances::insert(&db, &conflicting).await.unwrap());
    let original = instances::get(&db, fixture.character, &conflicting.goal_id).await.unwrap().unwrap();
    assert!(ensure_at(&resources, fixture.account, fixture.character, collision_at, false).await.is_err());
    assert!(periods::get(&db, fixture.character, "day", collision_bounds.day_start).await.unwrap().is_none());
    assert_eq!(instances::get(&db, fixture.character, &conflicting.goal_id).await.unwrap().unwrap(), original);
    world.unregister_pilot_generation(fixture.character, None);
    db.execute("DELETE FROM game.daily_goal_rewards WHERE goal_id IN (SELECT goal_id FROM game.daily_goal_instances WHERE character_id=$1)", &[&fixture.character]).await.unwrap();
    db.execute("DELETE FROM game.daily_goal_instances WHERE character_id=$1", &[&fixture.character]).await.unwrap();
    db.execute("DELETE FROM game.daily_goal_periods WHERE character_id=$1", &[&fixture.character]).await.unwrap();
    crate::services::tests::fixture::cleanup(&db, fixture).await.unwrap();
    std::fs::remove_dir_all(root).unwrap();
}
