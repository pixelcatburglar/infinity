use super::*;
use crate::space::tests::destiny_reference::ship;

#[test]
fn launch_resets_both_states_and_sets_native_follow_fields() {
    let launcher = ship(1, [10.0, 20.0, 30.0], [4.0, 0.0, 0.0]);
    let target = ship(3, [110.0, 20.0, 30.0], [0.0; 3]);
    let mut missile = ship(2, [0.0, 100.0, 0.0], [0.0; 3]);
    missile.radius = 1.0;
    missile.launch_native_missile((launcher.pos, launcher.vel, launcher.max_vel), Some((target.id, target.pos, target.radius, target.cloak)), 1, 42);
    assert_eq!((missile.pos, missile.old_pos), (launcher.pos, launcher.pos));
    assert_eq!((missile.vel, missile.old_vel), ([1.0, 0.0, 0.0], [1.0, 0.0, 0.0]));
    assert_eq!((missile.mode, missile.follow_id, missile.launcher, missile.effect_stamp), (MODE_MISSILE, 3, 1, 42));
    assert_eq!(missile.follow_range, -11.0);
    assert_eq!(missile.goto, [1e16, 0.0, 0.0]);
}

#[test]
fn fallback_uses_launcher_speed_and_cloaked_target_does_not_follow() {
    let mut missile = ship(2, [0.0; 3], [0.0; 3]);
    missile.launch_native_missile(([1.0, 2.0, 3.0], [0.0; 3], 200.0), Some((3, [10.0; 3], 10.0, 1)), 1, 7);
    assert_eq!(missile.pos, [1.0, 2.0, 3.0]);
    assert_eq!(missile.vel, [0.0, 0.0, -200.0]);
    assert_eq!(missile.mode, MODE_STOP);
    missile.launch_native_missile(([1.0, 2.0, 3.0], [0.0; 3], 0.0), None, 1, 7);
    assert_eq!(missile.vel, [0.0, 0.0, -150.0]);
    assert_eq!(missile.mode, MODE_STOP);
}

#[test]
fn prediction_uses_target_velocity_without_arrival_throttling() {
    let mut missile = ship(2, [0.0; 3], [0.0; 3]);
    missile.mass = 1e6;
    missile.radius = 1.0;
    missile.follow_range = -11.0;
    missile.effect_stamp = 0;
    missile.max_vel = 100.0;
    let target = TargetState { pos: [10.0, 0.0, 0.0], vel: [0.0, 20.0, 0.0], radius: 10.0 };
    let thrust = missile.evolve_missile(&target, 1);
    assert_eq!(missile.goto, [10.0, 2.0, 0.0]);
    assert!((len(thrust) - 100.0).abs() < 1e-10);
}

#[test]
fn sorted_behavior_observes_stopy_and_target_loss_drifts() {
    crate::tests::init_config();
    let mut system = System::default();
    let target = ship(1, [100.0, 0.0, 0.0], [0.0, 20.0, 0.0]);
    let mut missile = ship(2, [0.0; 3], [1.0, 0.0, 0.0]);
    missile.mode = MODE_MISSILE;
    missile.follow_id = 1;
    missile.effect_stamp = -2;
    missile.follow_range = -20.0;
    missile.massive = false;
    system.insert(target);
    system.insert(missile);
    system.tick(1000);
    let expected_y = 20.0 * (1.0 - 0.07) * 0.9345794392523364485981308411215 * 0.5;
    assert!((system.ball(2).unwrap().goto[1] - expected_y).abs() < 1e-12);
    let velocity = system.ball(2).unwrap().vel;
    system.take(1);
    let missile = system.ball(2).unwrap();
    assert_eq!(missile.mode, MODE_GOTO);
    assert!(!missile.massive);
    assert_eq!(missile.goto, add(missile.pos, scale(norm(velocity), GOTO_DIRECTION_REACH)));
    let before = missile.pos;
    system.tick(2000);
    assert_ne!(system.ball(2).unwrap().pos, before);
}

#[test]
fn lone_missing_target_missile_keeps_system_active() {
    crate::tests::init_config();
    let mut system = System::default();
    let mut missile = ship(2, [0.0; 3], [10.0, 0.0, 0.0]);
    missile.mode = MODE_MISSILE;
    missile.follow_id = 99;
    system.insert(missile);
    assert!(!system.is_cold(true));
    system.tick(1000);
    assert_eq!(system.ball(2).unwrap().mode, MODE_GOTO);
    assert!(system.ball(2).unwrap().pos[0] > 0.0);
}
