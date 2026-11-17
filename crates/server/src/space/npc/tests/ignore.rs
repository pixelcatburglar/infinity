use super::*;
use crate::space::drone::DroneSpec;

fn drone_spec() -> DroneSpec {
    DroneSpec { type_id: 1, bandwidth: 1.0, scan_resolution: 1.0, control_range: 100.0,
        max_target_range: 0.0, optimal_range: 50.0, orbit_range: 20.0, duration_ms: 1000.0, effect_id: None }
}

#[test]
fn ignored_pilot_retires_lock_and_reacquires_after_off() {
    let (mut s, mut jobs, mut notifications) = setup(Some(HOSTILE_TO_ALL));
    s.insert(ball(1, Some(10), 50.0));
    s.brains.get_mut(&1000).unwrap().next_think_ms = 0;
    s.tick(0);
    let (_, _, deadline) = s.brains[&1000].target.unwrap();
    s.tick(deadline);
    assert!(s.brains[&1000].announced);
    while jobs.try_recv().is_ok() {}
    let stale = s.brains[&1000].target_generation;
    s.set_npc_ignored(10, true);
    assert!(s.brains[&1000].target.is_none() && !s.brains[&1000].announced);
    assert!(!s.npc_projection_valid(1000, 1, stale));
    let lost = std::iter::from_fn(|| notifications.try_recv().ok()).any(|(name, args)| name == "OnTarget" && args.first() == Some(&Value::str("otherlost")));
    assert!(lost);
    s.brains.get_mut(&1000).unwrap().next_think_ms = 0;
    s.tick(deadline + 100);
    assert!(s.brains[&1000].target.is_none() && jobs.try_recv().is_err());
    s.set_npc_ignored(10, false);
    s.brains.get_mut(&1000).unwrap().next_think_ms = 0;
    s.tick(deadline + 200);
    let (ship, _, locked) = s.brains[&1000].target.unwrap();
    assert_eq!(ship, 1);
    s.tick(locked);
    assert!(s.brains[&1000].announced);
}

#[test]
fn ownerless_drone_follows_flag_and_abandon_restores() {
    let (mut s, _, _) = setup(Some(HOSTILE_TO_ALL));
    for (id, owner, x) in [(2, 10, 40.0), (3, 20, 50.0)] {
        let mut drone = ball(id, None, x);
        drone.owner_id = owner;
        drone.signature_radius = Some(1.0);
        s.insert(drone);
        s.register_drone(id, drone_spec(), Arc::new(|_, _| Some(Box::pin(async {}))), None, Some(owner), false);
    }
    s.set_npc_ignored(10, true);
    s.brains.get_mut(&1000).unwrap().next_think_ms = 0;
    s.tick(0);
    assert_eq!(s.pilots().count(), 0);
    assert_eq!(s.brains[&1000].target.map(|(ship, _, _)| ship), Some(3));
    s.drone_abandoned(2);
    s.take(3);
    s.brains.get_mut(&1000).unwrap().next_think_ms = 0;
    s.tick(2000);
    assert_eq!(s.brains[&1000].target.map(|(ship, _, _)| ship), Some(2));
}
