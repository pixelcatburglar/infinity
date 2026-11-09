use super::*;
use crate::space::drone::{DroneEffects, DroneOrder, DroneSpec};

fn spec() -> DroneSpec {
    DroneSpec { type_id: 1, bandwidth: 5.0, scan_resolution: 100.0, control_range: 10000.0,
        max_target_range: 10000.0, optimal_range: 1000.0, orbit_range: 500.0, duration_ms: 1000.0, effect_id: Some(1) }
}
fn effects() -> DroneEffects { Arc::new(|_, _| Some(Box::pin(async {}))) }
fn setup() -> System {
    let mut s = System::new(1, Arc::default(), 1000);
    s.insert(ball(10, Some(20), 0.0));
    s
}
fn drone_ball(id: i64) -> Ball {
    let mut b = ball(id, None, 10.0); b.owner_id = 20; b.signature_radius = Some(1.0); b
}

#[test]
fn combat_owner_uses_live_pilot_or_owned_drone() {
    let mut s = setup();
    let first_ball = drone_ball(30);
    s.insert(first_ball);
    s.register_drone(30, spec(), effects(), None, Some(20), false);
    let mut unowned = drone_ball(31);
    unowned.owner_id = infinity_core::consts::OWNER_UNOWNED;
    s.insert(unowned);
    s.register_drone(31, spec(), effects(), None, None, false);
    s.insert(ball(32, None, 0.0));

    assert_eq!(s.combat_owner(10), Some(20));
    assert_eq!(s.combat_owner(30), Some(20));
    assert_eq!(s.combat_owner(31), None);
    assert_eq!(s.combat_owner(32), None);
    s.balls.get_mut(&30).unwrap().char_id = Some(21);
    assert_eq!(s.combat_owner(30), Some(21));
    s.balls.remove(&30);
    assert!(s.drone(30).is_some());
    assert_eq!(s.combat_owner(30), None);
}

#[test]
fn reservations_count_pending_and_release() {
    let mut s = setup();
    let first = s.reserve_drone_launch(20, 10, &[5.0], 2.0, 10.0).unwrap();
    assert!(s.reserve_drone_launch(20, 10, &[5.0, 5.0], 2.0, 10.0).is_none());
    assert!(s.reserve_drone_launch(20, 10, &[f64::NAN], 2.0, 10.0).is_none());
    s.cancel_drone_launch(first);
    let second = s.reserve_drone_launch(20, 10, &[5.0, 5.0], 2.0, 10.0).unwrap();
    assert!(s.complete_drone_launch_reservation(second, 20, 10));
    for id in [30, 31] {
        s.insert(drone_ball(id));
        s.register_drone(id, spec(), effects(), Some(10), Some(20), false);
    }
    assert_eq!(s.drones().count(), 2);
    assert!(s.reserve_drone_launch(20, 10, &[1.0], 2.0, 100.0).is_none());
    assert_eq!(s.pilots().count(), 1);
}

#[test]
fn restored_drone_return_and_generation_are_distinct() {
    let mut s = setup();
    s.insert(drone_ball(30));
    s.register_drone(30, spec(), effects(), None, Some(20), false);
    let old = s.drone(30).unwrap();
    assert_eq!(old.controller, None);
    assert!(s.drone_order(30, 20, 10, DroneOrder::ReturnBay));
    assert!(!s.drone_current(30, old.generation));
    assert_eq!(s.drone(30).unwrap().order, DroneOrder::ReturnBay);
    s.drone_controller_lost(10);
    assert_eq!(s.drone(30).unwrap().controller, None);
    assert!(s.drone_order(30, 20, 10, DroneOrder::ReturnBay));
}

#[test]
fn stale_reservation_still_materializes_uncontrolled_item() {
    let mut s = setup();
    let token = s.reserve_drone_launch(20, 10, &[5.0], 1.0, 5.0).unwrap();
    s.cancel_drone_launch(token);
    assert!(!s.complete_drone_launch_reservation(token, 20, 10));
    s.insert(drone_ball(30));
    s.register_drone(30, spec(), effects(), None, Some(20), false);
    assert!(s.contains(30));
    assert_eq!(s.drone(30).unwrap().controller, None);
    assert_eq!(s.persistent_balls().count(), 2);
}

#[test]
fn contact_claim_serializes_orders_and_checks_generation() {
    let mut s = setup();
    s.insert(drone_ball(30));
    s.register_drone(30, spec(), effects(), None, Some(20), false);
    assert!(s.drone_order(30, 20, 10, DroneOrder::ReturnBay));
    let d = s.drone(30).unwrap();
    assert_eq!(d.controller, None);
    assert_eq!(d.target, Some(10));
    s.tick(1100);
    assert!(s.drone_claim_contact(30, d.generation.wrapping_add(1)).is_none());
    assert!(s.drone_claim_contact(30, d.generation).is_some());
    assert!(!s.drone_order(30, 20, 10, DroneOrder::ReturnBay));
    s.drone_release_contact(30, d.generation);
    assert!(s.drone_order(30, 20, 10, DroneOrder::ReturnBay));
}

