//! Admission policy TOML mapping and validation boundaries.
#[test]
fn admission_toml_mapping_and_bounds() {
    for (threshold, seconds) in [(0, 60), (1, 1), (100, 86400), (101, 60), (10, 0), (10, 86401)] {
        let config: super::super::Config = toml::from_str(&format!("[server]\nlogin_failure_threshold={threshold}\nlogin_failure_window_secs={seconds}\nlogin_failure_cooldown_secs={seconds}")).unwrap();
        let server = &config.server;
        assert_eq!((server.login_failure_threshold, server.login_failure_window_secs, server.login_failure_cooldown_secs), (threshold, seconds, seconds));
        assert_eq!(config.validate().is_ok(), (1..=100).contains(&threshold) && (1..=86400).contains(&seconds));
    }
}

#[test]
fn shutdown_reject_secs_default_toml_and_bounds() {
    assert_eq!(super::ServerConfig::default().shutdown_reject_secs, 120);
    for (seconds, valid) in [(1, true), (86400, true), (0, false), (86401, false)] {
        let config: super::super::Config = toml::from_str(&format!("[server]\nshutdown_reject_secs={seconds}")).unwrap();
        assert_eq!((config.server.shutdown_reject_secs, config.validate().is_ok()), (seconds, valid));
    }
}
