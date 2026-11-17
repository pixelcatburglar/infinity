//! Health and capacitor boundary tests without a database or running system actor.
use super::*;

#[cfg(feature = "tests-combat")] mod db_smoke;
#[cfg(feature = "tests-combat")] mod rewards;

#[cfg(any(feature = "tests-combat", feature = "tests-effects"))]
/// Drain up to the requested amount for capacitor integration tests.
pub(in crate::services) async fn drain_exact(db: &Db, world: &World, system: i64, ship: i64, amount: f64) -> Result<f64, CallError> {
    Ok(capacitor_operation(db, world, system, ship, Operation::Drain(amount), None, None).await?.map_or(0.0, |(_, amount, _)| amount))
}

#[cfg(feature = "tests-ships")]
pub(in crate::services) fn replace_capacitor_test_entry(ship: i64, entry: Option<(f64, i64)>) -> Option<(f64, i64)> {
    let mut state = state();
    match entry {
        Some(entry) => state.capacitor.insert(ship, entry),
        None => state.capacitor.remove(&ship),
    }
}

#[cfg(feature = "tests-ships")]
pub(in crate::services) fn capacitor_snapshot_for_capture(ship: i64, capacity: f64, recharge_tau_ms: f64) -> (i64, f64) {
    let mut st = state();
    let now = filetime_now();
    let result = mutate(&mut st.capacitor, ship, capacity, recharge_tau_ms, now, Operation::Snapshot);
    (now, result.after)
}

#[cfg(feature = "tests-combat")] #[test]
fn recharge_uses_existing_curve_and_never_runs_backwards() {
    let level = recharge(25.0, 100.0, 1000.0, 1.0);
    assert!((level - 100.0 * (1.0 - 0.5 * (-5.0_f64).exp()).powi(2)).abs() < 1e-9);
    assert_eq!(recharge(25.0, 100.0, 1000.0, -1.0), 25.0);
}

#[cfg(feature = "tests-combat")] #[test]
fn injection_and_capacity_are_clamped() {
    let mut entries = HashMap::new();
    entries.insert(1, (80.0, 0));
    let result = mutate(&mut entries, 1, 100.0, 1000.0, 0, Operation::Inject(50.0));
    assert_eq!((result.after, result.amount), (100.0, 20.0));
    let result = mutate(&mut entries, 1, 50.0, 1000.0, 0, Operation::Inject(-5.0));
    assert_eq!((result.after, result.amount), (50.0, 0.0));
    let result = mutate(&mut entries, 1, -1.0, 1000.0, 0, Operation::Snapshot);
    assert_eq!(result.after, 0.0);
}

#[cfg(feature = "tests-combat")] #[test]
fn drone_assistance_rejects_missing_or_self_source() {
    let system = crate::space::System::new(1, std::sync::Arc::default(), 0);
    for target in [1, 2] {
        assert!(!NpcCapacitorGuard::DroneAssistance {
            source: 1, target, generation: 3, optimal: 100.0, falloff: 0.0, owner: 4,
        }.valid(&system));
    }
}

#[cfg(feature = "tests-combat")] #[test]
fn drain_is_partial_and_negative_input_cannot_add_energy() {
    let mut entries = HashMap::new();
    entries.insert(1, (30.0, 0));
    let result = mutate(&mut entries, 1, 100.0, 1000.0, 0, Operation::Drain(50.0));
    assert_eq!(result.after, 0.0);
    // The recharge curve's sqrt/square round trip can introduce rounding at dt = 0.
    assert!((result.amount - 30.0).abs() < 1e-12);
    let result = mutate(&mut entries, 1, 100.0, 1000.0, 0, Operation::Drain(-10.0));
    assert_eq!((result.after, result.amount), (0.0, 0.0));
}

#[cfg(feature = "tests-combat")] #[test]
fn warp_debit_uses_shared_recharged_balance_and_partial_drain() {
    let ship = -920_202;
    state().capacitor.insert(ship, (25.0, FILETIME_SECOND));
    let debit = drain_warp_capacitor(ship, 100.0, 1000.0, 40.0, FILETIME_SECOND);
    assert_eq!(debit.before, 25.0);
    assert_eq!(debit.after, 0.0);
    assert_eq!(debit.drained, 25.0);
    let empty = drain_warp_capacitor(ship, 100.0, 1000.0, 1.0, FILETIME_SECOND);
    assert_eq!((empty.before, empty.after, empty.drained), (0.0, 0.0, 0.0));
    state().capacitor.remove(&ship);
}

