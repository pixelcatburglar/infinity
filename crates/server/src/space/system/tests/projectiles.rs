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
    s.launch_bomb(charge, [1.0, 0.0, 0.0], 1_000, 0.0, move |_, c| tx.send(c).unwrap());
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
    s.launch_bomb(projectile(4), [1.0, 0.0, 0.0], 1_000, 0.0, move |_, c| tx.send(c).unwrap());
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
    s.launch_bomb(projectile(4), [1.0, 0.0, 0.0], 5_000, 0.0, move |_, c| tx.send(c).unwrap());
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
    s.launch_bomb(projectile(4), [1.0, 0.0, 0.0], 1_000, 0.0, move |_, c| tx.send(c).unwrap());
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
    s.launch_bomb(projectile(4), [1.0, 0.0, 0.0], 1_000, 0.0, move |_, c| tx1.send(c).unwrap());
    s.tick(1_000);
    let (tx2, rx2) = std::sync::mpsc::channel::<Option<V3>>();
    s.launch_bomb(projectile(4), [1.0, 0.0, 0.0], 1_000, 0.0, move |_, c| tx2.send(c).unwrap());
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

/// An online session for `character` in `corporation`; the returned receiver
/// must stay alive for the session to read as online.
fn online(s: &System, character: i64, corporation: i64)
    -> tokio::sync::mpsc::UnboundedReceiver<(String, Vec<marshal::Value>)> {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    s.registry.register_pilot(character, tx);
    s.registry.set_online_session(character, Some(infinity_core::session::OnlineSession {
        character_id: character, user_id: None, session_id: character, ship_id: None,
        station_id: None, system_id: Some(0), corporation_id: Some(corporation),
        alliance_id: None, faction_id: None, fleet_id: None, role: 0, corporation_role: 0,
    }));
    rx
}

fn bomb_at(id: i64, owner: i64, pos: V3) -> Ball {
    let mut b = projectile(id);
    b.owner_id = owner;
    b.pos = pos;
    b
}

const SOURCE_CORP: i64 = 100;
const HOSTILE_CORP: i64 = 7;

#[test]
fn defender_selects_and_ends_only_the_threatening_bomb() {
    let mut s = live();
    s.insert(ball(1, Some(1), 0.0));
    let _source = online(&s, 1, SOURCE_CORP);
    let _hostile = online(&s, 7, HOSTILE_CORP);
    // Same corporation as the source: friendly, never a target.
    let _friend = online(&s, 8, SOURCE_CORP);
    let (bomb_tx, bomb_rx) = std::sync::mpsc::channel::<Option<V3>>();
    // Covering and in reach: the only legal target.
    s.launch_bomb(bomb_at(2, 7, [-4_000.0, 0.0, 0.0]), [1.0, 0.0, 0.0], 60_000, 500.0,
        move |_, c| bomb_tx.send(c).unwrap());
    // In reach but its path never crosses the source.
    s.launch_bomb(bomb_at(3, 7, [2_000.0, 3_000.0, 0.0]), [1.0, 0.0, 0.0], 60_000, 500.0, |_, _| {});
    // Friendly covering bomb.
    s.launch_bomb(bomb_at(4, 8, [-2_000.0, 0.0, 0.0]), [1.0, 0.0, 0.0], 60_000, 500.0, |_, _| {});
    // Covering but beyond the Defender's reach.
    s.launch_bomb(bomb_at(5, 7, [-50_000.0, 0.0, 0.0]), [1.0, 0.0, 0.0], 60_000, 500.0, |_, _| {});
    // An ordinary hostile missile is never a Defender target.
    s.launch_missile(projectile(6), 1, 5, 50_000.0, |_, _| {});
    s.tick(1_000); s.tick(2_000);
    let (picked, token) = s.reserve_defender_target(1, 10_000.0).expect("one legal bomb");
    assert_eq!(picked, 2);
    assert!(s.defender_reservations.contains(&2));
    let (tx, rx) = channel();
    s.launch_defender(projectile(9), 2, token, 1, 10_000.0, move |_, hit| tx.send(hit).unwrap());
    let mut hit = None;
    for ms in [3_000_i64, 4_000, 5_000, 6_000] {
        s.tick(ms);
        if let Ok(done) = rx.try_recv() { hit = Some(done); break; }
    }
    assert_eq!(hit.expect("Defender terminal ran").expect("contact reports the target").target, 2);
    assert!(bomb_rx.try_recv().unwrap().is_none(), "contact cancels, never detonates");
    assert!(s.ball(2).is_none() && !s.projectile_damageable(2));
    assert!(s.defender_reservations.is_empty());
    assert!(!s.actor_combat.contains_key(&2) && !s.actor_combat.contains_key(&9));
}

