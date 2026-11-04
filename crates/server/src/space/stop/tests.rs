use super::*;
use crate::consts::DAMAGE_LAYER_COUNT;
use crate::space::{DESTINY_FRICTION, MODE_GOTO, MODE_WARP, WarpState, integration_factor};
use std::collections::HashMap;

fn ball() -> Ball {
    Ball {
        id: 1, type_id: 1, owner_id: 2, corp_id: 3, alliance_id: 4, char_id: None,
        mode: MODE_GOTO, radius: 1.0, signature_radius: None, mass: 1e6, max_vel: 100.0,
        agility: 1.0, speed_fraction: 0.75, pos: [0.0; 3], vel: [10.0, 20.0, 30.0],
        old_pos: [0.0; 3], old_vel: [0.0; 3], segment_acceleration: None,
        goto: [1000.0, 0.0, 0.0], follow_id: 5, follow_range: 6.0, free: true,
        global: false, massive: false, interactive: false, warp: WarpState::default(), warp_factor: 3,
        last_ms: 0, interpolation_anchor_ms: 0, effect_stamp: 7, cloak: 0, cloak_until: 0,
        health: [1.0; DAMAGE_LAYER_COUNT], launcher: 8, slim: vec![],
    }
}

#[test]
fn stop_preserves_throttle_velocity_and_only_clears_follow_state() {
    for mode in [MODE_FOLLOW, MODE_ORBIT, MODE_GOTO, MODE_MISSILE, MODE_WARP] {
        let mut b = ball();
        b.mode = mode;
        b.warp.length = 1000.0;
        b.warp.min_range = 10.0;
        b.warp.factor = 3;
        b.stop();
        assert_eq!(b.mode, MODE_STOP);
        assert_eq!(b.speed_fraction, 0.75);
        assert_eq!(b.vel, [10.0, 20.0, 30.0]);
        assert_eq!((b.warp.length, b.warp.min_range, b.warp.factor), (1000.0, 10.0, 3));
        let expected = if matches!(mode, MODE_FOLLOW | MODE_ORBIT | MODE_MISSILE) { (0, 0, 0, 0.0) } else { (7, 5, 8, 6.0) };
        assert_eq!((b.effect_stamp, b.follow_id, b.launcher, b.follow_range), expected);
        assert_eq!(b.owner_id, 2);
    }
}

#[test]
fn stop_damps_y_before_tick_and_interpolation() {
    let mut b = ball();
    b.stop();
    b.evolve_behavior_for_ball(1000, 1000, &HashMap::new(), 0);
    b.evolve_position(1000, integration_factor(b.mass * b.agility as f64, DESTINY_FRICTION));
    let damped = (20.0 - STOP_VERTICAL_DAMPING * 20.0) * STOP_VERTICAL_RETAIN;
    assert_eq!(b.old_vel, [10.0, damped, 30.0]);
    let factor = (-1.0_f64).exp();
    for (i, initial) in [10.0, damped, 30.0].into_iter().enumerate() {
        assert!((b.vel[i] - initial * factor).abs() < 1e-12);
        assert!((b.pos[i] - initial * (1.0 - factor)).abs() < 1e-12);
        assert!((b.position_at(1500)[i] - initial * (1.0 - (-0.5_f64).exp())).abs() < 1e-12);
    }
}

#[test]
fn setters_reject_invalid_and_preserve_mode() {
    let mut b = ball();
    for x in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        b.set_speed_fraction(x); assert!(!b.set_max_speed(f64::from(x)));
    }
    for x in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, 0.0, -1.0] { assert!(!b.set_ball_mass(x)); }
    for x in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, 0.0, -1.0, f64::MAX, f64::MIN_POSITIVE] {
        assert!(!b.set_ball_agility(x));
    }
    for x in [-1.0, f64::MAX] { assert!(!b.set_max_speed(x)); }
    assert_eq!((b.speed_fraction, b.max_vel, b.mass, b.agility, b.mode, b.vel),
        (0.75, 100.0, 1e6, 1.0, MODE_GOTO, [10.0, 20.0, 30.0]));
    b.set_speed_fraction(-2.0); assert_eq!(b.speed_fraction, 0.0);
    b.set_speed_fraction(2.0); assert_eq!(b.speed_fraction, 1.0);
    assert!(b.set_max_speed(0.0)); assert!(b.set_ball_mass(2.0)); assert!(b.set_ball_agility(0.5));
    assert_eq!((b.max_vel, b.mass, b.agility, b.mode, b.vel), (0.0, 2.0, 0.5, MODE_GOTO, [10.0, 20.0, 30.0]));
    assert!(b.set_ball_agility(f64::from(f32::from_bits(1))));
    assert!(b.set_max_speed(f64::MIN_POSITIVE));
    assert!(b.set_ball_mass(f64::MIN_POSITIVE));
}

#[test]
fn low_effective_mass_is_unfloored_for_tick_and_interpolation() {
    let mut b = ball();
    b.set_ball_mass(100_000.0);
    b.agility = 0.5;
    b.vel = [10.0, 0.0, 0.0];
    b.stop();
    b.evolve_behavior_for_ball(1000, 1000, &HashMap::new(), 0);
    b.evolve_position(1000, integration_factor(b.mass * b.agility as f64, DESTINY_FRICTION));
    let expected = 10.0 * 0.05 * (1.0 - (-20.0_f64).exp());
    let half = 10.0 * 0.05 * (1.0 - (-10.0_f64).exp());
    assert!((b.pos[0] - expected).abs() < 1e-12);
    assert!((b.position_at(1500)[0] - half).abs() < 1e-12);
    assert!(b.pos.into_iter().chain(b.vel).chain(b.position_at(1500)).all(f64::is_finite));
}

#[test]
fn nonpositive_effective_mass_does_not_produce_nan() {
    let mut b = ball();
    b.mass = 0.0;
    b.stop();
    b.evolve_behavior_for_ball(1000, 1000, &HashMap::new(), 0);
    b.evolve_position(1000, integration_factor(b.mass * b.agility as f64, DESTINY_FRICTION));
    assert_eq!(b.pos, [0.0; 3]);
    assert_eq!(b.vel, [0.0; 3]);
    assert_eq!(b.position_at(1500), [0.0; 3]);
}
