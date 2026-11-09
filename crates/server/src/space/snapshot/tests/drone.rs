use super::*;
use crate::space::drone::DroneSpec;

#[test]
fn drones_without_pilots_are_snapshot_candidates() {
    crate::tests::init_config();
    let mut s = System::default();
    let spec = DroneSpec { type_id: 1, bandwidth: 1.0, scan_resolution: 1.0, control_range: 100.0,
        max_target_range: 100.0, optimal_range: 50.0, orbit_range: 20.0, duration_ms: 1000.0, effect_id: None };
    let drone = ball(1, None, 0.0);
    s.insert(drone);
    s.register_drone(1, spec, Arc::new(|_, _| Some(Box::pin(async {}))), None, None, false);
    assert!(s.snapshot_due > 0);
    s.command(1, |b, _, _| { b.pos[0] = 5.0; Vec::new() });
    s.tick(1000);
    assert_eq!(s.pilots().count(), 0);
    assert_eq!(candidates(&s).map(|(id, _)| id).collect::<Vec<_>>(), vec![1]);
}
