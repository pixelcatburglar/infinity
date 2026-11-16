//! `space.loaded_snapshot_failure_periods` loading, domain validation and snapshot tests.

use super::*;

#[test]
fn loaded_snapshot_failure_periods_defaults_overrides_bounds_and_snapshot() {
    let fixture = Fixture::new();
    assert_eq!(Config::load_with(&fixture.path(), |_| None).unwrap().space.loaded_snapshot_failure_periods, 3);
    fixture.write(&format!("[space]\nloaded_snapshot_failure_periods = {}", i64::MAX));
    let config = Config::load_with(&fixture.path(), |_| None).unwrap();
    assert_eq!(config.space.loaded_snapshot_failure_periods, i64::MAX);
    assert_eq!(config.game_config().space.loaded_snapshot_failure_periods, i64::MAX);
    let env = |key: &str| (key == "SPACE_LOADED_SNAPSHOT_FAILURE_PERIODS").then(|| "5".to_owned());
    assert_eq!(Config::load_with(&fixture.path(), env).unwrap().space.loaded_snapshot_failure_periods, 5);
    for value in [0, -1] {
        assert!(load_error(&format!("[space]\nloaded_snapshot_failure_periods = {value}")).contains("space.loaded_snapshot_failure_periods"));
    }
}
