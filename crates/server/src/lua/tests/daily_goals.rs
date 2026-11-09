//! Real Host publication, frozen daily assignments and process restart.
//! Uses server-authored Lua and dedicated initialized game/SDE tables; no client fixture.
use std::{path::Path, sync::Arc};
use infinity_core::{consts::{FILETIME_DAY, FILETIME_UNIX_EPOCH}};
use crate::{dao::{daily_goals::periods, lua_content::{self, activations}}, services::{daily_goals::{assignment, queries, Resources}, identity::Identity}};
pub(crate) mod payments;

const CHILD: &str = "INFINITY_DAILY_HOST_RESTART_CHILD";
const ROOT: &str = "INFINITY_DAILY_HOST_RESTART_ROOT";
const NAME: &str = "lua::tests::daily_goals::retained_assignment_survives_reload_restart_and_metadata_loss";

pub(crate) fn source(root: &Path, version: &str, changed: bool) {
    let content = root.join("content/daily_goals");
    std::fs::create_dir_all(content.join("data")).unwrap();
    let registration = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/lua/daily_goals.lua")).replace("content_version = \"1\"", &format!("content_version = \"{version}\""));
    let mut data = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/lua/content/daily_goals/data/daily_goals.json")).replacen("\"version\": \"1\"", &format!("\"version\": \"{version}\""), 1);
    let mut policy = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/lua/content/daily_goals/policy.lua")).to_owned();
    if changed {
        data = data.replace("\"key\":\"bonus\",\"target\":2", "\"key\":\"bonus\",\"target\":3");
        policy = policy.replace("return { keys = out }\n    end,\n}", "local reverse = {}\n        for i = #out, 1, -1 do reverse[#reverse + 1] = out[i] end\n        return { keys = reverse }\n    end,\n}");
    }
    std::fs::write(root.join("daily_goals.lua"), registration).unwrap();
    std::fs::write(content.join("policy.lua"), policy).unwrap();
    std::fs::write(content.join("data/daily_goals.json"), data).unwrap();
}

pub(crate) fn identity(character: i64) -> Identity {
    Identity { char_id: character, ship_id: None, corp_id: 0, station_id: None, system_id: None }
}

fn fingerprint(views: &[crate::services::daily_goals::GoalView]) -> Vec<String> {
    views.iter().map(|v| format!("{:?}|{:?}", v.goal, v.rewards)).collect()
}

pub(crate) async fn environment(root: &Path, missing: bool, registry: crate::progress::Registry) -> (crate::db::Db, Resources) {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap().with_progress_registry(registry);
    let sde = Arc::new(if missing { crate::sde::Sde::default() } else { crate::sde::Sde::load(&db).await.expect("initialized SDE required") });
    let world = crate::space::World::new(db.clone(), sde.clone(), 1);
    let hub = crate::chat::Hub::new("test", db.clone(), world.clone());
    let mut config = crate::config::Config::default();
    config.lua.enabled = true;
    config.lua.scripts_dir = root.to_string_lossy().into_owned();
    crate::lua::start(&config, &db, &sde, &world, &hub).await.unwrap();
    (db, Resources::from_world(&world).unwrap())
}
pub(crate) async fn reload() {
    crate::lua::HOST.get().unwrap().reload().await.unwrap();
}
async fn cleanup(db: &crate::db::Db, fixture: crate::services::tests::fixture::Fixture) -> Vec<String> {
    let mut errors = Vec::new();
    for sql in [
        "DELETE FROM game.daily_goal_admin_audit WHERE subject_id=$1 OR actor_id=$1",
        "DELETE FROM game.daily_goal_payments WHERE character_id=$1",
        "DELETE FROM game.daily_goal_rewards WHERE goal_id IN (SELECT goal_id FROM game.daily_goal_instances WHERE character_id=$1)",
        "DELETE FROM game.daily_goal_instances WHERE character_id=$1",
        "DELETE FROM game.daily_goal_periods WHERE character_id=$1",
    ] { if let Err(error) = db.execute(sql, &[&fixture.character]).await { errors.push(format!("{error:?}")); } }
    if let Err(error) = crate::services::tests::fixture::cleanup(db, fixture).await { errors.push(format!("{error:?}")); }
    errors
}

