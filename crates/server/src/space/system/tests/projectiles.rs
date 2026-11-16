//! Damageable projectile lifecycle: live-SDE profile registration and
//! refusal, the shared typed absorber, every once-only terminal (contact,
//! expiry, invalidation, detonation, removal, lethal damage) and
//! incarnation fencing against stale timers and callbacks.
use super::*;
use crate::space::damageable::DamageableProjectile;
use crate::space::system::missiles::MissileImpact;
use crate::services::combat::groups_health::ShipStats;

fn projectile(id: i64) -> Ball {
    let mut b = ball(id, None, 0.0);
    b.massive = false;
    b.max_vel = 2_000.0;
    b
}

/// A live actor with the synthetic combat SDE: type 1 carries 100 structure
/// hit points and unit resonances; unknown types have no structure capacity.
fn live() -> System {
    let mut s = System::new(0, Arc::default(), 0);
    s.projection_sde = Some(crate::sde::types::tests::combat_fixture());
    s
}

fn channel() -> (std::sync::mpsc::Sender<Option<MissileImpact>>,
    std::sync::mpsc::Receiver<Option<MissileImpact>>) {
    std::sync::mpsc::channel()
}

#[test]
fn profile_rejects_invalid_stats_and_raw() {
    crate::tests::init_config();
    let stats = |max: [f64; 3], resonance: [[f64; 4]; 3]| ShipStats { max, resonance, shield_tau_ms: 0.0 };
    assert!(DamageableProjectile::from_stats(stats([0.0; 3], [[1.0; 4]; 3])).is_none());
    assert!(DamageableProjectile::from_stats(stats([0.0, 0.0, f64::INFINITY], [[1.0; 4]; 3])).is_none());
    assert!(DamageableProjectile::from_stats(stats([0.0, 0.0, 100.0],
        [[1.0; 4], [1.0; 4], [f64::NAN; 4]])).is_none());
    let sde = crate::sde::types::tests::combat_fixture();
    assert!(DamageableProjectile::from_sde(&sde, 1).is_some());
    assert!(DamageableProjectile::from_sde(&sde, 999).is_none());
    // Structure resonance flows through the shared absorber: 100 raw em at
    // 0.5 resonance removes 50 of the 100 structure hit points.
    let mut p = DamageableProjectile::from_stats(stats([0.0, 0.0, 100.0],
        [[1.0; 4], [1.0; 4], [0.5, 1.0, 1.0, 1.0]])).unwrap();
    assert!(p.apply([-1.0, 0.0, 0.0, 0.0]).is_none());
    assert!(p.apply([f64::NAN, 0.0, 0.0, 0.0]).is_none());
    let out = p.apply([100.0, 0.0, 0.0, 0.0]).unwrap();
    assert_eq!((out.dealt, out.remaining_structure, out.destroyed), (50.0, 50.0, false));
    let out = p.apply([0.0; 4]).unwrap();
    assert_eq!((out.dealt, out.remaining_structure, out.destroyed), (0.0, 50.0, false));
    let out = p.apply([200.0, 0.0, 0.0, 0.0]).unwrap();
    assert_eq!((out.dealt, out.remaining_structure, out.destroyed), (50.0, 0.0, true));
}

#[test]
fn live_launch_refuses_undamageable_charge() {
    crate::tests::init_config();
    let mut s = live();
    s.insert(ball(1, None, 0.0));
    s.insert(ball(2, None, 500_000.0));
    let mut charge = projectile(3);
    charge.type_id = 999;
    let (tx, rx) = channel();
    s.launch_missile(charge, 2, 1, 1_000.0, move |_, i| tx.send(i).unwrap());
    assert!(rx.try_recv().unwrap().is_none());
    assert!(!s.contains(3) && !s.projectile_damageable(3) && !s.missile_flights.contains_key(&3));
    let mut charge = projectile(4);
    charge.type_id = 999;
    let (tx, rx) = std::sync::mpsc::channel::<Option<V3>>();
    s.launch_bomb(charge, [1.0, 0.0, 0.0], 1_000, move |_, c| tx.send(c).unwrap());
    assert!(rx.try_recv().unwrap().is_none());
    assert!(!s.contains(4) && !s.projectile_damageable(4) && !s.bomb_flights.contains_key(&4));
}

#[test]
fn live_missile_registers_then_expiry_and_invalidation_each_end_once() {
    crate::tests::init_config();
    let mut s = live();
    s.insert(ball(1, None, 0.0));
    s.insert(ball(2, None, 500_000.0));
    let (tx, rx) = channel();
    s.launch_missile(projectile(3), 2, 1, 1_000.0, move |_, i| tx.send(i).unwrap());
    assert!(s.projectile_damageable(3) && s.missile_flights.contains_key(&3));
    s.tick(1_000);
    s.tick(2_000);
    assert!(rx.try_recv().unwrap().is_none());
    assert!(!s.projectile_damageable(3) && s.ball(3).is_none() && rx.try_recv().is_err());
    // Invalidation clears the capability once; the dead flight expires later.
    let (tx, rx) = channel();
    s.launch_missile(projectile(4), 2, 1, 50_000.0, move |_, i| tx.send(i).unwrap());
    s.tick(3_000);
    assert!(s.projectile_damageable(4));
    s.invalidate_missile_target(2);
    assert!(rx.try_recv().unwrap().is_none());
    assert!(!s.projectile_damageable(4) && s.ball(4).is_some());
    s.tick(53_000);
    assert!(s.ball(4).is_none() && rx.try_recv().is_err());
}