#[cfg(feature = "tests-combat")] #[test]
fn warp_debit_and_module_operations_share_one_balance() {
    let ship = -920_203;
    state().capacitor.insert(ship, (100.0, FILETIME_SECOND));
    let first = drain_warp_capacitor(ship, 100.0, 1000.0, 40.0, FILETIME_SECOND);
    assert_eq!((first.before, first.after, first.drained), (100.0, 60.0, 40.0));
    let mut st = state();
    let injected = mutate(&mut st.capacitor, ship, 100.0, 1000.0, FILETIME_SECOND, Operation::Inject(20.0));
    assert!((injected.before - 60.0).abs() < 1e-12);
    assert!((injected.after - 80.0).abs() < 1e-12);
    let drained = mutate(&mut st.capacitor, ship, 100.0, 1000.0, FILETIME_SECOND, Operation::Drain(30.0));
    assert!((drained.before - 80.0).abs() < 1e-12);
    assert!((drained.after - 50.0).abs() < 1e-12);
    st.capacitor.remove(&ship);
}


#[cfg(feature = "tests-combat")] #[test]
fn concurrent_warp_and_module_debits_cannot_double_spend() {
    let ship = -920_204;
    state().capacitor.insert(ship, (100.0, FILETIME_SECOND));
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let worker_barrier = barrier.clone();
    let worker = std::thread::spawn(move || {
        worker_barrier.wait();
        drain_warp_capacitor(ship, 100.0, 1000.0, 80.0, FILETIME_SECOND).drained
    });
    barrier.wait();
    let module = {
        let mut st = state();
        mutate(&mut st.capacitor, ship, 100.0, 1000.0, FILETIME_SECOND, Operation::Drain(80.0)).amount
    };
    let warp = worker.join().unwrap();
    assert!((warp + module - 100.0).abs() < 1e-12);
    assert!(warp >= 0.0 && module >= 0.0);
    assert_eq!(state().capacitor.remove(&ship).unwrap().0, 0.0);
}
#[cfg(feature = "tests-combat")] #[test]
fn refused_cycle_drain_leaves_stored_state_untouched() {
    let mut entries = HashMap::new();
    entries.insert(1, (20.0, 0));
    let result = mutate(&mut entries, 1, 100.0, 1000.0, 0, Operation::RefuseDrain(30.0));
    assert_eq!(result.amount, 0.0);
    assert_eq!(entries.get(&1), Some(&(20.0, 0)));
}

#[cfg(feature = "tests-combat")] #[test]
fn fitting_capacitor_rebases_old_curve_and_preserves_depletion() {
    let mut entries = HashMap::new();
    entries.insert(1, (25.0, 0));
    let (old, new) = rebase_capacitor_at(&mut entries, 1, 100.0, 10_000.0, 200.0, FILETIME_SECOND);
    let expected = recharge(25.0, 100.0, 10_000.0, 1.0);
    assert_eq!((old, new), (expected, expected));
    assert_eq!(entries[&1], (expected, FILETIME_SECOND));
    let (old, new) = rebase_capacitor_at(&mut entries, 1, 200.0, 20_000.0, 10.0, FILETIME_SECOND);
    assert!(old > 10.0);
    assert_eq!(new, 10.0);
    assert_eq!(rebase_capacitor_at(&mut entries, 2, 100.0, 1000.0, 200.0, 0), (100.0, 100.0));
    entries.insert(3, (0.0, 0));
    assert_eq!(rebase_capacitor_at(&mut entries, 3, 100.0, 1000.0, 200.0, 0), (0.0, 0.0));
}