#[test]
fn launch_state_waits_for_add_balls_boundary() {
    crate::tests::init_config();
    let registry = Arc::new(Pilots::default());
    let (notify, mut notices) = mpsc::unbounded_channel();
    registry.register_pilot(20, notify);
    let mut s = System::new(1, registry, 1000);
    s.insert(ball(10, Some(20), 0.0));
    let (destiny, mut updates) = mpsc::unbounded_channel();
    s.observe(1, 10, destiny);
    let token = s.reserve_drone_launch(20, 10, &[5.0], 1.0, 5.0).unwrap();
    assert!(s.complete_drone_launch_reservation(token, 20, 10));
    s.add_ship(drone_ball(30));
    s.register_drone(30, spec(), effects(), Some(10), Some(20), false);
    s.queue_drone_announcement(30);
    let (ack_tx, ack_rx) = tokio::sync::oneshot::channel();
    s.queue_drone_launch_ack(ack_tx);
    let mut ack = ack_rx;
    assert!(notices.try_recv().is_err());
    assert!(updates.try_recv().is_err());
    assert!(ack.try_recv().is_err());
    s.tick(2000);
    assert!(updates.try_recv().is_ok());
    assert_eq!(notices.try_recv().unwrap().0, "OnDroneStateChange");
    assert!(ack.try_recv().is_ok());
}

#[test]
fn controller_jam_invalidates_locked_drone_generation() {
    let mut s = setup();
    let token = s.reserve_drone_launch(20, 10, &[5.0], 1.0, 5.0).unwrap();
    assert!(s.complete_drone_launch_reservation(token, 20, 10));
    s.insert(drone_ball(30));
    s.register_drone(30, spec(), effects(), Some(10), Some(20), false);
    let mut target = ball(40, None, 20.0);
    target.signature_radius = Some(1.0);
    s.insert(target);
    assert!(s.drone_order(30, 20, 10, DroneOrder::Engage(40)));
    s.tick(1100);
    s.tick(1_000_000);
    let generation = s.drone(30).unwrap().generation;
    assert!(s.drone_valid(30, 40, generation));
    s.drop_npc_targets(10);
    assert!(!s.drone_current(30, generation));
    assert!(!s.drone_valid(30, 40, generation));
}

#[test]
fn observer_bootstrap_only_registers_local_owned_drones() {
    let mut s = setup();
    s.insert(ball(11, Some(21), 0.0));
    s.insert(drone_ball(30));
    s.register_drone(30, spec(), effects(), None, Some(20), false);
    let (tx1, _rx1) = mpsc::unbounded_channel();
    let (tx2, _rx2) = mpsc::unbounded_channel();
    let owner = s.observe(1, 10, tx1);
    let other = s.observe(2, 11, tx2);
    let rows = |event: &Value| {
        let call = &event.items().unwrap()[1];
        let args = &call.items().unwrap()[1];
        let Value::Instance(_, fields) = &args.items().unwrap()[0] else { panic!("expected state KeyVal") };
        fields.get("droneState").unwrap().clone()
    };
    let owner_rows = rows(&owner);
    let [Value::Instance(_, fields)] = owner_rows.items().unwrap() else { panic!("expected one drone row") };
    assert_eq!(fields.get("droneID"), Some(&Value::Int(30)));
    assert_eq!(fields.get("ownerID"), Some(&Value::Int(20)));
    assert_eq!(fields.get("controllerID"), Some(&Value::None));
    assert!(rows(&other).items().unwrap().is_empty());
}

#[test]
fn contact_claim_survives_controller_loss_and_stale_release() {
    let mut s = setup();
    let token = s.reserve_drone_launch(20, 10, &[5.0], 1.0, 5.0).unwrap();
    assert!(s.complete_drone_launch_reservation(token, 20, 10));
    s.insert(drone_ball(30));
    s.register_drone(30, spec(), effects(), Some(10), Some(20), false);
    assert!(s.drone_order(30, 20, 10, DroneOrder::ReturnBay));
    let old = s.drone(30).unwrap().generation;
    s.tick(1100);
    assert!(!s.drone_owns_contact(30, old));
    assert!(s.drone_claim_contact(30, old).is_some());
    assert!(s.drone_owns_contact(30, old));
    s.drone_controller_lost(10);
    assert_eq!(s.drone(30).unwrap().controller, None);
    assert!(s.drone_owns_contact(30, old));
    assert!(!s.drone_order(30, 20, 10, DroneOrder::ReturnBay));
    s.drone_release_contact(30, old);
    assert!(!s.drone_owns_contact(30, old));
    assert!(s.drone_order(30, 20, 10, DroneOrder::ReturnBay));
    let new = s.drone(30).unwrap().generation;
    s.tick(1200);
    assert!(s.drone_claim_contact(30, new).is_some());
    assert!(s.drone_owns_contact(30, new));
    assert!(!s.drone_owns_contact(30, old));
    s.drone_release_contact(30, old);
    assert!(s.drone_owns_contact(30, new));
    assert!(!s.drone_order(30, 20, 10, DroneOrder::ReturnBay));
    s.remove_ball(30);
    assert!(!s.contains(30));
    assert!(s.drone(30).is_none());
    assert!(!s.drone_owns_contact(30, new));
    s.insert(drone_ball(30));
    s.register_drone(30, spec(), effects(), None, Some(20), false);
    assert!(!s.drone_owns_contact(30, old));
    assert!(!s.drone_owns_contact(30, new));
    s.drone_release_contact(30, old);
    assert_eq!(s.drones().count(), 1);
}

