//! Real-host receipt replay, stale snapshots, cascades and fractional HP.
use std::sync::Arc;
use crate::{dao::progress::{self as journal, JournalEvent}, db::outbox::{Outbox, SourceKey}, lua::events::{GameEvent, resources::{FactContext, FactOrigin}}, progress::{self, ApplyOutcome, ClaimMode, ContentProgressResources, DeliveryKey, PrepareOutcome}};

#[tokio::test]
#[ignore = "requires isolated test process, dedicated INFINITY_TEST_DATABASE_URL with SDE, and external flock"]
async fn real_prepared_receipts_preserve_cascades_and_replay() {
    const CHILD: &str = "INFINITY_DAILY_PREPARED_NO_EFFECT_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "services::daily_goals::progress::tests::prepared::real_prepared_receipts_preserve_cascades_and_replay", "--ignored", "--nocapture"])
            .env(CHILD, "1").status().unwrap();
        assert!(status.success());
        return;
    }
    crate::tests::init_config();
    let registration = super::super::registration();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap()
        .with_progress_registry(progress::Registry::new(vec![], vec![registration]).unwrap());
    db.execute("INSERT INTO game.progress_subscriptions (consumer_key,generation,activated_at,version,enabled) VALUES ($1,1,1,1,true) ON CONFLICT (consumer_key) DO NOTHING", &[&registration.consumer_key]).await.unwrap();
    db.execute("INSERT INTO game.progress_subscription_kinds (consumer_key,kind) VALUES ($1,'WreckSalvaged') ON CONFLICT DO NOTHING", &[&registration.consumer_key]).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.expect("real SDE required"));
    let world = crate::space::World::new(db.clone(), sde.clone(), 1);
    let hub = crate::chat::Hub::new("test", db.clone(), world.clone());
    let nonce = crate::handshake::random_u64();
    let version = format!("prepared-test-{nonce}");
    let root = std::env::temp_dir().join(format!("daily-prepared-{nonce}"));
    std::fs::create_dir_all(root.join("content/daily_goals/data")).unwrap();
    let script = include_str!("../../../../../lua/daily_goals.lua").replace("content_version = \"1\"", &format!("content_version = \"{version}\""));
    let data = include_str!("../../../../../lua/content/daily_goals/data/daily_goals.json")
        .replacen("\"version\": \"1\"", &format!("\"version\": \"{version}\""), 1)
        .replace("[\"npc\",\"damage\",\"loyalty\"]", "[\"damage\"]")
        .replace("[\"scan\",\"jump\"]", "[\"jump\"]")
        .replace("\"key\":\"jump\",\"target\":5", "\"key\":\"jump\",\"target\":1")
        .replace("\"key\":\"bonus\",\"target\":2", "\"key\":\"bonus\",\"target\":1");
    std::fs::write(root.join("daily_goals.lua"), script).unwrap();
    std::fs::write(root.join("content/daily_goals/policy.lua"), include_bytes!("../../../../../lua/content/daily_goals/policy.lua")).unwrap();
    std::fs::write(root.join("content/daily_goals/data/daily_goals.json"), data).unwrap();
    let mut config = crate::config::Config::default();
    config.lua.enabled = true;
    config.lua.scripts_dir = root.to_string_lossy().into_owned();
    crate::lua::start(&config, &db, &sde, &world, &hub).await.expect("real daily host required");
    let fixture = crate::services::tests::fixture::create(&db).await.unwrap();
    let now = crate::client::filetime_now();
    let wreck = i64::try_from(nonce & i64::MAX as u64).unwrap().max(1);
    let context = FactContext { corporation_id: None, alliance_id: None, faction_id: None, ship_id: None, ship_type_id: None, system_id: None, station_id: None, origin: FactOrigin::Gm };
    let event = GameEvent::WreckSalvaged { char_id: fixture.character, wreck_id: wreck, wreck_type: 1, occurred_at: now, context };
    let row = JournalEvent { source_namespace: "business".into(), source_key: format!("WreckSalvaged:{wreck}"), schema_version: progress::codec::SCHEMA_VERSION, kind: event.kind().into(), occurred_at: now, recorded_at: now, payload: progress::codec::encode(&event).unwrap(), context: serde_json::to_string(&context).unwrap(), origin: context.origin.as_str().into(), ordinal: 0, subject_character_id: Some(fixture.character) };
    let inserted = journal::insert_batch(&db, &[row]).await.unwrap();
    assert!(inserted[0].2);
    let key = DeliveryKey { consumer_key: registration.consumer_key.into(), event_id: inserted[0].1.clone() };
    let resources = ContentProgressResources { db: db.clone(), sde: sde.clone(), world: world.clone(), policies: crate::lua::retained_policies().unwrap() };
    let plan = match super::super::prepare::delivery(resources.clone(), key.clone()).await.unwrap() { PrepareOutcome::Ready(plan) => plan, PrepareOutcome::AlreadyApplied => panic!("new receipt already applied") };
    let (mut client, connection) = tokio_postgres::connect(&crate::config::tests::test_database().url, tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let tx = client.transaction().await.unwrap();
    let mut outbox = Outbox::default();
    assert!(matches!(progress::apply_once_tx(&tx, &mut outbox, &sde, &registration, key.clone(), Some(plan.as_ref()), ClaimMode::Blocking).await.unwrap(), ApplyOutcome::Applied));
    tx.commit().await.unwrap();
    assert!(matches!(super::super::prepare::delivery(resources.clone(), key).await.unwrap(), PrepareOutcome::AlreadyApplied));
    let count: i64 = db.query_one("SELECT count(*) AS count FROM game.daily_goal_instances WHERE character_id=$1", &[&fixture.character]).await.unwrap().get("count");
    assert_eq!(count, 0);
    let occurrence = crate::dao::daily_goals::periods::bounds(&db, now).await.unwrap().month_end + 1;
    db.execute("INSERT INTO game.progress_subscription_kinds (consumer_key,kind) VALUES ($1,'Jumped') ON CONFLICT DO NOTHING", &[&registration.consumer_key]).await.unwrap();
    let jump = GameEvent::Jumped { char_id: fixture.character, ship_id: 2, from_system_id: 3, to_system_id: 4 };
    let row = JournalEvent { source_namespace: "business".into(), source_key: format!("Jumped:{wreck}"), schema_version: progress::codec::SCHEMA_VERSION, kind: jump.kind().into(), occurred_at: occurrence, recorded_at: now, payload: progress::codec::encode(&jump).unwrap(), context: "{}".into(), origin: "server".into(), ordinal: 0, subject_character_id: Some(fixture.character) };
    let inserted = journal::insert_batch(&db, &[row]).await.unwrap();
    assert!(inserted[0].2);
    let jump_key = DeliveryKey { consumer_key: registration.consumer_key.into(), event_id: inserted[0].1.clone() };
    let jump_plan = match super::super::prepare::delivery(resources.clone(), jump_key.clone()).await.unwrap() { PrepareOutcome::Ready(plan) => plan, PrepareOutcome::AlreadyApplied => panic!("new receipt already applied") };
    let grade = crate::dao::accounts::subscriptions::by_user(&db, fixture.account).await.unwrap();
    assert!(crate::dao::accounts::subscriptions::set_grade(&db, fixture.account, grade.grade, grade.omega_until, grade.grace_until, grade.revision).await.unwrap());
    let tx = client.transaction().await.unwrap();
    assert!(matches!(progress::apply_once_tx(&tx, &mut Outbox::default(), &sde, &registration, jump_key.clone(), Some(jump_plan.as_ref()), ClaimMode::Blocking).await, Err(progress::ApplyError::Reprepare)));
    tx.rollback().await.unwrap();
    assert!(matches!(journal::delivery(&db, &jump_key).await.unwrap(), journal::DeliveryState::Pending(_)));
    let stale = match super::super::prepare::delivery(resources.clone(), jump_key.clone()).await.unwrap() { PrepareOutcome::Ready(plan) => plan, PrepareOutcome::AlreadyApplied => panic!("receipt unexpectedly applied") };
    let bounds = crate::dao::daily_goals::periods::bounds(&db, occurrence).await.unwrap();
    let rows = crate::dao::daily_goals::instances::period(&db, fixture.character, "day", bounds.day_start).await.unwrap();
    let other = rows.iter().find(|row| row.category == "ordinary").expect("assigned ordinary goal");
    let mut extra = (*other).clone();
    extra.generation += 1;
    extra.goal_id = crate::services::daily_goals::ids::goal(fixture.character, extra.period_start, &extra.version, &extra.slot_key, extra.generation);
    assert!(crate::dao::daily_goals::instances::insert(&db, &extra).await.unwrap());
    let tx = client.transaction().await.unwrap();
    assert!(matches!(progress::apply_once_tx(&tx, &mut Outbox::default(), &sde, &registration, jump_key.clone(), Some(stale.as_ref()), ClaimMode::Blocking).await, Err(progress::ApplyError::Reprepare)));
    tx.rollback().await.unwrap();
    assert!(matches!(journal::delivery(&db, &jump_key).await.unwrap(), journal::DeliveryState::Pending(_)));
    db.execute("DELETE FROM game.daily_goal_instances WHERE character_id=$1 AND goal_id=$2::text::uuid", &[&fixture.character, &extra.goal_id]).await.unwrap();
    let update = crate::dao::daily_goals::instances::ProgressUpdate { progress: other.progress, remainder: other.remainder.clone(), completed_at: other.completed_at, earned_omega: other.earned_omega, paid: other.paid, has_unclaimed: other.has_unclaimed };
    assert!(crate::dao::daily_goals::instances::update_progress(&db, fixture.character, &other.goal_id, other.revision, &update).await.unwrap());
    let tx = client.transaction().await.unwrap();
    assert!(matches!(progress::apply_once_tx(&tx, &mut Outbox::default(), &sde, &registration, jump_key.clone(), Some(stale.as_ref()), ClaimMode::Blocking).await, Err(progress::ApplyError::Reprepare)));
    tx.rollback().await.unwrap();
    assert!(matches!(journal::delivery(&db, &jump_key).await.unwrap(), journal::DeliveryState::Pending(_)));
    let plan = match super::super::prepare::delivery(resources.clone(), jump_key.clone()).await.unwrap() { PrepareOutcome::Ready(plan) => plan, PrepareOutcome::AlreadyApplied => panic!("receipt unexpectedly applied") };
    let (sender, _receiver) = tokio::sync::mpsc::unbounded_channel();
    world.register_pilot(fixture.character, sender);
    world.set_online_session(fixture.character, Some(infinity_core::session::OnlineSession {
        character_id: fixture.character, user_id: Some(fixture.account), session_id: 1, ship_id: None, station_id: None,
        system_id: None, corporation_id: None, alliance_id: None, faction_id: None, fleet_id: None, role: 0, corporation_role: 0,
    }));
    let mut notices = crate::gateway::tests::notice_stream(&world, [7; 16], fixture.character, 32);
    let result = db.transaction::<_, _, progress::ApplyError, Outbox, _, _, _>(|tx, outbox| {
        let sde = Arc::clone(&sde);
        let plan = Arc::clone(&plan);
        let key = jump_key.clone();
        Box::pin(async move { progress::apply_once_tx(tx, outbox, &sde, &registration, key, Some(plan.as_ref()), ClaimMode::Blocking).await })
    }, |value, outbox| async { crate::db::outbox::dispatch_world(value, outbox, &world).map_err(progress::ApplyError::from) }).await.unwrap();
    assert!(matches!(result, ApplyOutcome::Applied));
    use prost_reflect::ReflectMessage;
    let (_, first) = crate::gateway::tests::decode_notice(&notices.rx.try_recv().expect("progress notice"));
    let (_, second) = crate::gateway::tests::decode_notice(&notices.rx.try_recv().expect("completion notice"));
    assert_eq!(first.descriptor().full_name(), "eve_public.dailygoal.api.ProgressedNotice");
    assert_eq!(second.descriptor().full_name(), "eve_public.dailygoal.api.CompletedNotice");
    let rows = crate::dao::daily_goals::instances::period(&db, fixture.character, "day", bounds.day_start).await.unwrap();
    let jump_goal = rows.iter().find(|row| crate::services::daily_goals::frozen::validate(row).unwrap().template.key == "jump").expect("jump goal assigned");
    assert_eq!(jump_goal.progress, jump_goal.target);
    assert!(jump_goal.completed_at.is_some());
    let bonus = rows.iter().find(|row| row.category == "bonus").expect("assigned bonus");
    assert_eq!(bonus.progress, bonus.target);
    assert!(bonus.completed_at.is_some());
    let monthly = crate::dao::daily_goals::instances::period(&db, fixture.character, "month", bounds.month_start).await.unwrap();
    assert_eq!(monthly.len(), 8);
    assert!(monthly.iter().all(|row| row.progress == 1));
    let sources = crate::dao::daily_goals::instances::completed_sources(&db, fixture.character, bounds.month_start, bounds.month_end).await.unwrap();
    assert_eq!(sources.iter().map(|source| source.period_start).collect::<Vec<_>>(), vec![bounds.day_start]);
    let source = SourceKey::canonical("DailyGoalCompleted", &jump_goal.goal_id, 0);
    let event = journal::source(&db, "business", source.as_str()).await.unwrap().expect("completion event");
    assert_eq!(event.ordinal, 0);
    assert!(matches!(super::super::prepare::delivery(resources.clone(), jump_key).await.unwrap(), PrepareOutcome::AlreadyApplied));
    use crate::lua::events::combat::{BinaryAmount, CombatEntity, CombatSlice, HpLayer};
    db.execute("INSERT INTO game.progress_subscription_kinds (consumer_key,kind) VALUES ($1,'DamageApplied') ON CONFLICT DO NOTHING", &[&registration.consumer_key]).await.unwrap();
    let natural = FactContext { origin: FactOrigin::Natural, ..context };
    let actor = CombatEntity { item_id: 1, type_id: 1, incarnation: [1; 16], character_id: Some(fixture.character), corporation_id: None, alliance_id: None, faction_id: None };
    let recipient = CombatEntity { item_id: 2, type_id: 1, incarnation: [2; 16], character_id: None, corporation_id: None, alliance_id: None, faction_id: None };
    let half = BinaryAmount { coefficient: "1".into(), exponent: -1 };
    let slice = |index, at| CombatSlice { index, qualification_epoch: 0, boundary_at: at, active_until: at, first_occurred_at: at, last_occurred_at: at, successful_count: 1, amount: half.clone() };
    let next_day = occurrence + infinity_core::consts::FILETIME_DAY;
    let damage = GameEvent::DamageApplied { char_id: Some(fixture.character), context: natural, source: actor, target: recipient, module_id: None, weapon_type_id: None, effect_id: None, layer: HpLayer::Shield, slices: vec![slice(0, occurrence), slice(1, occurrence + 1), slice(2, next_day)] };
    let row = JournalEvent { source_namespace: "business".into(), source_key: format!("DamageApplied:{wreck}:1:0:0:0"), schema_version: progress::codec::SCHEMA_VERSION, kind: damage.kind().into(), occurred_at: occurrence, recorded_at: now, payload: progress::codec::encode(&damage).unwrap(), context: serde_json::to_string(&natural).unwrap(), origin: natural.origin.as_str().into(), ordinal: 0, subject_character_id: Some(fixture.character) };
    let inserted = journal::insert_batch(&db, &[row]).await.unwrap();
    assert!(inserted[0].2);
    let damage_key = DeliveryKey { consumer_key: registration.consumer_key.into(), event_id: inserted[0].1.clone() };
    let plan = match super::super::prepare::delivery(resources.clone(), damage_key.clone()).await.unwrap() { PrepareOutcome::Ready(plan) => plan, PrepareOutcome::AlreadyApplied => panic!("new damage receipt already applied") };
    let result = db.transaction::<_, _, progress::ApplyError, Outbox, _, _, _>(|tx, outbox| {
        let sde = Arc::clone(&sde);
        let plan = Arc::clone(&plan);
        let key = damage_key.clone();
        Box::pin(async move { progress::apply_once_tx(tx, outbox, &sde, &registration, key, Some(plan.as_ref()), ClaimMode::Blocking).await })
    }, |value, outbox| async { crate::db::outbox::dispatch_world(value, outbox, &world).map_err(progress::ApplyError::from) }).await.unwrap();
    assert!(matches!(result, ApplyOutcome::Applied));
    let first_day = crate::dao::daily_goals::instances::period(&db, fixture.character, "day", bounds.day_start).await.unwrap();
    let second_bounds = crate::dao::daily_goals::periods::bounds(&db, next_day).await.unwrap();
    let second_day = crate::dao::daily_goals::instances::period(&db, fixture.character, "day", second_bounds.day_start).await.unwrap();
    let first_goal = first_day.iter().find(|row| crate::services::daily_goals::frozen::validate(row).unwrap().template.key == "damage").unwrap();
    let second_goal = second_day.iter().find(|row| crate::services::daily_goals::frozen::validate(row).unwrap().template.key == "damage").unwrap();
    assert_eq!(first_goal.progress, 1);
    assert_eq!(first_goal.remainder.coefficient, "0");
    assert_eq!(second_goal.progress, 0);
    assert_eq!(second_goal.remainder, half);
    assert!(matches!(super::super::prepare::delivery(resources.clone(), damage_key).await.unwrap(), PrepareOutcome::AlreadyApplied));
    // The two plans contain different daily rows but the same eight month rows.
    let mut day_keys = Vec::new();
    for at in [occurrence, next_day] {
        let id = i64::try_from(crate::handshake::random_u64() & i64::MAX as u64).unwrap().max(1);
        let event = GameEvent::Jumped { char_id: fixture.character, ship_id: 2, from_system_id: 3, to_system_id: 4 };
        let row = JournalEvent { source_namespace: "business".into(), source_key: format!("Jumped:{id}"), schema_version: progress::codec::SCHEMA_VERSION, kind: event.kind().into(), occurred_at: at, recorded_at: now, payload: progress::codec::encode(&event).unwrap(), context: "{}".into(), origin: "server".into(), ordinal: 0, subject_character_id: Some(fixture.character) };
        let inserted = journal::insert_batch(&db, &[row]).await.unwrap();
        assert!(inserted[0].2);
        day_keys.push(DeliveryKey { consumer_key: registration.consumer_key.into(), event_id: inserted[0].1.clone() });
    }
    let first_plan = match super::super::prepare::delivery(resources.clone(), day_keys[0].clone()).await.unwrap() { PrepareOutcome::Ready(plan) => plan, PrepareOutcome::AlreadyApplied => panic!("new receipt already applied") };
    let second_plan = match super::super::prepare::delivery(resources.clone(), day_keys[1].clone()).await.unwrap() { PrepareOutcome::Ready(plan) => plan, PrepareOutcome::AlreadyApplied => panic!("new receipt already applied") };
    let (mut client2, connection2) = tokio_postgres::connect(&crate::config::tests::test_database().url, tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection2.await.unwrap(); });
    let tx1 = client.transaction().await.unwrap();
    let tx2 = client2.transaction().await.unwrap();
    first_plan.prelock(&tx1).await.unwrap();
    tx2.batch_execute("SET LOCAL lock_timeout = '100ms'").await.unwrap();
    assert!(matches!(second_plan.prelock(&tx2).await, Err(progress::ApplyError::Call(_))));
    tx2.rollback().await.unwrap();
    tx1.rollback().await.unwrap();
    for key in day_keys {
        let plan = match super::super::prepare::delivery(resources.clone(), key.clone()).await.unwrap() { PrepareOutcome::Ready(plan) => plan, PrepareOutcome::AlreadyApplied => panic!("receipt unexpectedly applied") };
        let result = db.transaction::<_, _, progress::ApplyError, Outbox, _, _, _>(|tx, outbox| {
            let sde = Arc::clone(&sde);
            let plan = Arc::clone(&plan);
            let key = key.clone();
            Box::pin(async move { progress::apply_once_tx(tx, outbox, &sde, &registration, key, Some(plan.as_ref()), ClaimMode::Blocking).await })
        }, |value, outbox| async { crate::db::outbox::dispatch_world(value, outbox, &world).map_err(progress::ApplyError::from) }).await.unwrap();
        assert!(matches!(result, ApplyOutcome::Applied));
    }
    let sources = crate::dao::daily_goals::instances::completed_sources(&db, fixture.character, bounds.month_start, bounds.month_end).await.unwrap();
    let dates: std::collections::BTreeSet<_> = sources.iter().map(|source| source.period_start).collect();
    assert_eq!(dates, [bounds.day_start, second_bounds.day_start].into_iter().collect());
    world.unregister_pilot_generation(fixture.character, Some(1));
    db.execute("DELETE FROM game.daily_goal_rewards WHERE goal_id IN (SELECT goal_id FROM game.daily_goal_instances WHERE character_id=$1)", &[&fixture.character]).await.unwrap();
    db.execute("DELETE FROM game.daily_goal_instances WHERE character_id=$1", &[&fixture.character]).await.unwrap();
    db.execute("DELETE FROM game.daily_goal_periods WHERE character_id=$1", &[&fixture.character]).await.unwrap();
    // Journal rows are append-only; only the test's profile and account are removed.
    db.execute("DELETE FROM game.characters WHERE character_id=$1", &[&fixture.character]).await.unwrap();
    db.execute("DELETE FROM game.accounts WHERE user_id=$1", &[&fixture.account]).await.unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
