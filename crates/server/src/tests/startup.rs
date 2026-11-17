//! Real `main` startup regression for `lua.enabled = false`. Each scenario runs
//! the production `main` in an independent child while the parent holds the
//! final game bind: reaching the structured `AddrInUse` proves the catalogue
//! load, the HostUnavailable capability projection, sovereignty recovery, the
//! scheduler and every ordinary seed completed. The retained Hub fixture is a
//! real published package; only rows this test owns are created or removed.
use infinity_core::consts::OWNER_UNOWNED;

const NAME: &str = "tests::startup::disabled_lua_completes_startup_with_retained_hubs";
const CHILD: &str = "INFINITY_STARTUP_CHILD";
const EXPECT: &str = "INFINITY_STARTUP_EXPECT";

/// Job identities `main` seeds unconditionally; cleanup deletes only these.
const SEEDED_JOBS: [&str; 8] = [
    crate::scheduler::MARKET, crate::scheduler::CONTRACTS, crate::scheduler::BILLS, crate::scheduler::MINING_LEDGER,
    crate::scheduler::MINING_PROGRESS, crate::scheduler::MAP_HISTORY, crate::progress::DRAIN_KIND, crate::services::daily_goals::rollover::KIND,
];

/// Rows this test owns, filled as the scenario progresses so cleanup is exact.
#[derive(Default)]
struct Owned { root: std::path::PathBuf, system: Option<i64>, item: Option<i64>, version: Option<String>, fixture: Option<crate::services::tests::fixture::Fixture>, jobs: bool }

/// One child run of the production `main`; `mode` selects the expected
/// failure shape and `expect` carries the occupied bind address.
fn child(mode: &str, config: &std::path::Path, url: &str, expect: &str) {
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", NAME, "--ignored", "--nocapture"]).env_clear()
        .env(CHILD, mode).env("CONFIG", config).env("DATABASE_URL", url).env(EXPECT, expect)
        .status().unwrap();
    assert!(status.success(), "{mode} child failed");
}

/// Child entry: `crate::main` must fail; the failure shape is the assertion.
fn run_main(mode: &str) {
    let error = crate::main().expect_err("main must fail at or before the occupied final bind");
    let expect = std::env::var(EXPECT).unwrap();
    match mode {
        "bind" => {
            let io = error.downcast_ref::<std::io::Error>().unwrap_or_else(|| panic!("the final bind failure must be an io::Error: {error:?}"));
            assert_eq!((io.kind(), error.to_string().as_str()), (std::io::ErrorKind::AddrInUse, format!("binding {expect}").as_str()));
        }
        "enabled" => {
            assert!(error.downcast_ref::<std::io::Error>().is_none());
            assert_eq!(error.to_string(), "unknown authored content namespace sovereignty");
        }
        mode => panic!("unknown child mode {mode}"),
    }
}

fn write_config(root: &std::path::Path, bind: &str, enabled: bool) -> std::path::PathBuf {
    let path = root.join(if enabled { "enabled.toml" } else { "disabled.toml" });
    std::fs::write(&path, format!(
        "[server]\nbind = \"{bind}\"\n[database]\nschema_on_start = false\nschema_downgrade_guard = false\n\
         [chat]\nbind = \"127.0.0.1:0\"\ndomain = \"test\"\n[images]\nbind = \"127.0.0.1:0\"\ndir = \"{}\"\n\
         [gateway]\nbind = \"127.0.0.1:0\"\n[cdn]\nbind = \"127.0.0.1:0\"\n[lua]\nenabled = {enabled}\nscripts_dir = \"{}\"\n",
        root.join("images").display(), root.join("scripts").display())).unwrap();
    path
}

