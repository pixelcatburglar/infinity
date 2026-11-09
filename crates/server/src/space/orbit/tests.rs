use crate::space::{tests::destiny_reference::ship, Pilots, System, MODE_GOTO, MODE_ORBIT, MODE_STOP};
use std::sync::Arc;

#[test]
fn default_orbital_normal_converts_negative_zero_to_positive_zero() {
    let orbiter = ship(3, [0.0; 3], [0.0; 3]);
    assert_eq!(orbiter.default_orbital_normal(0)[2].to_bits(), 0.0_f64.to_bits());
}

#[test]
fn orbit_uses_target_position_before_integration() {
    crate::tests::init_config();
    let mut system = System::new(1, Arc::<Pilots>::default(), 0);
    let mut target = ship(1, [1000.0, 0.0, 0.0], [0.0, 100.0, 0.0]);
    target.goto_direction([0.0, 1.0, 0.0]);
    let mut orbiter = ship(2, [0.0; 3], [0.0; 3]);
    assert!(orbiter.orbit(&target, 500.0));
    // For an x-axis separation, the native plane for id 2 gives radial -Z.
    let radial = ((-(480.0_f64 * 480.0) / 40_000.0).exp() * 10_000_000.0) as i64 as f64 / 10_000_000.0;
    let transverse = -0.52 * radial + (1.0 + radial * radial * (0.52 * 0.52 - 1.0)).sqrt();
    let acceleration = [transverse * (1.0_f64 - 0.52 * 0.52).sqrt() * 200.0, 0.0,
        -(radial + transverse * 0.52) * 200.0];
    system.insert(orbiter);
    system.insert(target);
    let orbiter = system.ball(2).unwrap();
    assert_eq!((orbiter.mode, orbiter.follow_id, orbiter.follow_range), (MODE_ORBIT, 1, 500.0));
    system.tick(1000);
    let actual = system.ball(2).unwrap();
    let actual_acceleration = actual.segment_acceleration.unwrap();
    for (index, (actual, expected)) in actual_acceleration.into_iter().zip(acceleration).enumerate() {
        assert!(
            (actual - expected).abs() < 1e-10,
            "component {index}: actual={actual:?}, expected={expected:?}, actual acceleration={actual_acceleration:?}"
        );
    }
    assert_ne!(system.ball(1).unwrap().pos, [1000.0, 0.0, 0.0]);
}

#[test]
fn coincident_zero_throttle_orbit_stays_finite() {
    crate::tests::init_config();
    let mut system = System::new(1, Arc::<Pilots>::default(), 0);
    let target = ship(1, [0.0; 3], [0.0; 3]);
    let mut orbiter = ship(2, [0.0; 3], [0.0; 3]);
    orbiter.speed_fraction = 0.0;
    assert!(orbiter.orbit(&target, 0.0));
    system.insert(target);
    system.insert(orbiter);
    system.tick(1000);
    let b = system.ball(2).unwrap();
    assert!(b.pos.into_iter().chain(b.vel).all(f64::is_finite));
}

#[test]
fn removing_target_stops_npc_and_sends_interactive_orbiter_along_velocity() {
    let mut system = System::new(1, Arc::<Pilots>::default(), 0);
    let target = ship(1, [1000.0, 0.0, 0.0], [0.0; 3]);
    let mut pilot = ship(2, [0.0; 3], [3.0, 4.0, 0.0]);
    let mut npc = ship(3, [0.0; 3], [0.0; 3]);
    npc.interactive = false;
    assert!(pilot.orbit(&target, 100.0));
    assert!(npc.orbit(&target, 100.0));
    system.insert(target);
    system.insert(pilot);
    system.insert(npc);
    system.take(1);
    assert_eq!(system.ball(2).unwrap().mode, MODE_GOTO);
    assert_eq!(system.ball(3).unwrap().mode, MODE_STOP);
}
