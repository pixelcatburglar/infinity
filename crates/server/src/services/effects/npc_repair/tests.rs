use super::*;

#[test]
fn selects_only_damaged_targets_with_nonzero_range_strength() {
    assert_eq!(score(Some(0.25), 10.0, 10.0, 0.0), Some(0.25));
    assert_eq!(score(Some(1.0), 10.0, 10.0, 0.0), None);
    assert_eq!(score(None, 0.0, 10.0, 0.0), None);
    assert_eq!(score(Some(f64::NAN), 0.0, 10.0, 0.0), None);
    assert_eq!(score(Some(0.25), 11.0, 10.0, 0.0), None);
    assert_eq!(score(Some(0.25), 30.0, 10.0, 20.0), Some(0.25));
}

#[test]
fn npc_repair_variants_exclude_unimplemented_families() {
    assert!(supported(i64::from(EFFECT_NPC_REMOTE_ARMOR_REPAIRER)));
    assert!(supported(i64::from(EFFECT_NPC_BEHAVIOR_REMOTE_SHIELD)));
    assert!(supported(i64::from(infinity_core::consts::dogma::EFFECT_NPC_REMOTE_HULL_REPAIRER)));
    assert!(!supported(i64::from(infinity_core::consts::dogma::EFFECT_ANCILLARY_REMOTE_ARMOR_REPAIRER)));
    assert!(!supported(i64::from(infinity_core::consts::dogma::EFFECT_MUTADAPTIVE_REMOTE_ARMOR_REPAIRER)));
}

#[test]
fn remote_repair_phase_and_range_policy() {
    for (layer, start) in [(0, true), (1, false), (2, false)] {
        assert_eq!(repair::applies_at_start(layer), start);
        assert_eq!(repair::scaled_amount(100.0, 10.0, 10.0, 20.0), 100.0);
        assert_eq!(repair::scaled_amount(100.0, 30.0, 10.0, 20.0), 50.0);
        assert_eq!(repair::scaled_amount(100.0, 11.0, 10.0, 0.0), 0.0);
    }
}

mod db_smoke;
