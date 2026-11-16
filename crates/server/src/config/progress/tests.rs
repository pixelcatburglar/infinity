//! Progress retry policy defaults, TOML mapping and bound validation.

use super::*;

#[test]
fn permanent_retry_limit_defaults_toml_env_and_bounds() {
    assert_eq!(ProgressConfig::default().permanent_retry_limit, 5);
    let root: crate::config::Config = toml::from_str("[progress]\npermanent_retry_limit = 9").unwrap();
    assert_eq!(root.game_config().progress.permanent_retry_limit, 9);
    for value in [1, 100] {
        assert!(toml::from_str::<ProgressConfig>(&format!("permanent_retry_limit = {value}")).unwrap().validate().is_ok(), "{value}");
    }
    for value in [0, -1, 101] {
        assert!(toml::from_str::<ProgressConfig>(&format!("permanent_retry_limit = {value}")).unwrap().validate().is_err(), "{value}");
    }
    let path = std::env::temp_dir().join(format!("infinity-progress-config-{}", std::process::id()));
    let env = |key: &str| (key == "PROGRESS_PERMANENT_RETRY_LIMIT").then(|| "7".into());
    assert_eq!(crate::config::Config::load_with(&path, env).unwrap().game_config().progress.permanent_retry_limit, 7);
    assert!(crate::config::Config::load_with(&path, |key| (key == "PROGRESS_PERMANENT_RETRY_LIMIT").then(|| "0".into())).is_err());
}
