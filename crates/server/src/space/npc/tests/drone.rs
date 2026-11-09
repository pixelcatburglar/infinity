use super::*;
use crate::space::drone::DroneSpec;

#[test]
fn npc_locks_drone_without_pilot_index() {
    let (mut s, mut jobs, _) = setup(Some(10));
    let mut target = ball(2, None, 50.0);
    target.owner_id = 10;
    target.signature_radius = Some(1.0);
    let spec = DroneSpec { type_id: 1, bandwidth: 1.0, scan_resolution: 1.0, control_range: 100.0,
        max_target_range: 100.0, optimal_range: 50.0, orbit_range: 20.0, duration_ms: 1000.0, effect_id: None };
    s.insert(target.clone());
    s.register_drone(2, spec, Arc::new(|_, _| Some(Box::pin(async {}))), None, Some(10), false);
    s.brains.get_mut(&1000).unwrap().next_think_ms = 0;
    s.tick(0);
    let (_, _, deadline) = s.brains[&1000].target.unwrap();
    s.tick(deadline);
    assert!(s.brains[&1000].announced);
    assert!(matches!(jobs.try_recv().unwrap(), NpcAction::Shot { victim, .. } if victim.id == 2));
    assert_eq!(s.pilots().count(), 0);
}
