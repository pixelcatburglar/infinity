use super::*;
use crate::space::{Ball, System, WarpState, MODE_STOP};
use std::sync::Arc;

#[test]
fn jam_clears_locks_and_overlaps_expire_independently() {
    crate::tests::init_config();
    let ship = NEXT_LOCK.fetch_add(10, Ordering::Relaxed) as i64;
    let pilot = ship + 1;
    let source = ship + 2;
    let module = ship + 3;
    let other = ship + 4;
    let locked = ship + 5;
    let pending = ship + 6;
    let silent = ship + 7;
    let recipient = ship + 8;
    let silent_recipient = ship + 9;
    let registry = Arc::new(crate::space::Pilots::default());
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (silent_tx, mut silent_rx) = tokio::sync::mpsc::unbounded_channel();
    registry.register_pilot(recipient, tx);
    registry.register_pilot(silent_recipient, silent_tx);
    let mut system = System::new(ship, registry, 1000);
    system.insert(Ball {
        id: ship, type_id: 1, owner_id: pilot, corp_id: 1, alliance_id: 1,
        char_id: Some(pilot), signature_radius: None, mode: MODE_STOP, radius: 1.0,
        mass: 1.0, max_vel: 0.0, agility: 1.0, speed_fraction: 0.0,
        pos: [0.0; 3], vel: [0.0; 3], old_pos: [0.0; 3], old_vel: [0.0; 3],
        segment_acceleration: None, goto: [0.0; 3], follow_id: 0, follow_range: 0.0,
        free: false, global: false, massive: true, interactive: true,
        warp: WarpState::default(), last_ms: 0, interpolation_anchor_ms: 0,
        effect_stamp: 0, cloak: 0, cloak_until: 0, health: [1.0; 3], launcher: 0, slim: Vec::new(),
    });
    {
        let mut st = state();
        st.targets.entry(pilot).or_default().insert(locked);
        st.targets.entry(pilot).or_default().insert(silent);
        st.announced_locks.insert((pilot, locked), (ship, recipient));
        st.locking.insert((pilot, pending), 1);
    }
    apply(&mut system, source, module, ship, 100.0);
    assert!(is_jammed(ship, 1000));
    assert!(!state().targets.contains_key(&pilot));
    let (event, args) = rx.try_recv().unwrap();
    assert_eq!(event, "OnTarget");
    assert_eq!(args[0].as_bytes(), Some(b"otherlost".as_slice()));
    assert_eq!(args[1].as_int(), Some(ship));
    assert!(rx.try_recv().is_err());
    assert!(silent_rx.try_recv().is_err());
    assert!(!state().announced_locks.contains_key(&(pilot, locked)));
    {
        let mut st = state();
        assert!(!st.begin_lock(ship, pilot, pending, 2, 1000));
        assert_eq!(st.complete_lock(ship, pilot, pending, 1, 1000, true), (false, false));
        st.locking.insert((pilot, pending), 3);
        assert_eq!(st.complete_lock(ship, pilot, pending, 3, 1000, true), (true, false));
        assert!(!st.targets.contains_key(&pilot));
    }
    assert!(!state().locking.contains_key(&(pilot, pending)));
    apply(&mut system, other, module, ship, 300.0);
    apply(&mut system, source, module, ship, 200.0);
    system.tick(1100);
    assert!(is_jammed(ship, 1100));
    assert_eq!(state().jammed[&ship].len(), 2);
    system.tick(1200);
    assert!(is_jammed(ship, 1200));
    assert_eq!(state().jammed[&ship].len(), 1);
    system.tick(1300);
    assert!(!is_jammed(ship, 1300));
    assert!(!state().jammed.contains_key(&ship));
}

#[test]
fn jamming_drone_or_controller_invalidates_its_lock_without_losing_control() {
    crate::tests::init_config();
    for jam_controller in [false, true] {
        let base = NEXT_LOCK.fetch_add(10, Ordering::Relaxed) as i64;
        let (ship, owner, drone, target) = (base, base + 1, base + 2, base + 3);
        let mut s = System::new(base, Arc::default(), 1000);
        let ball = Ball {
            id: ship, type_id: 1, owner_id: owner, corp_id: 1, alliance_id: 1,
            char_id: Some(owner), signature_radius: Some(1.0), mode: MODE_STOP, radius: 1.0,
            mass: 1.0, max_vel: 0.0, agility: 1.0, speed_fraction: 0.0,
            pos: [0.0; 3], vel: [0.0; 3], old_pos: [0.0; 3], old_vel: [0.0; 3],
            segment_acceleration: None, goto: [0.0; 3], follow_id: 0, follow_range: 0.0,
            free: false, global: false, massive: true, interactive: true,
            warp: WarpState::default(), last_ms: 0, interpolation_anchor_ms: 0,
            effect_stamp: 0, cloak: 0, cloak_until: 0, health: [1.0; 3], launcher: 0, slim: Vec::new(),
        };
        s.insert(ball.clone());
        let mut drone_ball = ball.clone(); drone_ball.id = drone; drone_ball.char_id = None;
        let mut victim = ball; victim.id = target; victim.char_id = None; victim.owner_id = target;
        victim.pos[0] = 20.0; victim.old_pos = victim.pos;
        let spec = crate::space::drone::DroneSpec {
            type_id: 1, bandwidth: 5.0, scan_resolution: 0.0, control_range: 10000.0,
            max_target_range: 0.0, optimal_range: 1000.0, orbit_range: 500.0,
            duration_ms: 1000.0, effect_id: Some(1),
        };
        s.insert(victim.clone());
        s.insert(drone_ball);
        s.register_drone(target, spec.clone(), Arc::new(|_, _| Some(Box::pin(async {}))), None, None, false);
        s.register_drone(drone, spec.clone(), Arc::new(|_, _| Some(Box::pin(async {}))), Some(ship), Some(owner), false);
        assert!(s.drone_order(drone, owner, ship, crate::space::drone::DroneOrder::Engage(target)));
        s.tick(1100);
        let before = s.drone(drone).unwrap();
        assert!(s.drone_valid(drone, target, before.generation));
        use crate::services::combat::groups_health::NpcCapacitorGuard;
        let assistance = NpcCapacitorGuard::DroneAssistance {
            source: drone, target, generation: before.generation, optimal: 20.0, falloff: 0.0, owner,
        };
        assert!(assistance.valid(&s));
        assert!(!NpcCapacitorGuard::DroneAssistance {
            source: drone, target, generation: before.generation, optimal: 1.0, falloff: 0.0, owner,
        }.valid(&s));
        assert!(!NpcCapacitorGuard::DroneAssistance {
            source: drone, target, generation: before.generation, optimal: 20.0, falloff: 0.0, owner: owner + 100,
        }.valid(&s));
        apply(&mut s, target, target, if jam_controller { ship } else { drone }, 100.0);
        let after = s.drone(drone).unwrap();
        assert_ne!(after.generation, before.generation);
        assert_eq!((after.owner, after.controller), (owner, Some(ship)));
        assert!(!s.drone_valid(drone, target, before.generation));
        assert!(!assistance.valid(&s));
        clear(&mut s, if jam_controller { ship } else { drone });
    }
}