#[tokio::test]
#[ignore = "requires dedicated initialized SDE database and externally serialized Lua publication"]
async fn retained_assignment_survives_reload_restart_and_metadata_loss() {
    if let Some(phase) = std::env::var_os(CHILD) {
        let root = std::path::PathBuf::from(std::env::var_os(ROOT).unwrap());
        if phase == "seed" { seed(&root).await; } else { reopen(&root, phase == "missing").await; }
        return;
    }
    let root = std::env::temp_dir().join(format!("daily-host-{}-{}", std::process::id(), crate::handshake::random_u64()));
    std::fs::create_dir(&root).unwrap();
    let result = crate::tests::catch_scenario(async {
        for phase in ["seed", "restart", "missing"] {
            let status = std::process::Command::new(std::env::current_exe().unwrap()).args(["--exact", NAME, "--ignored", "--nocapture"]).env(CHILD, phase).env(ROOT, &root).status().unwrap();
            assert!(status.success(), "{phase} child failed");
        }
    }).await;
    let mut errors = Vec::new();
    let cleanup_result = crate::tests::catch_scenario(async {
        if root.join("owner.json").exists() {
            crate::tests::init_config();
            let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
            let owner: (i64, i64) = serde_json::from_slice(&std::fs::read(root.join("owner.json")).unwrap()).unwrap();
            errors.extend(cleanup(&db, crate::services::tests::fixture::Fixture { account: owner.0, character: owner.1 }).await);
        }
    }).await;
    let removed = std::fs::remove_dir_all(&root);
    if let Err(panic) = result { std::panic::resume_unwind(panic); }
    if let Err(panic) = cleanup_result { std::panic::resume_unwind(panic); }
    assert!(errors.is_empty(), "database cleanup failed: {errors:?}");
    removed.unwrap();
}

async fn seed(root: &Path) {
    let id = crate::handshake::random_u64();
    let a = format!("daily-host-{id}-a");
    let b = format!("daily-host-{id}-b");
    let scripts = root.join("scripts");
    source(&scripts, &a, false);
    let (db, resources) = environment(&scripts, false, crate::progress::Registry::default()).await;
    let fixture = crate::services::tests::fixture::create(&db).await.unwrap();
    std::fs::write(root.join("owner.json"), serde_json::to_vec(&(fixture.account, fixture.character)).unwrap()).unwrap();
    let host = crate::lua::HOST.get().unwrap();
    let result = crate::tests::catch_scenario(async {
        let row_a = lua_content::load(&db, "daily_goals", &a).await.unwrap().unwrap();
        source(&scripts, &b, true);
        host.reload().await.unwrap();
        assert_eq!(crate::lua::content_policy("daily_goals").unwrap().version(), b);
        let row_b = lua_content::load(&db, "daily_goals", &b).await.unwrap().unwrap();
        let bounds = periods::bounds(&db, FILETIME_UNIX_EPOCH + 18_000 * FILETIME_DAY).await.unwrap();
        let a_at = bounds.month_start - FILETIME_DAY;
        let b_at = bounds.month_start + 5 * FILETIME_DAY;
        let at_a = bounds.month_start + FILETIME_DAY;
        let at = bounds.month_start + 7 * FILETIME_DAY;
        let mixed = periods::bounds(&db, at).await.unwrap();
        assert!(mixed.day_start > b_at && mixed.month_start > a_at && mixed.month_start <= b_at);
        db.transaction(|tx, _: &mut ()| {
            let a = row_a.clone(); let b = row_b.clone();
            Box::pin(async move {
                assert!(activations::append_if_changed(tx, &a, a_at).await?);
                assert!(activations::append_if_changed(tx, &b, b_at).await?);
                Ok::<_, crate::db::Error>(())
            })
        }, |(), ()| async { Ok(()) }).await.unwrap();
        let log = activations::list(&db, "daily_goals").await.unwrap();
        let tail = &log[log.len()-2..];
        assert_eq!((&tail[0].version, &tail[0].fingerprint, tail[0].published_at), (&a, &row_a.fingerprint, a_at));
        assert_eq!((&tail[1].version, &tail[1].fingerprint, tail[1].published_at), (&b, &row_b.fingerprint, b_at));
        let first = assignment::ensure_at(&resources, fixture.account, fixture.character, at_a, true).await.unwrap();
        assert_eq!(first.day.unwrap().version, a);
        assert_eq!(first.month.unwrap().version, a);
        let first_views = first.current.unwrap();
        assert_eq!(first_views.len(), 13);
        assert_eq!(first_views.iter().find(|v| v.goal.category == "bonus").unwrap().goal.target, 2);
        let first_before = fingerprint(&first_views);
        let next = assignment::ensure_at(&resources, fixture.account, fixture.character, at, true).await.unwrap();
        assert_eq!(next.day.unwrap().version, b);
        assert_eq!(next.month.unwrap().version, a);
        let views = next.current.unwrap();
        assert_eq!(views.len(), 13);
        assert_eq!(views.iter().filter(|v| v.goal.version == b).count(), 5);
        assert_eq!(views.iter().filter(|v| v.goal.version == a).count(), 8);
        assert_eq!(views.iter().filter(|v| v.goal.kind == "day" && v.goal.category == "ordinary").count(), 4);
        assert_eq!(views.iter().filter(|v| v.goal.kind == "day" && v.goal.category == "bonus").count(), 1);
        assert_eq!(views.iter().filter(|v| v.goal.slot_key.starts_with("base:")).count(), 4);
        assert_eq!(views.iter().filter(|v| v.goal.slot_key.starts_with("omega:")).count(), 4);
        let goal_ids: std::collections::HashSet<_> = views.iter().map(|v| &v.goal.goal_id).collect();
        let asset_ids: std::collections::HashSet<_> = views.iter().flat_map(|v| v.rewards.iter().map(|r| &r.asset_id)).collect();
        assert_eq!(goal_ids.len(), 13);
        assert_eq!(asset_ids.len(), views.iter().map(|v| v.rewards.len()).sum::<usize>());
        assert_eq!(views.iter().find(|v| v.goal.kind == "day" && v.goal.category == "bonus").unwrap().goal.target, 3);
        let before = fingerprint(&views);
        let pin_a = resources.policies.resolve("daily_goals", &a).await.unwrap();
        let pin_b = resources.policies.resolve("daily_goals", &b).await.unwrap();
        use crate::lua::content::daily_goals::plans::{self, ProjectInput, Projection};
        let input = ProjectInput { entries: vec![Projection { key: "one".into(), selected: true }, Projection { key: "two".into(), selected: true }] };
        assert_eq!(plans::project(&pin_a, &input, None).unwrap().keys, ["one", "two"]);
        assert_eq!(plans::project(&pin_b, &input, None).unwrap().keys, ["two", "one"]);
        source(&scripts, &a, false);
        host.reload().await.unwrap();
        assert_eq!(crate::lua::content_policy("daily_goals").unwrap().version(), a);
        let publications = activations::list(&db, "daily_goals").await.unwrap();
        assert_eq!(publications.last().unwrap().version, a);
        assert_eq!(publications[publications.len()-2].version, b);
        assert_eq!(fingerprint(&queries::current(&resources, &identity(fixture.character), fixture.account, at).await.unwrap()), before);
        assert_eq!(fingerprint(&queries::current(&resources, &identity(fixture.character), fixture.account, at_a).await.unwrap()), first_before);
        let fresh = assignment::ensure_at(&resources, fixture.account, fixture.character, at + FILETIME_DAY, true).await.unwrap();
        assert_eq!(fresh.day.unwrap().version, b);
        assert_eq!(fresh.month.unwrap().version, a);
        assert_eq!(fresh.current.unwrap().len(), 13);
        assert_eq!(plans::project(&pin_b, &input, None).unwrap().keys, ["two", "one"]);
        let count: i64 = db.query_one("SELECT count(*) AS n FROM game.daily_goal_periods WHERE character_id=$1 AND kind='day'", &[&fixture.character]).await.unwrap().get("n");
        assert_eq!(count, 3);
        let marker = serde_json::json!({"a": a, "b": b, "at": at, "fingerprint": before, "publications": publications.len()});
        std::fs::write(root.join("marker.json"), serde_json::to_vec(&marker).unwrap()).unwrap();
    }).await;
    source(&scripts, &a, false);
    host.reload().await.unwrap();
    if let Err(panic) = result { std::panic::resume_unwind(panic); }
}

