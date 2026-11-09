use super::*;

fn row(ordinal: i64, version: &str, published_at: i64) -> ActivationRow {
    ActivationRow { ordinal, namespace: DAILY_GOALS_NAMESPACE.into(), version: version.into(), fingerprint: vec![ordinal as u8; 32], published_at }
}
fn version(rows: &[ActivationRow], start: i64, end: i64) -> Option<&str> {
    select(rows, start, end).unwrap().map(|row| row.version.as_str())
}

#[test]
fn first_enable_and_next_period_boundaries() {
    let rows = vec![row(1, "a", 15), row(2, "b", 25)];
    assert_eq!(version(&rows, 0, 10), None);
    assert_eq!(version(&rows, 10, 20), Some("a"));
    assert_eq!(version(&rows, 20, 30), Some("a"));
    assert_eq!(version(&rows, 30, 40), Some("b"));
    assert_eq!(version(&rows, 25, 30), Some("a"));
    assert_eq!(version(&rows, 0, 15), None);
    assert_eq!(version(&rows, -10, 0), None);
}

#[test]
fn monthly_and_rollback_and_same_day_publications() {
    let rows = vec![row(1, "a", 15), row(2, "b", 25), row(3, "a", 26)];
    assert_eq!(version(&rows, 20, 30), Some("a"));
    assert_eq!(select(&rows, 30, 40).unwrap().unwrap().ordinal, 3);
    assert_eq!(select(&rows, 10, 40).unwrap().unwrap().ordinal, 1);
    assert_eq!(select(&rows, 40, 70).unwrap().unwrap().ordinal, 3);
    let reconstructed = rows.clone();
    assert_eq!(select(&rows, 30, 40).unwrap(), select(&reconstructed, 30, 40).unwrap());
}

#[test]
fn invalid_log_and_interval_never_produce_selection() {
    let base = vec![row(1, "a", 15), row(2, "b", 25)];
    assert!(select(&base, 20, 20).is_err());
    assert!(select(&base, 21, 20).is_err());
    for mutation in 0..6 {
        let mut rows = base.clone();
        match mutation {
            0 => rows[1].ordinal = 1,
            1 => rows[0].ordinal = 0,
            2 => rows[1].namespace = "other".into(),
            3 => rows[1].fingerprint.pop().map(|_| ()).unwrap(),
            4 => rows[1].published_at = 0,
            _ => rows[1].version.clear(),
        }
        assert!(select(&rows, 10, 20).is_err());
    }
}

#[tokio::test]
#[ignore = "requires isolated test process, dedicated INFINITY_TEST_DATABASE_URL with SDE, and external flock"]
async fn real_host_reuses_retained_pin_for_day_and_month() {
    const CHILD: &str = "INFINITY_DAILY_ACTIVATION_PIN_TEST_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "services::daily_goals::activation::tests::real_host_reuses_retained_pin_for_day_and_month", "--ignored", "--nocapture"])
            .env(CHILD, "1").status().unwrap();
        assert!(status.success());
        return;
    }
    use std::sync::Arc;
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.expect("daily activation requires real SDE"));
    let world = crate::space::World::new(db.clone(), sde.clone(), 1);
    let hub = crate::chat::Hub::new("test", db.clone(), world.clone());
    let id = crate::handshake::random_u64();
    let version = format!("activation-pin-test-{id}");
    let root = std::env::temp_dir().join(format!("daily-activation-pin-{id}"));
    std::fs::create_dir_all(root.join("content/daily_goals/data")).unwrap();
    let registration = include_str!("../../../../lua/daily_goals.lua").replace("content_version = \"1\"", &format!("content_version = \"{version}\""));
    let data = include_str!("../../../../lua/content/daily_goals/data/daily_goals.json").replacen("\"version\": \"1\"", &format!("\"version\": \"{version}\""), 1);
    std::fs::write(root.join("daily_goals.lua"), registration).unwrap();
    std::fs::write(root.join("content/daily_goals/policy.lua"), include_bytes!("../../../../lua/content/daily_goals/policy.lua")).unwrap();
    std::fs::write(root.join("content/daily_goals/data/daily_goals.json"), data).unwrap();
    let mut config = crate::config::Config::default();
    config.lua.enabled = true;
    config.lua.scripts_dir = root.to_string_lossy().into_owned();
    crate::lua::start(&config, &db, &sde, &world, &hub).await.unwrap();
    let resources = Resources::from_world(&world).unwrap();
    let now = crate::client::filetime_now();
    let bounds = crate::dao::daily_goals::periods::bounds(&db, now).await.unwrap();
    let next = crate::dao::daily_goals::periods::bounds(&db, bounds.month_end + 1).await.unwrap();
    let mut history = load(&resources).await.unwrap();
    let day = resolve(&resources, &mut history, "day", next.day_start, next.day_end).await.unwrap().unwrap();
    let month = resolve(&resources, &mut history, "month", next.month_start, next.month_end).await.unwrap().unwrap();
    assert_eq!(day.policy().version(), version);
    assert_eq!(month.policy().version(), version);
    assert!(day.policy().same_package(month.policy()));
    let mut separate = load(&resources).await.unwrap();
    let reopened = resolve(&resources, &mut separate, "day", next.day_start, next.day_end).await.unwrap().unwrap();
    assert!(!day.policy().same_package(reopened.policy()));
    let selected = history.rows.iter_mut().find(|row| row.version == version).unwrap();
    selected.fingerprint[0] ^= 1;
    assert!(resolve(&resources, &mut history, "month", next.month_start, next.month_end).await.is_err());
    std::fs::remove_dir_all(root).unwrap();
}