#[ignore = "requires isolated test process, dedicated INFINITY_TEST_DATABASE_URL with SDE, and external flock"]
async fn retained_completion_matchers_filter_real_virtual_cascades() {
    const CHILD: &str = "INFINITY_DAILY_RESTRICTED_CASCADE_CHILD";
    let scenario = match std::env::var(CHILD) {
        Ok(value) => value,
        Err(_) => {
            for value in ["uuid", "category", "defaults"] {
                let status = std::process::Command::new(std::env::current_exe().unwrap())
                    .args(["--exact", "services::daily_goals::progress::tests::prepared::retained_completion_matchers_filter_real_virtual_cascades", "--ignored", "--nocapture"])
                    .env(CHILD, value).status().unwrap();
                assert!(status.success());
            }
            return;
        }
    };
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    use crate::lua::tests::daily_goals as fixture_host;
    let fixture = crate::services::tests::fixture::create(&db).await.unwrap();
    let current = crate::client::filetime_now();
    let now = crate::dao::daily_goals::periods::bounds(&db, current).await.unwrap().month_end + 1;
    let bounds = crate::dao::daily_goals::periods::bounds(&db, now).await.unwrap();
    let nonce = crate::handshake::random_u64();
    let version = format!("restricted-test-{nonce}");
    let ordinary = crate::services::daily_goals::ids::goal(fixture.character, bounds.day_start, &version, "ordinary:0", 0);
    let other = crate::services::daily_goals::ids::goal(fixture.character, bounds.day_start, &version, "ordinary:1", 0);
    let bonus = crate::services::daily_goals::ids::goal(fixture.character, bounds.day_start, &version, "bonus", 0);
    let root = std::env::temp_dir().join(format!("daily-restricted-{nonce}"));
    fixture_host::source(&root, &version, false);
    let mut data: serde_json::Value = serde_json::from_str(include_str!("../../../../../lua/content/daily_goals/data/daily_goals.json")).unwrap();
    data["version"] = version.clone().into();
    if scenario != "defaults" {
        data["bonus"]["target"] = 1.into();
        data["bonus"]["method"]["entries"][0]["goal"] = serde_json::json!({"kind":"goal", "uuid":ordinary.to_uppercase()});
        if scenario == "category" { data["bonus"]["method"]["entries"][0]["category"] = serde_json::json!({"kind":"bonus"}); }
        data["monthly_base"][0]["method"]["entries"][0]["goal"] = serde_json::json!({"kind":"goal", "uuid":bonus.to_uppercase()});
    }
    std::fs::write(root.join("content/daily_goals/data/daily_goals.json"), serde_json::to_vec(&data).unwrap()).unwrap();
    let registry = progress::Registry::new(vec![], vec![]).unwrap();
    let (db, resources) = fixture_host::environment(&root, false, registry).await;
    let outcome = crate::services::daily_goals::assignment::ensure_at(&resources, fixture.account, fixture.character, now, false).await.unwrap();
    let day = outcome.day.unwrap();
    let month = outcome.month.unwrap();
    let periods = [((day.kind.clone(), day.period_start), day), ((month.kind.clone(), month.period_start), month)].into_iter().collect();
    let mut state = super::super::prepare::load_state(&resources, fixture.account, fixture.character, periods, outcome.pins, false).await.unwrap();
    if scenario != "defaults" {
        let historical_day = if bounds.day_start == bounds.month_start { bounds.day_start + 1 } else { bounds.month_start };
        state.bonus_sources.get_mut(&bounds.month_start).unwrap().insert(crate::dao::daily_goals::instances::CompletedSource {
            goal_id: "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa".into(), period_start: historical_day,
        });
    }
    super::super::state::plan_paid(&mut state, &other, bounds.day_start, bounds.month_start, now).unwrap();
    assert_eq!(state.goals[&bonus].goal.progress, if scenario == "defaults" { 1 } else { 0 });
    let restricted = state.goals.values().find(|g| g.meta.baseline.slot_key == "base:0").unwrap().meta.baseline.goal_id.clone();
    let unrestricted = state.goals.values().find(|g| g.meta.baseline.slot_key == "base:1").unwrap().meta.baseline.goal_id.clone();
    assert_eq!(state.goals[&restricted].goal.progress, 0);
    assert_eq!(state.goals[&unrestricted].goal.progress, if scenario == "defaults" { 0 } else { 1 });
    super::super::state::plan_paid(&mut state, &ordinary, bounds.day_start, bounds.month_start, now).unwrap();
    if scenario == "uuid" || scenario == "defaults" {
        assert_eq!(state.goals[&bonus].goal.progress, if scenario == "defaults" { 2 } else { 1 });
        assert!(state.goals[&bonus].goal.completed_at.is_some());
        assert_eq!(state.goals[&restricted].goal.progress, 1);
        assert_eq!(state.goals[&unrestricted].goal.progress, if scenario == "defaults" { 1 } else { 2 });
    } else {
        assert_eq!(state.goals[&bonus].goal.progress, 0);
        assert_eq!(state.goals[&restricted].goal.progress, 0);
    }
    db.execute("DELETE FROM game.daily_goal_rewards WHERE goal_id IN (SELECT goal_id FROM game.daily_goal_instances WHERE character_id=$1)", &[&fixture.character]).await.unwrap();
    db.execute("DELETE FROM game.daily_goal_instances WHERE character_id=$1", &[&fixture.character]).await.unwrap();
    db.execute("DELETE FROM game.daily_goal_periods WHERE character_id=$1", &[&fixture.character]).await.unwrap();
    db.execute("DELETE FROM game.characters WHERE character_id=$1", &[&fixture.character]).await.unwrap();
    db.execute("DELETE FROM game.accounts WHERE user_id=$1", &[&fixture.account]).await.unwrap();
    std::fs::remove_dir_all(root).unwrap();
}
