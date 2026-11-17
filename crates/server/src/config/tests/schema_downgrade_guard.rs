use super::*;

/// Dedicated test-database configuration shared by every domain: the test URL
/// with the downgrade guard off, so connects never refuse on recorded history.
pub(super) fn database_config() -> deployment::DatabaseConfig {
    deployment::DatabaseConfig {
        url: std::env::var("INFINITY_TEST_DATABASE_URL").expect("INFINITY_TEST_DATABASE_URL must name a dedicated test database"),
        schema_downgrade_guard: false,
        ..Default::default()
    }
}

#[cfg(feature = "tests-runtime")]
#[test]
fn defaults_file_and_environment() {
    let fixture = Fixture::new();
    assert!(Config::load_with(&fixture.path(), |_| None).unwrap().database.schema_downgrade_guard);
    fixture.write("[database]\nschema_downgrade_guard = false");
    assert!(!Config::load_with(&fixture.path(), |_| None).unwrap().database.schema_downgrade_guard);
    fixture.write("[database]\nschema_downgrade_guard = true");
    let config = Config::load_with(&fixture.path(), |key| (key == "DATABASE_SCHEMA_DOWNGRADE_GUARD").then(|| "false".into())).unwrap();
    assert!(!config.database.schema_downgrade_guard);
    assert!(load_error("[database]\nschema_downgrade_guard = 5").contains("schema_downgrade_guard"));
}
