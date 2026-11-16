//! `space.idle_unload_secs` loading, domain validation and snapshot tests.

use super::*;

#[test]
fn idle_unload_secs_defaults_overrides_bounds_and_snapshot() {
    let fixture = Fixture::new();
    assert_eq!(Config::load_with(&fixture.path(), |_| None).unwrap().space.idle_unload_secs, 1800);
    fixture.write("[space]\nidle_unload_secs = 0");
    let disabled = Config::load_with(&fixture.path(), |_| None).unwrap();
    assert_eq!(disabled.game_config().space.idle_unload_secs, 0);
    for value in [60, i64::MAX] {
        fixture.write(&format!("[space]\nidle_unload_secs = {value}"));
        let config = Config::load_with(&fixture.path(), |_| None).unwrap();
        assert_eq!(config.space.idle_unload_secs, value);
        assert_eq!(config.game_config().space.idle_unload_secs, value);
    }
    let env = |key: &str| (key == "SPACE_IDLE_UNLOAD_SECS").then(|| "120".to_owned());
    assert_eq!(Config::load_with(&fixture.path(), env).unwrap().space.idle_unload_secs, 120);
    for value in [-1, 59] {
        assert!(load_error(&format!("[space]\nidle_unload_secs = {value}")).contains("space.idle_unload_secs"));
    }
    let invalid = |key: &str| (key == "SPACE_IDLE_UNLOAD_SECS").then(|| "no".to_owned());
    assert!(Config::load_with(&fixture.path(), invalid).err().unwrap().to_string().contains("SPACE_IDLE_UNLOAD_SECS"));
}