async fn run(owned: &mut Owned) {
    let database = crate::config::tests::test_database();
    let url = database.url.clone();
    let db = crate::db::Db::connect(&database).await.unwrap();
    let sde = crate::tests::shared_sde(&db).await;
    // The dedicated database must not carry foreign sovereignty rows or jobs.
    let clean: bool = db.query_one("SELECT NOT EXISTS (SELECT 1 FROM game.sov_systems) AND NOT EXISTS (SELECT 1 FROM game.sov_structures) \
        AND NOT EXISTS (SELECT 1 FROM game.sov_capture_targets) AND NOT EXISTS (SELECT 1 FROM game.sov_history) \
        AND NOT EXISTS (SELECT 1 FROM game.sov_link_sources) AND NOT EXISTS (SELECT 1 FROM game.sov_link_source_changes) \
        AND NOT EXISTS (SELECT 1 FROM game.sov_alliance_policy) AND NOT EXISTS (SELECT 1 FROM game.jobs) AS clean", &[]).await.unwrap().get("clean");
    assert!(clean, "dedicated database must be free of sovereignty rows and jobs");
    let id = format!("{}-{}", std::process::id(), crate::handshake::random_u64());
    owned.root = std::env::temp_dir().join(format!("startup-disabled-{id}"));
    std::fs::create_dir_all(owned.root.join("scripts")).unwrap();
    std::fs::create_dir_all(owned.root.join("images")).unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let bind = listener.local_addr().unwrap().to_string();
    let disabled = write_config(&owned.root, &bind, false);
    let enabled = write_config(&owned.root, &bind, true);
    owned.jobs = true;
    child("bind", &disabled, &url, &bind);

    // Publish one real retained sovereignty package and keep only its version.
    let version = format!("startup-disabled-{id}");
    let package = owned.root.join("package");
    std::fs::create_dir_all(package.join("content/sovereignty/data")).unwrap();
    std::fs::write(package.join("sovereignty_content.lua"), include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/lua/sovereignty_content.lua"))
        .replace("content_version = \"1\"", &format!("content_version = \"{version}\""))).unwrap();
    std::fs::write(package.join("content/sovereignty/policy.lua"), include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/lua/content/sovereignty/policy.lua"))).unwrap();
    std::fs::write(package.join("content/sovereignty/data/policy.json"), include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/lua/content/sovereignty/data/policy.json"))
        .replace("\"version\": \"1\"", &format!("\"version\": \"{version}\""))).unwrap();
    owned.version = Some(version.clone());
    let lua = crate::config::lua::LuaConfig { scripts_dir: package.to_string_lossy().into_owned(), ..Default::default() };
    let world = crate::space::World::new(db.clone(), sde.clone(), 1);
    let pilots = crate::lua::host::pilots::Pilots::new(&db, crate::chat::Hub::new("test", db.clone(), world.clone())).await.unwrap();
    let resources = crate::lua::host::Resources { db: db.clone(), sde: sde.clone(), world, pilots, store_value_limit_bytes: lua.store_value_limit_bytes };
    let host = crate::lua::reload::Host::new(lua.clone(), resources, crate::lua::catalogue::Catalogue::load(&db, &lua, &sde).await.unwrap()).await.unwrap();
    let checkpoint = i64::try_from(host.policies().resolve("sovereignty", &version).unwrap().checkpoint_interval_ms().unwrap()).unwrap();
    drop(host);

    // A self-consistent unclaimed Hub snapshot with an in-flight capture:
    // item, system, structure and capture rows shaped like a fresh deployment.
    owned.fixture = Some(crate::services::tests::fixture::create(&db).await.unwrap());
    let fixture = owned.fixture.unwrap();
    let corporation: i64 = db.query_one("SELECT corporation_id FROM game.characters WHERE character_id = $1", &[&fixture.character]).await.unwrap().get("corporation_id");
    let system = sde.map.systems().find(|s| s.security.is_some_and(|security| security <= 0.0))
        .map(|s| s.solar_system_id).expect("dedicated SDE must contain a sovereign system");
    assert!(crate::dao::sovereignty::systems::read(&db, system).await.unwrap().is_none());
    owned.system = Some(system);
    let definition = sde.sovereignty.hub();
    let (type_id, owner, capture_time, regen) = (definition.type_id, definition.deploy.default_owner, definition.capture.capture_time_seconds, definition.capture.defensive_regen_rate);
    let now = crate::client::filetime_now();
    let tx_version = version.clone();
    let (item, revision) = db.transaction(move |tx, _: &mut ()| {
        let version = tx_version.clone();
        Box::pin(async move {
            let item = crate::dao::items::creation::positioned(tx, type_id, owner, system, [0.0; 3]).await?;
            let expected = crate::dao::sovereignty::systems::lock(tx, system).await?;
            let updated = crate::dao::sovereignty::systems::replace(tx, &expected, &crate::dao::sovereignty::systems::SystemChange {
                hub_item_id: Some(item), claim_corporation_id: None, alliance_id: None, claim_time: None, ownership_epoch: expected.ownership_epoch,
            }).await?.expect("the empty sovereignty system must update");
            assert!(crate::dao::sovereignty::structures::insert(tx, &crate::dao::sovereignty::structures::StructureRow {
                item_id: item, system_id: system, source_item_id: item, deployer_character_id: fixture.character,
                deployer_corporation_id: corporation, deployed_at: now, content_namespace: "sovereignty".into(),
                content_version: version.clone(), self_destruct_due_at: None, self_destruct_actor: None,
            }).await?);
            assert!(crate::dao::sovereignty::capture::insert(tx, &crate::dao::sovereignty::capture::CaptureRow {
                target_item_id: item, system_id: system, ownership_epoch: updated.ownership_epoch, controlling_team: None,
                scoring_team: Some(corporation), defender_team: OWNER_UNOWNED, score: 0.5, rate: 1.0, blocked: false,
                score_sim_time: now, checkpoint_wall_time: now, revision: 0, capture_time_seconds: capture_time,
                defensive_regen_rate: regen, checkpoint_interval_ms: checkpoint, content_namespace: "sovereignty".into(), content_version: version,
            }).await?);
            Ok::<(i64, i64), crate::db::Error>((item, updated.revision))
        })
    }, |value, ()| async move { Ok::<_, crate::db::Error>(value) }).await.unwrap();
    owned.item = Some(item);
    child("bind", &disabled, &url, &bind);
    let hubs = crate::dao::sovereignty::structures::snapshots(&db, Some(system), None).await.unwrap();
    let [hub] = hubs.as_slice() else { panic!("the persisted Hub must survive disabled startup recovery") };
    assert_eq!((hub.system.hub_item_id, hub.system.ownership_epoch, hub.system.revision), (Some(item), 0, revision + 1));
    assert_eq!((hub.system.claim_corporation_id, hub.system.alliance_id, hub.system.claim_time), (None, None, None));
    assert_eq!((hub.structure.content_namespace.as_str(), hub.structure.content_version.as_str()), ("sovereignty", version.as_str()));
    assert_eq!((hub.structure.deployed_at, hub.structure.self_destruct_due_at, hub.structure.self_destruct_actor), (now, None, None));
    let target = crate::dao::sovereignty::capture::read(&db, item).await.unwrap().unwrap();
    assert_eq!((target.revision, target.ownership_epoch, target.controlling_team), (1, 0, None));
    assert_eq!((target.scoring_team, target.rate, target.blocked, target.score, target.defender_team), (None, 0.0, false, 0.5, OWNER_UNOWNED));
    assert_eq!(target.score_sim_time, target.checkpoint_wall_time);
    assert!(target.score_sim_time >= now);

    // Enabled with a real but empty script root keeps the current-policy refusal.
    child("enabled", &enabled, &url, "");
    drop(listener);
}

/// Remove exactly this run's rows, package version and seeded jobs.
async fn cleanup(owned: &Owned) {
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    if let Some(item) = owned.item {
        db.execute("DELETE FROM game.sov_capture_targets WHERE target_item_id = $1", &[&item]).await.unwrap();
        db.execute("DELETE FROM game.sov_structures WHERE item_id = $1", &[&item]).await.unwrap();
        db.execute("DELETE FROM game.items WHERE item_id = $1", &[&item]).await.unwrap();
    }
    if let Some(system) = owned.system {
        db.execute("DELETE FROM game.sov_systems WHERE system_id = $1", &[&system]).await.unwrap();
    }
    if let Some(version) = &owned.version {
        db.execute("DELETE FROM game.lua_content_activations WHERE namespace = 'sovereignty' AND version = $1", &[version]).await.unwrap();
        db.execute("DELETE FROM game.lua_content_versions WHERE namespace = 'sovereignty' AND version = $1", &[version]).await.unwrap();
    }
    if owned.jobs {
        db.execute("DELETE FROM game.jobs WHERE job_id = ANY($1)", &[&&SEEDED_JOBS[..]]).await.unwrap();
    }
    if let Some(fixture) = owned.fixture {
        crate::services::tests::fixture::cleanup(&db, fixture).await.unwrap();
    }
    let _ = std::fs::remove_dir_all(&owned.root);
}

#[test]
#[ignore = "requires a dedicated initialized INFINITY_TEST_DATABASE_URL with SDE and the external server lock; run this exact test in its own process"]
fn disabled_lua_completes_startup_with_retained_hubs() {
    if let Some(mode) = std::env::var_os(CHILD) {
        run_main(mode.to_str().unwrap());
        return;
    }
    crate::tests::init_config();
    let mut owned = Owned::default();
    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| runtime.block_on(run(&mut owned))));
    drop(runtime);
    let cleanup_runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let cleanup = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| cleanup_runtime.block_on(cleanup(&owned))));
    if let Err(error) = result { std::panic::resume_unwind(error); }
    if let Err(error) = cleanup { std::panic::resume_unwind(error); }
}
