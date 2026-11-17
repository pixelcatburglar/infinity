//! Actor-owned ramp readouts: registration dedup, membership fencing, the
//! changed-only publish, per-generation clear and the stopped-generation skip.
use super::*;
use crate::services::combat::groups_health::{Condition, ShipStats};
use crate::services::fit::{Fit, Key};
use crate::space::{Ball, MODE_STOP, Pilots, WarpState};
use marshal::Value;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::mpsc;

fn ball(id: i64, pilot: Option<i64>) -> Ball {
    Ball { id, type_id: 1, owner_id: 1, corp_id: 1, alliance_id: -1, char_id: pilot, signature_radius: pilot.map(|_| 1.0),
        mode: MODE_STOP, radius: 1.0, mass: 1e6, max_vel: 0.0, agility: 1.0, speed_fraction: 0.0, pos: [0.0; 3], vel: [0.0; 3],
        goto: [0.0; 3], old_pos: [0.0; 3], old_vel: [0.0; 3], segment_acceleration: None, follow_id: 0, follow_range: 0.0,
        free: true, global: false, interactive: pilot.is_some(), massive: true, warp: WarpState::default(), last_ms: 0,
        interpolation_anchor_ms: 0, effect_stamp: 0, cloak: 0, cloak_until: 0, health: [1.0; 3], launcher: 0, slim: Vec::new() }
}

fn armed() -> (System, u64, mpsc::UnboundedReceiver<(String, Vec<Value>)>) {
    crate::tests::init_config();
    let registry = Arc::new(Pilots::default());
    let (notify, notifications) = mpsc::unbounded_channel();
    registry.register_pilot(10, notify);
    let mut s = System::new(1, registry, 0);
    s.insert(ball(1, Some(10)));
    let fit = Fit::for_space_type(crate::sde::types::tests::combat_fixture(), 1, Vec::new());
    let stats = ShipStats::from_fit(&fit, Key::Ship);
    s.install_condition(1, Condition { now: stats.max, stats, shield_at: crate::client::filetime_now() }, fit);
    let membership = s.combat_membership_generation(1).unwrap();
    (s, membership, notifications)
}

fn rows(rx: &mut mpsc::UnboundedReceiver<(String, Vec<Value>)>) -> Vec<f64> {
    let (event, args) = rx.try_recv().expect("OnModuleAttributeChanges");
    assert_eq!(event, "OnModuleAttributeChanges");
    let Some(Value::List(rows)) = args.into_iter().next() else { panic!("row list") };
    rows.iter().map(|row| match row {
        Value::Tuple(fields) => match &fields[5] { Value::Float(value) => *value, _ => panic!("float") }, _ => panic!("tuple") }).collect()
}

#[test]
fn readouts_prime_publish_clear_and_skip_stopped_generations() {
    let (mut s, membership, mut rx) = armed();
    let stop = Arc::new(AtomicBool::new(false));
    s.spool_readout_begin(1, 7, 5, membership, &stop, 42, 10);
    assert_eq!((s.spool_readouts(1), rows(&mut rx)), (vec![(7, 0.0, 42)], vec![0.0, 42.0]));
    s.spool_readout_begin(1, 7, 5, membership, &stop, 43, 10);
    s.spool_readout_begin(1, 8, 5, membership + 1, &stop, 44, 10);
    s.spool_readout_publish(1, 7, &stop, 0.0, 42, 10);
    assert_eq!(s.spool_readouts(1), vec![(7, 0.0, 42)]);
    assert!(rx.try_recv().is_err());
    s.spool_readout_publish(1, 7, &stop, 0.07, 45, 10);
    assert_eq!((s.spool_readouts(1), rows(&mut rx)), (vec![(7, 0.07, 45)], vec![0.07, 45.0]));
    let next = Arc::new(AtomicBool::new(false));
    s.spool_readout_begin(1, 7, 5, membership, &next, 50, 10);
    assert_eq!(rows(&mut rx), vec![0.0, 50.0]);
    stop.store(true, Ordering::Relaxed);
    assert_eq!(s.spool_readouts(1), vec![(7, 0.0, 50)]);
    s.spool_readout_clear(1, &stop);
    assert_eq!((s.spool_readouts(1), rows(&mut rx)), (vec![(7, 0.0, 50)], vec![0.0, 0.0]));
}
