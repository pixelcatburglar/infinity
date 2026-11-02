use super::*;

#[test]
fn name_shape_checks_configured_boundaries_and_spacing() {
    crate::tests::init_config();
    let policy = &crate::config::game().characters;
    assert_eq!(name_shape_code(&"a".repeat((policy.name_min_len - 1) as usize)), NAME_TOO_SHORT);
    assert_eq!(name_shape_code(&"a".repeat(policy.name_min_len as usize)), NAME_VALID);
    assert_eq!(name_shape_code(&"a".repeat((policy.name_max_len + 1) as usize)), NAME_TOO_LONG);
    assert_eq!(name_shape_code("A  B"), NAME_CONSECUTIVE_SPACES);
    assert_eq!(name_shape_code(&format!("A{}", " B".repeat((policy.name_max_spaces + 1) as usize))), NAME_TOO_MANY_SPACES);
}
