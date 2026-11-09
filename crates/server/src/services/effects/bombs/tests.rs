use super::*;
use crate::space::{System, WarpState};
use std::sync::Mutex;

fn ball(id: i64, x: f64) -> Ball {
    Ball {
        id, type_id: 1, owner_id: 1, corp_id: 1, alliance_id: -1, char_id: None,
        signature_radius: None, mode: MODE_STOP, radius: 1.0, mass: 1.0,
        max_vel: 100.0, agility: 1.0, speed_fraction: 0.0,
        pos: [x, 0.0, 0.0], vel: [0.0; 3], old_pos: [x, 0.0, 0.0], old_vel: [0.0; 3], segment_acceleration: None,
        goto: [x, 0.0, 0.0], follow_id: 0, follow_range: 0.0, free: true, global: false,
        massive: false, interactive: false, warp: WarpState::default(), 
        last_ms: 0, interpolation_anchor_ms: 0, effect_stamp: 0, cloak: 0, cloak_until: 0,
        health: [1.0; 3], launcher: 0, slim: Vec::new(),
    }
}

#[test]
fn bomb_flies_straight_and_excludes_itself_not_the_launcher() {
    crate::tests::init_config();
    let mut system = System::new(1, Arc::default(), 0);
    system.insert(ball(1, 0.0));
    let launcher_generation = system.combat_membership_generation(1);
    assert_eq!(launcher_generation, None);
    let observed = Arc::new(Mutex::new(None));
    let result = observed.clone();
    system.launch_bomb(ball(2, 0.0), [1.0, 0.0, 0.0], 1000, move |s| {
        let center = s.sample_position(2).unwrap();
        *result.lock().unwrap() = Some((center, aoe::victims(s, 2, center, 1000.0)));
    });
    system.tick(1000);
    system.tick(2000);
    system.tick(3000);
    let (center, victims) = observed.lock().unwrap().take().expect("detonation timer");
    assert!(center[0] > 0.0);
    assert_eq!(victims, vec![(1, None, launcher_generation, false, None)]);
}

#[test]
fn removed_bomb_has_no_detonation_victims() {
    crate::tests::init_config();
    let mut system = System::new(1, Arc::default(), 0);
    system.insert(ball(1, 0.0));
    let observed = Arc::new(Mutex::new(None));
    let result = observed.clone();
    system.launch_bomb(ball(2, 0.0), [1.0, 0.0, 0.0], 1000, move |s| {
        if let Some(center) = s.sample_position(2) {
            *result.lock().unwrap() = Some(aoe::victims(s, 2, center, 1000.0));
        }
    });
    system.tick(1000);
    system.remove_ball(2);
    system.tick(2000);
    system.tick(3000);
    assert!(observed.lock().unwrap().is_none());
}
