//! Actor-level superweapon frozen-geometry hit-set regression.
use super::*;
use crate::space::system::super_weapons::{Aim, FiringSpec};
use std::sync::atomic::AtomicBool;

fn spec(ship: i64, aim: Aim) -> FiringSpec {
    FiringSpec { ship, character: 42, master: 1, weapon_type: 1, effect: 1,
        generation: Arc::new(AtomicBool::new(false)), combat_generation: 0, sequence: 1, aim,
        hit_delay_ms: 0, window_ms: 0, cycle_ms: 1.0, radius: 100.0, range: 1_000.0,
        signature_radius: 400.0, damage: [100.0; 4], immobility_ms: 0, no_jump_or_cloak_ms: 0,
        single: true, illegal: false }
}

fn victim_ids(s: &System, spec: &FiringSpec) -> Vec<i64> {
    let mut ids: Vec<i64> = s.super_weapon_victims(spec).into_iter().map(|v| v.0).collect();
    ids.sort_unstable();
    ids
}

#[test]
fn cylinder_hit_set_uses_segment_distance_and_excludes_source() {
    let mut s = System::new(1, Arc::default(), 1_000);
    let spec = spec(1, Aim::Cylinder { origin: [0.0; 3], direction: [1.0, 0.0, 0.0] });
    s.insert(ball(1, Some(42), 0.0));
    for (id, pos) in [(2, [500.0, 99.0, 0.0]), (3, [500.0, 100.0, 0.0]), (4, [500.0, 100.01, 0.0]),
        (5, [1_000.0, 0.0, 0.0]), (6, [1_101.0, 0.0, 0.0]), (7, [-99.0, 0.0, 0.0]), (8, [-100.01, 0.0, 0.0])] {
        let mut b = ball(id, None, pos[0]);
        b.pos = pos;
        s.insert(b);
    }
    assert_eq!(victim_ids(&s, &spec), vec![2, 3, 5, 7]);
    // A ball entering the frozen shape after evaluation is still hit.
    s.insert(ball(9, None, 400.0));
    assert_eq!(victim_ids(&s, &spec), vec![2, 3, 5, 7, 9]);
}

#[test]
fn cone_hit_set_enforces_reach_and_proxy_half_angle() {
    let mut s = System::new(1, Arc::default(), 1_000);
    let cos_half = 15.0_f64.to_radians().cos();
    let spec = spec(1, Aim::Cone { origin: [0.0; 3], direction: [1.0, 0.0, 0.0], cos_half });
    s.insert(ball(1, Some(42), 0.0));
    let at = |s: &mut System, id: i64, theta: f64, range: f64| {
        let t = theta.to_radians();
        let mut b = ball(id, None, range * t.cos());
        b.pos = [range * t.cos(), range * t.sin(), 0.0];
        s.insert(b);
    };
    at(&mut s, 2, 14.0, 500.0);
    at(&mut s, 3, 16.0, 500.0);
    at(&mut s, 4, 0.0, 1_000.0);
    at(&mut s, 5, 0.0, 1_000.01);
    at(&mut s, 6, 170.0, 500.0);
    assert_eq!(victim_ids(&s, &spec), vec![2, 4]);
}

#[test]
fn slash_hit_set_covers_the_swept_fan_and_edge_cylinders() {
    let mut s = System::new(1, Arc::default(), 1_000);
    let spec = spec(1, Aim::Slash {
        origin: [0.0; 3], d0: [1.0, 0.0, 0.0], d1: [0.0, 1.0, 0.0], axis: [0.0, 0.0, 1.0],
        angle: std::f64::consts::FRAC_PI_2 });
    s.insert(ball(1, Some(42), 0.0));
    // In-fan, off-plane inside thickness, beyond thickness, outside the arc,
    // within each endpoint cylinder and past radial reach.
    for (id, pos) in [(2, [500.0, 500.0, 0.0]), (3, [500.0, 500.0, 99.0]), (4, [500.0, 500.0, 101.0]),
        (5, [-500.0, -500.0, 0.0]), (6, [110.0, -50.0, 0.0]), (7, [-50.0, 110.0, 0.0]),
        (8, [1_100.0, 0.0, 0.0]), (9, [990.0, -50.0, 0.0])] {
        let mut b = ball(id, None, pos[0]);
        b.pos = pos;
        s.insert(b);
    }
    assert_eq!(victim_ids(&s, &spec), vec![2, 3, 6, 7, 9]);
}

#[test]
fn target_aim_suppresses_dead_or_replaced_targets() {
    let mut s = System::new(1, Arc::default(), 1_000);
    let spec = spec(1, Aim::Target { id: 2, ball_generation: 5 });
    s.insert(ball(1, Some(42), 0.0));
    // No combat membership record at all: the captured generation mismatches
    // and the hit is suppressed, exactly like a destroyed or replaced target.
    s.insert(ball(2, None, 10.0));
    assert!(victim_ids(&s, &spec).is_empty());
    assert!(s.remove_ball(2).is_some());
    assert!(victim_ids(&s, &spec).is_empty());
}