#[test]
fn bomb_delay_detonates_once_removal_and_lethal_damage_cancel() {
    crate::tests::init_config();
    let mut s = live();
    s.insert(ball(1, None, 0.0));
    let (tx, rx) = std::sync::mpsc::channel::<Option<V3>>();
    s.launch_bomb(projectile(4), [1.0, 0.0, 0.0], 1_000, move |_, c| tx.send(c).unwrap());
    assert!(s.projectile_damageable(4) && s.bomb_flights.contains_key(&4));
    s.tick(1_000);
    assert!(rx.try_recv().is_err() && s.ball(4).is_some());
    s.tick(2_000);
    let centre = rx.try_recv().unwrap().expect("one real detonation");
    assert!(centre.iter().all(|v| v.is_finite()) && centre[0] > 0.0);
    assert!(s.ball(4).is_none() && !s.projectile_damageable(4) && !s.bomb_flights.contains_key(&4));
    assert!(rx.try_recv().is_err());
    // Lethal damage consumes the same terminal once as a cancel.
    let mut s = live();
    s.insert(ball(1, None, 0.0));
    let (tx, rx) = std::sync::mpsc::channel::<Option<V3>>();
    s.launch_bomb(projectile(4), [1.0, 0.0, 0.0], 5_000, move |_, c| tx.send(c).unwrap());
    s.tick(1_000);
    let out = s.apply_projectile_damage(4, [0.0, 0.0, 100.0, 0.0]).unwrap();
    assert!(out.destroyed && rx.try_recv().unwrap().is_none());
    assert!(s.ball(4).is_none() && !s.projectile_damageable(4) && s.apply_projectile_damage(4, [1.0; 4]).is_none());
    s.tick(7_000);
    assert!(rx.try_recv().is_err());
    // Removal does the same and the armed timer becomes a no-op.
    let mut s = live();
    s.insert(ball(1, None, 0.0));
    let (tx, rx) = std::sync::mpsc::channel::<Option<V3>>();
    s.launch_bomb(projectile(4), [1.0, 0.0, 0.0], 1_000, move |_, c| tx.send(c).unwrap());
    s.tick(1_000);
    s.take(4);
    assert!(rx.try_recv().unwrap().is_none());
    s.tick(3_000);
    assert!(rx.try_recv().is_err());
    // A ball that is no projectile has no damageable profile.
    s.insert(ball(9, None, 0.0));
    assert!(s.apply_projectile_damage(9, [100.0; 4]).is_none() && s.ball(9).is_some());
}

#[test]
fn same_id_replacement_preserves_new_incarnation() {
    crate::tests::init_config();
    let mut s = live();
    s.insert(ball(1, None, 0.0));
    let (tx1, rx1) = std::sync::mpsc::channel::<Option<V3>>();
    s.launch_bomb(projectile(4), [1.0, 0.0, 0.0], 1_000, move |_, c| tx1.send(c).unwrap());
    s.tick(1_000);
    let (tx2, rx2) = std::sync::mpsc::channel::<Option<V3>>();
    s.launch_bomb(projectile(4), [1.0, 0.0, 0.0], 1_000, move |_, c| tx2.send(c).unwrap());
    // Replacement consumed the old terminal as a cancel, exactly once.
    assert!(rx1.try_recv().unwrap().is_none() && rx1.try_recv().is_err());
    assert!(s.projectile_damageable(4) && s.bomb_flights.contains_key(&4));
    // The old armed deadline passes as a no-op; only the new flight detonates.
    s.tick(2_000);
    assert!(rx1.try_recv().is_err() && rx2.try_recv().is_err());
    s.tick(3_000);
    assert!(rx2.try_recv().unwrap().is_some());
    assert!(rx1.try_recv().is_err() && rx2.try_recv().is_err());
}

#[test]
fn lethal_damage_ends_missile_without_combat_record() {
    crate::tests::init_config();
    let mut s = live();
    s.insert(ball(1, None, 0.0));
    s.insert(ball(2, None, 500_000.0));
    let (tx, rx) = channel();
    s.launch_missile(projectile(3), 2, 1, 50_000.0, move |_, i| tx.send(i).unwrap());
    s.tick(1_000);
    assert!(s.apply_projectile_damage(3, [-1.0; 4]).is_none() && s.projectile_damageable(3));
    let out = s.apply_projectile_damage(3, [40.0, 0.0, 0.0, 0.0]).unwrap();
    assert_eq!((out.dealt, out.remaining_structure, out.destroyed), (40.0, 60.0, false));
    assert!(s.ball(3).is_some() && rx.try_recv().is_err());
    let out = s.apply_projectile_damage(3, [0.0, 0.0, 100.0, 0.0]).unwrap();
    assert_eq!((out.dealt, out.remaining_structure), (60.0, 0.0));
    assert!(out.destroyed);
    assert!(rx.try_recv().unwrap().is_none());
    assert!(s.ball(3).is_none() && !s.projectile_damageable(3) && rx.try_recv().is_err());
    assert!(!s.actor_combat.contains_key(&3) && s.apply_projectile_damage(3, [1.0; 4]).is_none());
    // Lethal damage of a live missile consumes its terminal the same way.
    let (tx, rx) = channel();
    s.launch_missile(projectile(4), 2, 1, 50_000.0, move |_, i| tx.send(i).unwrap());
    s.tick(2_000);
    assert!(s.apply_projectile_damage(4, [0.0, 0.0, 100.0, 0.0]).unwrap().destroyed && rx.try_recv().unwrap().is_none());
    assert!(s.ball(4).is_none() && !s.projectile_damageable(4));
}