#[test]
fn defender_source_without_session_selects_nothing() {
    let mut s = live();
    s.insert(ball(1, Some(1), 0.0));
    let _hostile = online(&s, 7, HOSTILE_CORP);
    s.launch_bomb(bomb_at(2, 7, [-4_000.0, 0.0, 0.0]), [1.0, 0.0, 0.0], 60_000, 500.0, |_, _| {});
    s.tick(1_000); s.tick(2_000);
    assert!(s.reserve_defender_target(1, 10_000.0).is_none() && s.defender_reservations.is_empty());
}

#[test]
fn defender_expiry_releases_reservation_and_bomb_keeps_flying() {
    let mut s = live();
    s.insert(ball(1, Some(1), 0.0));
    let _source = online(&s, 1, SOURCE_CORP);
    let _hostile = online(&s, 7, HOSTILE_CORP);
    let mut slow = bomb_at(2, 7, [-4_000.0, 0.0, 0.0]);
    slow.max_vel = 500.0;
    s.launch_bomb(slow, [1.0, 0.0, 0.0], 60_000, 500.0, |_, _| {});
    s.tick(1_000); s.tick(2_000);
    let (picked, token) = s.reserve_defender_target(1, 10_000.0).unwrap();
    assert_eq!(picked, 2);
    let (tx, rx) = channel();
    s.launch_defender(projectile(9), 2, token, 1, 400.0, move |_, hit| tx.send(hit).unwrap());
    s.tick(3_000); s.tick(4_000); s.tick(5_000);
    assert!(rx.try_recv().unwrap().is_none());
    assert!(s.defender_reservations.is_empty() && s.ball(2).is_some() && s.ball(9).is_none());
}

#[test]
fn defender_lethal_damage_releases_reservation() {
    let mut s = live();
    s.insert(ball(1, Some(1), 0.0));
    let _source = online(&s, 1, SOURCE_CORP);
    let _hostile = online(&s, 7, HOSTILE_CORP);
    s.launch_bomb(bomb_at(2, 7, [-4_000.0, 0.0, 0.0]), [1.0, 0.0, 0.0], 60_000, 500.0, |_, _| {});
    s.tick(1_000); s.tick(2_000);
    let (picked, token) = s.reserve_defender_target(1, 10_000.0).unwrap();
    assert_eq!(picked, 2);
    let (tx, rx) = channel();
    s.launch_defender(projectile(9), 2, token, 1, 10_000.0, move |_, hit| tx.send(hit).unwrap());
    assert!(s.apply_projectile_damage(9, [0.0, 0.0, 100.0, 0.0]).unwrap().destroyed);
    assert!(rx.try_recv().unwrap().is_none());
    s.tick(3_000);
    assert!(s.defender_reservations.is_empty() && s.ball(2).is_some() && !s.actor_combat.contains_key(&9));
}

#[test]
fn defender_target_departure_releases_reservation() {
    let mut s = live();
    s.insert(ball(1, Some(1), 0.0));
    let _source = online(&s, 1, SOURCE_CORP);
    let _hostile = online(&s, 7, HOSTILE_CORP);
    s.launch_bomb(bomb_at(2, 7, [-4_000.0, 0.0, 0.0]), [1.0, 0.0, 0.0], 60_000, 500.0, |_, _| {});
    s.tick(1_000); s.tick(2_000);
    let (picked, _) = s.reserve_defender_target(1, 10_000.0).unwrap();
    assert_eq!(picked, 2);
    s.take(2);
    assert!(s.defender_reservations.is_empty());
}

#[test]
fn defender_stale_token_cannot_end_replacement() {
    let mut s = live();
    s.insert(ball(1, Some(1), 0.0));
    let _source = online(&s, 1, SOURCE_CORP);
    let _hostile = online(&s, 7, HOSTILE_CORP);
    s.launch_bomb(bomb_at(2, 7, [-4_000.0, 0.0, 0.0]), [1.0, 0.0, 0.0], 60_000, 500.0, |_, _| {});
    s.tick(1_000); s.tick(2_000);
    let (picked, token) = s.reserve_defender_target(1, 10_000.0).unwrap();
    assert_eq!(picked, 2);
    s.take(2);
    s.insert(ball(2, None, 5.0));
    s.defender_reservations.insert(2);
    let (tx, rx) = channel();
    s.launch_defender(projectile(9), 2, token, 1, 10_000.0, move |_, hit| tx.send(hit).unwrap());
    s.tick(3_000); s.tick(4_000);
    assert!(rx.try_recv().unwrap().is_none());
    assert!(s.ball(2).is_some() && s.defender_reservations.is_empty());
}
