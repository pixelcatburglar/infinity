use super::*;
use crate::space::{Ball, WarpState, MODE_STOP};

fn ball(id: i64, x: f64) -> Ball {
    Ball { id, type_id: 1, owner_id: 1, corp_id: 1, alliance_id: -1, char_id: None, signature_radius: None,
        mode: MODE_STOP, radius: 1.0, mass: 1.0, max_vel: 1.0, agility: 1.0, speed_fraction: 0.0,
        pos: [x, 0.0, 0.0], vel: [0.0; 3], goto: [0.0; 3], follow_id: 0,
        old_pos: [x, 0.0, 0.0], old_vel: [0.0; 3], segment_acceleration: None,
        follow_range: 0.0, free: true, global: false, massive: false, interactive: false,
        warp: WarpState::default(), last_ms: 0, interpolation_anchor_ms: 0,
        effect_stamp: 0, cloak: 0, cloak_until: 0, health: [1.0; 3], launcher: 0, slim: Vec::new() }
}

#[test]
fn tractor_accepts_only_wrecks_and_supported_container_groups() {
    assert!(container_group(i64::from(GROUP_WRECK)));
    for group in CARGO_CONTAINER_GROUPS { assert!(container_group(i64::from(group))); }
    assert!(!container_group(i64::MAX));
}

#[test]
fn tractor_range_uses_surface_gap_and_rejects_missing_and_invalid_targets() {
    let mut s = System::new(0, Arc::default(), 0);
    s.insert(ball(1, 0.0));
    s.insert(ball(2, 12.0));
    assert!(in_range(&s, 1, 2, 10.0));
    assert!(!in_range(&s, 1, 2, 9.0));
    assert!(!in_range(&s, 1, 1, 10.0));
    assert!(!in_range(&s, 1, 3, 10.0));
    assert!(!in_range(&s, 1, 2, f64::NAN));
}