#[cfg(feature = "tests-combat")] #[test]
fn actor_fitting_publication_keeps_intervening_capacitor_spend() {
    use crate::services::fit::attributes::tests::ship_fit;
    use crate::space::{Ball, System, MODE_STOP};
    use std::sync::Arc;
    crate::tests::init_config();
    let ship = -920_201;
    let before = ship_fit(&[]);
    let patch = ship_fit(&[(ATTR_CAPACITOR_CAPACITY, 200.0), (ATTR_RECHARGE_RATE, 2000.0)])
        .into_committed_fitting(&before);
    let registry = Arc::new(crate::space::Pilots::default());
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    registry.register_pilot(2, tx);
    let mut system = System::new(1, registry, 0);
    system.add_persistent_ship(Ball { id: ship, type_id: 1, owner_id: 2, corp_id: 1,
        alliance_id: -1, char_id: Some(2), signature_radius: None, mode: MODE_STOP,
        radius: 1.0, mass: 1.0, max_vel: 0.0, agility: 1.0, speed_fraction: 0.0,
        pos: [0.0; 3], vel: [0.0; 3], old_pos: [0.0; 3], old_vel: [0.0; 3],
        segment_acceleration: None, goto: [0.0; 3], follow_id: 0, follow_range: 0.0,
        free: true, global: false, massive: true, interactive: true, warp: Default::default(),
        last_ms: 0, interpolation_anchor_ms: 0, effect_stamp: 0, cloak: 0, cloak_until: 0,
        health: [1.0; 3], launcher: 0, slim: Vec::new() });
    let generation = system.persistent_generation(ship).unwrap();
    let stats = ShipStats::from_fit(&before, Key::Ship);
    system.install_condition(ship, Condition { now: stats.max, stats, shield_at: filetime_now() }, before);
    state().capacitor.insert(ship, (25.0, filetime_now() + FILETIME_SECOND));
    assert!(system.publish_fitting(ship, 2, generation, patch, vec![]));
    let (_, args) = rx.try_recv().unwrap();
    let [Value::List(rows)] = args.as_slice() else { panic!("expected batch") };
    let index = rows.iter().position(|row| {
        let Value::Tuple(fields) = row else { return false };
        fields[3].as_int() == Some(i64::from(ATTR_CAPACITOR_CAPACITY))
    }).unwrap();
    let Value::Tuple(fields) = &rows[index + 1] else { panic!("expected charge") };
    assert_eq!(fields[3].as_int(), Some(i64::from(ATTR_CHARGE)));
    assert_eq!(fields[5], Value::Float(25.0));
    assert_eq!(state().capacitor.remove(&ship).unwrap().0, 25.0);
}

#[cfg(feature = "tests-combat")] #[test]
fn super_weapon_guard_requires_live_generation_pilot_ball_and_module() {
    let mut s = crate::space::System::new(1, std::sync::Arc::default(), 0);
    let generation = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let module = -9_000_005_i64;
    let guard = NpcCapacitorGuard::SuperWeapon { source: 1, character: 7, module, generation: generation.clone() };
    assert!(!guard.valid(&s), "no pilot ball and no module activation yet");
    let mut b = crate::space::Ball { id: 1, type_id: 1, owner_id: 7, corp_id: 1, alliance_id: -1,
        char_id: Some(7), signature_radius: None, mode: crate::space::MODE_STOP, radius: 1.0, mass: 1e6,
        max_vel: 0.0, agility: 1.0, speed_fraction: 0.0, pos: [0.0; 3], vel: [0.0; 3],
        old_pos: [0.0; 3], old_vel: [0.0; 3], segment_acceleration: None, goto: [0.0; 3], follow_id: 0,
        follow_range: 0.0, free: true, global: false, interactive: true, massive: true,
        warp: crate::space::WarpState::default(), last_ms: 0, interpolation_anchor_ms: 0,
        effect_stamp: 0, cloak: 0, cloak_until: 0, health: [1.0; 3], launcher: 0, slim: Vec::new() };
    // Registering the pilot ball alone is not enough: the module activation
    // must carry the same stop generation.
    s.insert(b.clone());
    assert!(!guard.valid(&s));
    {
        let mut st = state();
        st.active.insert(module, ActiveModule { char_id: 7, stop: generation.clone(), forced_inactive: false });
    }
    assert!(guard.valid(&s));
    // A stale activation generation, a replaced module and a warping source
    // each refuse the warmup drain.
    generation.store(true, std::sync::atomic::Ordering::Relaxed);
    assert!(!guard.valid(&s));
    let wrong_module = NpcCapacitorGuard::SuperWeapon { source: 1, character: 7, module: -9_000_006, generation: generation.clone() };
    assert!(!wrong_module.valid(&s));
    b.mode = crate::space::MODE_WARP;
    s.insert(b);
    assert!(!guard.valid(&s));
    state().active.remove(&module);
}

