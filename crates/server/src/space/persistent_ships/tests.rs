use super::*;
use crate::space::{MODE_STOP, WarpState};

fn hull(id: i64) -> Ball {
    Ball {
        id, type_id: 1, owner_id: 1, corp_id: 1, alliance_id: 0, char_id: None,
        signature_radius: None, mode: MODE_STOP, radius: 1.0, mass: 1.0,
        max_vel: 0.0, agility: 1.0, speed_fraction: 0.0, pos: [0.0; 3], vel: [0.0; 3],
        old_pos: [0.0; 3], old_vel: [0.0; 3], segment_acceleration: None,
        goto: [0.0; 3], follow_id: 0, follow_range: 0.0, free: true, global: false,
        massive: true, interactive: false, warp: WarpState::default(), 
        last_ms: 0, interpolation_anchor_ms: 0, effect_stamp: 0, cloak: 0, cloak_until: 0,
        health: [1.0; 3], launcher: 0, slim: Vec::new(),
    }
}

#[test]
fn free_hull_membership_is_idempotent_and_replacement_changes_generation() {
    crate::tests::init_config();
    let mut system = System::default();
    system.add_persistent_ship(hull(1));
    let first = system.persistent_generation(1).unwrap();
    assert_eq!(system.persistent_balls().count(), 1);
    assert_eq!(system.pilots().count(), 0);
    assert!(!system.ball(1).unwrap().interactive);
    assert!(system.snapshot_due > 0);
    system.add_persistent_ship(hull(1));
    assert_eq!(system.persistent_generation(1), Some(first));
    assert_eq!(system.persistent_balls().count(), 1);
    system.take(1);
    system.add_persistent_ship(hull(1));
    assert_ne!(system.persistent_generation(1), Some(first));
}
