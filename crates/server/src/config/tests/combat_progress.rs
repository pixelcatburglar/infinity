use super::*;

#[cfg(feature = "tests-runtime")]
#[test]
fn combat_progress_capacity_defaults_toml_and_zero_rejection() {
    let fixture = Fixture::new();
    let defaults = Config::load_with(&fixture.path(), |_| None).unwrap();
    assert_eq!((defaults.combat.progress_key_capacity, defaults.combat.progress_pending_batches), (4096, 4));
    fixture.write("[combat]\nprogress_key_capacity = 17\nprogress_pending_batches = 2");
    let config = Config::load_with(&fixture.path(), |_| None).unwrap();
    assert_eq!((config.combat.progress_key_capacity, config.combat.progress_pending_batches), (17, 2));
    let env = Config::load_with(&fixture.path(), |key| match key {
        "COMBAT_PROGRESS_KEY_CAPACITY" => Some("23".into()),
        "COMBAT_PROGRESS_PENDING_BATCHES" => Some("3".into()),
        _ => None,
    }).unwrap();
    assert_eq!((env.combat.progress_key_capacity, env.combat.progress_pending_batches), (23, 3));
    for key in ["COMBAT_PROGRESS_KEY_CAPACITY", "COMBAT_PROGRESS_PENDING_BATCHES"] {
        assert!(Config::load_with(&fixture.path(), |name| (name == key).then(|| "0".into())).is_err());
    }
    for key in ["progress_key_capacity", "progress_pending_batches"] {
        assert!(load_error(&format!("[combat]\n{key} = 0")).contains(&format!("combat.{key}")));
    }
}