async fn reopen(root: &Path, missing: bool) {
    let marker: serde_json::Value = serde_json::from_slice(&std::fs::read(root.join("marker.json")).unwrap()).unwrap();
    let owner: (i64, i64) = serde_json::from_slice(&std::fs::read(root.join("owner.json")).unwrap()).unwrap();
    let scripts = if missing { let p = root.join("empty"); std::fs::create_dir_all(&p).unwrap(); p } else { root.join("scripts") };
    let (db, resources) = environment(&scripts, missing, crate::progress::Registry::default()).await;
    if !missing {
        assert_eq!(activations::list(&db, "daily_goals").await.unwrap().len(), marker["publications"].as_u64().unwrap() as usize);
    }
    let at = marker["at"].as_i64().unwrap();
    for version in [marker["a"].as_str().unwrap(), marker["b"].as_str().unwrap()] {
        assert!(lua_content::load(&db, "daily_goals", version).await.unwrap().is_some());
        resources.policies.resolve("daily_goals", version).await.unwrap();
    }
    let views = queries::current(&resources, &identity(owner.1), owner.0, at).await.unwrap();
    let expected: Vec<String> = serde_json::from_value(marker["fingerprint"].clone()).unwrap();
    assert_eq!(fingerprint(&views), expected);
    for view in &views {
        let read = queries::get(&resources, &identity(owner.1), owner.0, &view.goal.goal_id, at).await.unwrap();
        assert_eq!(fingerprint(&[read]), fingerprint(std::slice::from_ref(view)));
    }
}
