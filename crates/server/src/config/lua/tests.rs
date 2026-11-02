use super::*;

#[test]
fn defaults_and_script_settings_deserialize() {
    let config: LuaConfig = toml::from_str("[scripts.motd]\nmessage = 'hello'\n").unwrap();
    assert!(config.enabled);
    assert_eq!(config.scripts_dir, "crates/server/lua");
    assert!(config.mod_dirs.is_empty());
    assert_eq!(config.memory_limit_mb, 64);
    assert_eq!(config.call_budget_ms, 50);
    assert_eq!(config.call_budget_instructions, 5_000_000);
    assert_eq!(config.instruction_hook_interval, 1000);
    assert_eq!(config.scripts["motd"]["message"].as_str(), Some("hello"));
    config.validate().unwrap();
    assert!(toml::from_str::<LuaConfig>("unexpected = 1").is_err());
}

#[test]
fn zero_and_overflowing_limits_are_rejected() {
    for contents in [
        "memory_limit_mb = 0", "call_budget_ms = 0", "call_budget_instructions = 0",
        "instruction_hook_interval = 0", "scripts_dir = ''", "mod_dirs = ['']",
        "memory_limit_mb = 9223372036854775807",
    ] {
        let config: LuaConfig = toml::from_str(contents).unwrap();
        assert!(config.validate().is_err(), "{contents}");
    }
}
