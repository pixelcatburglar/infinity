//! `combat.npc_wreck_id_block_size`/`npc_wreck_id_retry_ms` loading and validation.

use super::*;

#[test]
fn npc_wreck_id_block_defaults_file_env_and_bounds() {
    let fixture = Fixture::new();
    let pair = |config: &Config| (config.combat.npc_wreck_id_block_size, config.combat.npc_wreck_id_retry_ms);
    assert_eq!(pair(&Config::load_with(&fixture.path(), |_| None).unwrap()), (4096, 1000));
    fixture.write("[combat]\nnpc_wreck_id_block_size = 64\nnpc_wreck_id_retry_ms = 250");
    assert_eq!(pair(&Config::load_with(&fixture.path(), |_| None).unwrap()), (64, 250));
    let env = |key: &str| match key {
        "COMBAT_NPC_WRECK_ID_BLOCK_SIZE" => Some("777".to_string()),
        "COMBAT_NPC_WRECK_ID_RETRY_MS" => Some("125".to_string()),
        _ => None,
    };
    let config = Config::load_with(&fixture.path(), env).unwrap();
    assert_eq!(pair(&config), (777, 125));
    assert_eq!(config.game_config().combat.npc_wreck_id_block_size, 777);
    // One natural generation needs the root plus the maximum two loot stacks.
    for bad in [0, -1, 1, 2, i64::MAX / 4 + 1] {
        assert!(load_error(&format!("[combat]\nnpc_wreck_id_block_size = {bad}")).contains("npc_wreck_id_block_size"), "block {bad}");
    }
    for bad in [0, -5, i64::MAX] {
        assert!(load_error(&format!("[combat]\nnpc_wreck_id_retry_ms = {bad}")).contains("npc_wreck_id_retry_ms"), "retry {bad}");
    }
}