#[test]
fn zero_targeting_attributes_and_positive_shared_lock() {
    let mut s = setup();
    let mut target = ball(40, None, 20.0);
    target.signature_radius = Some(1.0);
    s.insert(target);
    let mut zero = spec();
    zero.scan_resolution = 0.0;
    zero.max_target_range = 0.0;
    let token = s.reserve_drone_launch(20, 10, &[5.0], 1.0, 5.0).unwrap();
    assert!(s.complete_drone_launch_reservation(token, 20, 10));
    s.insert(drone_ball(30));
    s.register_drone(30, zero, effects(), Some(10), Some(20), false);
    assert!(s.drone_order(30, 20, 10, DroneOrder::Engage(40)));
    s.tick(1100);
    let generation = s.drone(30).unwrap().generation;
    assert!(s.drone_valid(30, 40, generation));

    let mut s = setup();
    let mut target = ball(40, None, 20.0);
    target.signature_radius = Some(1.0);
    s.insert(target);
    let mut positive = spec();
    positive.max_target_range = 5.0;
    let token = s.reserve_drone_launch(20, 10, &[5.0], 1.0, 5.0).unwrap();
    assert!(s.complete_drone_launch_reservation(token, 20, 10));
    s.insert(drone_ball(30));
    s.register_drone(30, positive, effects(), Some(10), Some(20), false);
    assert!(s.drone_order(30, 20, 10, DroneOrder::Engage(40)));
    s.tick(1100);
    assert!(!s.drone_valid(30, 40, s.drone(30).unwrap().generation));

    let mut s = setup();
    let mut target = ball(40, None, 20.0);
    target.signature_radius = Some(1.0);
    s.insert(target);
    let token = s.reserve_drone_launch(20, 10, &[5.0], 1.0, 5.0).unwrap();
    assert!(s.complete_drone_launch_reservation(token, 20, 10));
    s.insert(drone_ball(30));
    s.register_drone(30, spec(), effects(), Some(10), Some(20), false);
    assert!(s.drone_order(30, 20, 10, DroneOrder::Engage(40)));
    s.tick(1100);
    let generation = s.drone(30).unwrap().generation;
    let deadline = 1100 + lock_time_ms(100.0, 1.0) as i64;
    s.tick(deadline - 1);
    assert!(!s.drone_valid(30, 40, generation));
    s.tick(deadline);
    assert!(s.drone_valid(30, 40, generation));
}

#[test]
fn raw_mining_pending_lifecycle() {
    use crate::space::drone::PendingMining;
    use crate::services::mining::rounding::{MiningDraws, MiningInputs};
    let mut s = setup();
    let mut target = ball(40, None, 20.0);
    target.signature_radius = Some(1.0);
    s.insert(target);
    let mut mining_spec = spec();
    mining_spec.scan_resolution = 0.0;
    let token = s.reserve_drone_launch(20, 10, &[5.0], 1.0, 5.0).unwrap();
    assert!(s.complete_drone_launch_reservation(token, 20, 10));
    s.insert(drone_ball(30));
    s.register_drone(30, mining_spec, effects(), Some(10), Some(20), false);
    assert!(s.drone_order(30, 20, 10, DroneOrder::Mine(40)));
    s.tick(1100);
    let generation = s.drone(30).unwrap().generation;
    let pending = PendingMining {
        rock: 40, type_id: 1, unit_volume: 2.0,
        inputs: MiningInputs { clamp_factor: 0.5, volume: 3.0, waste_volume_multiplier: 0.2, waste_probability: 4.0, crit_quantity_multiplier: 0.3, crit_probability: 0.4, efficiency: 0.9 },
        draws: MiningDraws { waste_check: 7, normal_round: 0.1, waste_round: 0.2, crit_check: 0.3, crit_round: 0.4 },
    };
    assert!(!s.drone_pending(30, generation.wrapping_add(1), pending.clone()));
    assert!(s.drone_pending(30, generation, pending.clone()));
    let stored = s.drone(30).unwrap().pending.unwrap();
    assert_eq!(format!("{stored:?}"), format!("{pending:?}"));
    s.drone_delivered(30, generation, true);
    assert!(s.drone(30).unwrap().pending.is_none());
    assert_eq!(s.drone(30).unwrap().order, DroneOrder::Mine(40));
    assert!(s.drone_order(30, 20, 10, DroneOrder::Mine(40)));
    s.tick(1200);
    let next = s.drone(30).unwrap().generation;
    assert_ne!(generation, next);
    assert!(s.drone_pending(30, next, pending));
    s.drone_controller_lost(10);
    assert!(s.drone(30).unwrap().pending.is_none());
}
