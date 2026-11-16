use super::*;

#[test]
fn loss_tolerant_async_commit_defaults_file_and_environment() {
    let fixture = Fixture::new();
    assert!(Config::load_with(&fixture.path(), |_| None).unwrap().database.loss_tolerant_async_commit);
    fixture.write("[database]\nloss_tolerant_async_commit = false");
    assert!(!Config::load_with(&fixture.path(), |_| None).unwrap().database.loss_tolerant_async_commit);
    let env = |key: &str| (key == "DATABASE_LOSS_TOLERANT_ASYNC_COMMIT").then(|| "true".into());
    assert!(Config::load_with(&fixture.path(), env).unwrap().database.loss_tolerant_async_commit);
    let bad = |key: &str| (key == "DATABASE_LOSS_TOLERANT_ASYNC_COMMIT").then(|| "yes".into());
    assert!(Config::load_with(&fixture.path(), bad).is_err());
    assert!(load_error("[database]\nloss_tolerant_async_commit = 5").contains("loss_tolerant_async_commit"));
}
