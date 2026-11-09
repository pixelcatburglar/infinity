use super::*;
use crate::space::{Ball, WarpState, MODE_STOP};

fn ball(id: i64, x: f64, pilot: Option<i64>) -> Ball {
    Ball {
        id, type_id: 1, owner_id: 1, corp_id: 1, alliance_id: -1,
        char_id: pilot, signature_radius: None, mode: MODE_STOP, radius: 1.0, mass: 1.0,
        max_vel: 1.0, agility: 1.0, speed_fraction: 0.0,
        pos: [x, 0.0, 0.0], vel: [0.0; 3], old_pos: [x, 0.0, 0.0], old_vel: [0.0; 3], segment_acceleration: None,
        goto: [0.0; 3], follow_id: 0, follow_range: 0.0, free: false, global: false,
        interactive: false, massive: true, warp: WarpState::default(), 
        last_ms: 0, interpolation_anchor_ms: 0, effect_stamp: 0, cloak: 0, cloak_until: 0,
        health: [1.0; 3], launcher: 0, slim: Vec::new(),
    }
}

#[test]
fn radius_includes_boundary_and_own_unpiloted_balls_but_not_source() {
    crate::tests::init_config();
    let mut system = System::new(1, Arc::default(), 0);
    for b in [ball(1, 0.0, Some(1)), ball(3, 10.0, None), ball(2, 5.0, Some(2)), ball(4, 10.1, None)] { system.insert(b); }
    let pilot_generation = system.combat_membership_generation(2);
    let unpiloted_generation = system.combat_membership_generation(3);
    assert_eq!((pilot_generation, unpiloted_generation), (None, None));
    assert_eq!(victims(&system, 1, [0.0; 3], 10.0), vec![(2, Some(2), pilot_generation, false, None), (3, None, unpiloted_generation, false, None)]);
    assert!(victims(&system, 1, [0.0; 3], f64::NAN).is_empty());
}

mod npc;