#[cfg(feature = "tests-combat")] #[test]
fn super_weapon_warmup_neut_drains_once_scaled_by_resistance_and_signature() {
    use crate::services::fit::attributes::tests::ship_fit;
    use crate::space::{Ball, System, MODE_STOP};
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;
    crate::tests::init_config();
    let (source, victim, outside, module, character, victim_char) = (-920_311_i64, -920_312, -920_313, -920_314, 2_i64, 3_i64);
    let generation = Arc::new(AtomicBool::new(false));
    let resistance_attr = infinity_core::consts::dogma::ATTR_SHIELD_EM_DAMAGE_RESONANCE;
    let ball_at = |id: i64, x: f64, signature: Option<f64>, character: Option<i64>| Ball {
        id, type_id: 1, owner_id: character.unwrap_or(1), corp_id: 1, alliance_id: -1, char_id: character,
        signature_radius: signature, mode: MODE_STOP, radius: 1.0, mass: 1e6, max_vel: 0.0,
        agility: 1.0, speed_fraction: 0.0, pos: [x, 0.0, 0.0], vel: [0.0; 3], old_pos: [x, 0.0, 0.0],
        old_vel: [0.0; 3], segment_acceleration: None, goto: [0.0; 3], follow_id: 0, follow_range: 0.0,
        free: true, global: false, interactive: character.is_some(), massive: true,
        warp: Default::default(), last_ms: 0, interpolation_anchor_ms: 0, effect_stamp: 0,
        cloak: 0, cloak_until: 0, health: [1.0; 3], launcher: 0, slim: Vec::new() };
    let mut system = System::new(1, Arc::default(), 0);
    // The source pilot ball is also the guard's activation carrier.
    system.insert(ball_at(source, 0.0, None, Some(character)));
    system.insert(ball_at(victim, 50.0, Some(200.0), Some(victim_char)));
    system.insert(ball_at(outside, 200.0, None, None));
    // Fit the victim: capacitor 500, recharge tau 2s, 25% resistance.
    let before = ship_fit(&[]);
    let patch = ship_fit(&[(ATTR_CAPACITOR_CAPACITY, 500.0), (ATTR_RECHARGE_RATE, 2_000.0), (resistance_attr, 0.25)])
        .into_committed_fitting(&before);
    let victim_generation = system.persistent_generation(victim).unwrap_or_else(|| {
        system.add_persistent_ship(ball_at(victim, 50.0, Some(200.0), Some(victim_char)));
        system.persistent_generation(victim).unwrap()
    });
    let stats = ShipStats::from_fit(&before, Key::Ship);
    system.install_condition(victim, Condition { now: stats.max, stats, shield_at: filetime_now() }, before);
    assert!(system.publish_fitting(victim, victim_char, victim_generation, patch, vec![]));
    state().capacitor.insert(victim, (500.0, filetime_now()));
    state().capacitor.insert(outside, (500.0, filetime_now()));
    {
        let mut st = state();
        st.active.insert(module, ActiveModule { char_id: character, stop: generation.clone(), forced_inactive: false });
    }
    let guard = NpcCapacitorGuard::SuperWeapon { source, character, module, generation: generation.clone() };
    let sde = crate::sde::Sde::default();
    drain_area_in_actor(&mut system, &sde, source, [0.0; 3], 100.0, 100.0, 400.0, resistance_attr, &guard);
    // amount(100) * signature factor(200/400) * (1 - resistance 0.25) = 37.5, applied once.
    assert_eq!(state().capacitor.get(&victim).unwrap().0, 462.5);
    assert_eq!(state().capacitor.get(&outside).unwrap().0, 500.0, "outside the neut radius");
    state().active.remove(&module);
    state().capacitor.remove(&victim);
    state().capacitor.remove(&outside);
}
