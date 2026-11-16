//! Missile flight lifecycle observed through launch, ticks, ball state and the
//! one-shot finish callback: queued registration, target-only segment sweeps,
//! the shared contact/expiry resolver and permanent invalidation boundaries.

use super::*;
use crate::space::system::missiles::MissileImpact;

fn missile(id: i64) -> Ball {
    let mut b = ball(id, None, 0.0);
    b.massive = false;
    b.max_vel = 2_000.0;
    b
}

fn channel() -> (std::sync::mpsc::Sender<Option<MissileImpact>>,
    std::sync::mpsc::Receiver<Option<MissileImpact>>) {
    std::sync::mpsc::channel()
}

#[test]
fn old_expiry_does_not_end_replacement_flight() {
    crate::tests::init_config();
    let mut s = System::new(0, Arc::default(), 0);
    s.insert(ball(1, None, 0.0)); s.insert(ball(2, None, 500_000.0));
    s.launch_missile(missile(3), 2, 1, 1_000.0, |_, _| {}); s.tick(1_000);
    s.launch_missile(missile(3), 2, 1, 50_000.0, |_, _| {});
    let (tx, mut late) = mpsc::unbounded_channel();
    let (completed, mut done) = tokio::sync::oneshot::channel();
    assert!(s.observe(7, 1, tx, completed));
    s.apply_scene_prefix(false);
    assert_eq!(done.try_recv(), Ok(true));
    let state = super::events::initial_state(&mut late);
    let bag = &state.items().unwrap()[1].items().unwrap()[1].items().unwrap()[0];
    let slims = crate::services::value_field(bag, "slims").unwrap().items().unwrap();
    assert!(slims.iter().any(|slim| crate::services::value_field(slim, "itemID").and_then(Value::as_int) == Some(3)));
    let successor = s.ball_token(3).unwrap();
    s.tick(2_000);
    assert!(late.try_recv().is_err());
    assert!(s.ball_token(3) == Some(successor));
    assert!(s.ball(3).is_some());
    assert!(s.missile_flights.contains_key(&3), "the stale expiry cannot retire the successor flight");
    s.tick(3_000);
    assert!(s.missile_flights.contains_key(&3) && s.ball(3).is_some(), "the successor flight still flies");
}

/// Contact fraction of the just-completed segment for fixture balls 3 and 2.
fn segment_fraction(s: &System) -> f64 {
    let (missile, target) = (s.ball(3).unwrap(), s.ball(2).unwrap());
    collision::collide_two_spheres(missile.old_pos, missile.pos,
        target.old_pos, target.pos, f64::from(missile.radius) + f64::from(target.radius))
        .expect("contact on completed segment")
}

#[test]
fn contact_sweeps_only_the_target_and_reports_impact() {
    crate::tests::init_config();
    for collisions in [false, true] {
        for (target_x, blocker) in [(100.0, false), (100.0, true), (0.5, false)] {
            let mut s = System::new(0, Arc::default(), 0);
            s.insert(ball(1, None, 0.0));
            s.insert(ball(2, None, target_x));
            if blocker {
                let mut b = ball(9, None, 50.0);
                b.radius = 40.0;
                s.insert(b);
            }
            let (tx, rx) = channel();
            s.launch_missile(missile(3), 2, 1, 20_000.0, move |_, impact| tx.send(impact).unwrap());
            let mut seen = None;
            for ms in [1_000_i64, 1_500, 2_000] {
                s.tick_with_collisions(ms, collisions);
                if let Ok(impact) = rx.try_recv() { seen = Some(impact); break; }
            }
            let impact = seen.flatten().expect("contact must be recorded");
            assert_eq!(impact.target, 2);
            assert!(s.ball(3).is_none());
        }
    }
}

/// Derive the expected offset from the first completed launch segment.
fn expected_elapsed() -> f64 {
    let mut s = System::new(0, Arc::default(), 0);
    s.insert(ball(1, None, 0.0));
    s.insert(ball(2, None, 100.0));
    let (tx, rx) = channel();
    s.launch_missile(missile(3), 2, 1, 50_000.0, move |_, i| tx.send(i).unwrap());
    s.tick(1_000);
    assert!(rx.try_recv().is_err());
    seconds_to_ms_f64(segment_fraction(&s))
}

#[test]
fn strict_lifetime_bounds_and_shared_deadline_resolver() {
    crate::tests::init_config();
    let elapsed = expected_elapsed();
    assert!(elapsed > 0.0 && elapsed < 1_000.0 && elapsed.fract() != 0.0);
    let same_ceil = elapsed.ceil();
    for (lifetime, expect_hit) in [(same_ceil, true), (elapsed, false), (elapsed - 0.5, false)] {
        let mut s = System::new(0, Arc::default(), 0);
        s.insert(ball(1, None, 0.0));
        s.insert(ball(2, None, 100.0));
        let (tx, rx) = channel();
        s.launch_missile(missile(3), 2, 1, lifetime, move |_, i| tx.send(i).unwrap());
        let mut seen = None;
        for ms in [1_000_i64, 1_500, 2_000] {
            s.tick(ms);
            if let Ok(impact) = rx.try_recv() { seen = Some(impact); break; }
        }
        let terminal = seen.expect("one terminal callback");
        if expect_hit {
            assert_eq!(terminal.expect("same-deadline contact must win").target, 2);
        } else {
            assert!(terminal.is_none());
        }
        assert!(s.ball(3).is_none());
        assert!(rx.try_recv().is_err());
    }
}

#[test]
fn invalidation_is_permanent_for_target_and_source() {
    crate::tests::init_config();
    // A flight invalidated while queued never regains its target, applies
    // untargeted and keeps its visual ball until the frozen expiry.
    let mut s = System::new(0, Arc::default(), 0);
    s.insert(ball(1, None, 0.0));
    s.insert(ball(2, None, 10.0));
    let (tx, rx) = channel();
    s.launch_missile(missile(3), 2, 1, 50_000.0, move |_, i| tx.send(i).unwrap());
    s.take(2);
    assert!(rx.try_recv().unwrap().is_none());
    s.insert(ball(2, None, 10.0));
    s.tick(1_000);
    assert_eq!(s.ball(3).unwrap().mode, MODE_GOTO);
    assert!(rx.try_recv().is_err());
    s.tick(51_000);
    assert!(s.ball(3).is_none());
    assert!(rx.try_recv().is_err());
    let mut s = System::new(0, Arc::default(), 0);
    s.insert(ball(1, None, 0.0));
    let mut target = ball(2, None, 500_000.0);
    target.mode = MODE_GOTO;
    target.speed_fraction = 1.0;
    target.goto = [1e12, 0.0, 0.0];
    target.vel = [100.0, 0.0, 0.0];
    s.insert(target);
    let (tx, rx) = channel();
    s.launch_missile(missile(3), 2, 1, 50_000.0, move |_, i| tx.send(i).unwrap());
    s.tick(1_000);
    s.command(2, |b, _, _| { b.warp_to([1e12, 0.0, 0.0], 0.0, 3000); vec![] });
    assert!(rx.try_recv().is_err());
    s.tick(2_000);
    assert!(s.ball(2).unwrap().is_warping());
    assert!(rx.try_recv().unwrap().is_none());
    assert_eq!(s.ball(3).unwrap().mode, MODE_GOTO);
    let mut s = System::new(0, Arc::default(), 0);
    s.insert(ball(1, None, 0.0));
    s.insert(ball(2, None, 500_000.0));
    let (tx, rx) = channel();
    s.launch_missile(missile(3), 2, 1, 50_000.0, move |_, i| tx.send(i).unwrap());
    s.tick(1_000);
    let generation = Arc::new(std::sync::atomic::AtomicBool::new(false));
    s.begin_module_cloak(2, 99, generation, 0, 0, |_| false, None);
    s.tick(2_000);
    assert_eq!(s.ball(3).unwrap().mode, MODE_MISSILE);
    assert!(rx.try_recv().is_err());
    let generation = Arc::new(std::sync::atomic::AtomicBool::new(false));
    assert!(s.begin_module_cloak(2, 99, generation, 0, 0, |_| true, None));
    s.tick(3_000);
    assert_eq!(s.ball(2).unwrap().cloak, 1);
    assert!(rx.try_recv().unwrap().is_none());
    // Removing the launcher kills its flight; the missile keeps flying straight.
    let mut s = System::new(0, Arc::default(), 0);
    s.insert(ball(1, None, 0.0));
    s.insert(ball(2, None, 500_000.0));
    let (tx, rx) = channel();
    s.launch_missile(missile(3), 2, 1, 50_000.0, move |_, i| tx.send(i).unwrap());
    let (tx2, rx2) = channel();
    s.launch_missile(missile(4), 2, 1, 50_000.0, move |_, i| tx2.send(i).unwrap());
    s.tick(1_000);
    s.destroy_ball(4);
    assert!(rx2.try_recv().unwrap().is_none());
    s.take(1);
    assert!(rx.try_recv().unwrap().is_none());
    s.tick(2_000);
    assert_eq!(s.ball(3).unwrap().mode, MODE_GOTO);
    assert!(rx2.try_recv().is_err());
    let mut s = System::new(0, Arc::default(), 0);
    s.insert(ball(1, None, 0.0));
    s.insert(ball(2, None, 100.0));
    let (tx, rx) = channel();
    s.launch_missile(missile(3), 2, 1, 50_000.0, move |_, i| tx.send(i).unwrap());
    s.tick(1_000); // same sweep as the proven contact, before its deadline
    s.invalidate_missile_target(2);
    assert!(rx.try_recv().unwrap().is_none());
    s.tick(2_000);
    assert!(s.ball(3).is_some());
    assert!(rx.try_recv().is_err());
}

#[test]
fn late_catchup_applies_launch_at_real_time_without_history() {
    crate::tests::init_config();
    let mut s = System::new(0, Arc::default(), 0);
    s.insert(ball(1, None, 0.0));
    s.insert(ball(2, None, 100.0));
    let (tx, rx) = channel();
    s.launch_missile(missile(3), 2, 1, 50_000.0, move |_, i| tx.send(i).unwrap());
    // Beyond the five-step clamp the single realigned step anchors to now; the
    // raw offset therefore stays inside the post-application second.
    s.tick(20_000);
    let elapsed = seconds_to_ms_f64(segment_fraction(&s));
    assert!(elapsed > 0.0 && elapsed < 1_000.0 && rx.try_recv().is_err());
    let mut seen = None;
    for ms in [20_100_i64, 20_500, 21_000] {
        s.tick(ms);
        if let Ok(impact) = rx.try_recv() { seen = Some(impact); break; }
    }
    let impact = seen.flatten().expect("post-launch contact expected");
    assert_eq!(impact.target, 2);
    let mut s = System::new(0, Arc::default(), 0);
    s.insert(ball(1, None, 0.0));
    s.insert(ball(2, None, 3_000.0));
    let (tx, rx) = channel();
    s.launch_missile(missile(3), 2, 1, 50_000.0, move |_, i| tx.send(i).unwrap());
    s.tick(1_000);
    s.tick(6_000);
    assert!(rx.try_recv().unwrap().is_some());
}

#[test]
fn committed_contact_is_final_and_carries_exact_fraction_velocity() {
    crate::tests::init_config();
    let mut s = System::new(0, Arc::default(), 0);
    s.insert(ball(1, None, 0.0));
    let mut target = ball(2, None, 80.0);
    target.max_vel = 500.0;
    target.speed_fraction = 1.0;
    target.mode = MODE_GOTO;
    target.goto = [1e6, 0.0, 0.0];
    s.insert(target);
    let (tx, rx) = channel();
    s.launch_missile(missile(3), 2, 1, 60_000.0, move |_, i| tx.send(i).unwrap());
    s.tick(1_000); // applies at the whole-second anchor, so the segment owns the contact
    let fraction = segment_fraction(&s);
    assert!(fraction > 0.05 && fraction < 1.0);
    let b = s.ball(2).unwrap();
    let expected = integrate(b.old_pos, b.old_vel, b.segment_acceleration.unwrap(),
        b.mass * b.agility as f64, DESTINY_FRICTION, 0.0, fraction).1;
    let endpoints = (b.old_vel, b.vel);
    let mut seen = None;
    for ms in [1_100_i64, 1_500] {
        s.tick(ms);
        if let Ok(impact) = rx.try_recv() { seen = Some(impact); break; }
    }
    let impact = seen.flatten().expect("impact");
    assert_eq!(impact.target_velocity, expected);
    assert_ne!(impact.target_velocity, endpoints.0);
    assert_ne!(impact.target_velocity, endpoints.1);
    // A committed impact is final: later departures cannot re-fire or revoke it.
    s.take(2);
    s.take(1);
    s.tick(3_000);
    assert!(rx.try_recv().is_err());
}

#[test]
fn projectile_reference_events_keep_fifo_position() {
    crate::tests::init_config();
    let collisions = crate::space::collision::enabled();
    let mut s = System::new(0, Arc::default(), 0);
    s.insert(ball(1, None, 0.0));
    s.insert(ball(2, None, 100_000.0));
    let mut rx = watch(&mut s, 1, 1);
    let (missile_tx, missile_rx) = channel();
    s.launch_missile(missile(3), 2, 1, 5_000.0, move |_, i| missile_tx.send(i).unwrap());
    s.launch_bomb(missile(4), [0.0, 1.0, 0.0], 50_000, |_, _| {});
    s.set_ball_position(3, [7.0, 0.0, 0.0], |_, _| true);
    s.actions.push_back((s.current_time, Box::new(move |s, _| s.add_ship(ball(6, None, 6.0)))));
    let before = crate::client::filetime_now();
    s.apply_scene_prefix(collisions);
    // Birth, launch and the later motion all keep their FIFO positions.
    // The birth Add carries the just-launched state, before the queued motion.
    let mut launched = missile(3);
    launched.vel = [1.0, 0.0, 0.0];
    launched.follow_id = 2;
    launched.follow_range = -2.0;
    launched.launcher = 1;
    launched.goto = [1e16, 0.0, 0.0]; // native initial missile GotoReach
    launched.speed_fraction = 1.0;
    launched.mode = MODE_MISSILE;
    let actual = super::events::destiny(rx.try_recv().unwrap());
    let mut expected = vec![crate::space::add_balls_event(0, &launched, collisions)];
    super::events::reuse_checked_damage_time(&actual[0], &mut expected[0], before..=crate::client::filetime_now());
    assert_eq!(actual, expected);
    assert_eq!(super::events::destiny(rx.try_recv().unwrap()), vec![event(0, "LaunchMissile", vec![
        Value::Int(3), Value::Int(2), Value::Int(1), Value::Bool(true), Value::Bool(false)])]);
    let actual = super::events::destiny(rx.try_recv().unwrap());
    let mut expected = vec![crate::space::add_balls_event(0, s.ball(4).unwrap(), collisions)];
    super::events::reuse_checked_damage_time(&actual[0], &mut expected[0], before..=crate::client::filetime_now());
    assert_eq!(actual, expected);
    assert_eq!(super::events::destiny(rx.try_recv().unwrap()), vec![
        event(0, "GotoDirection", vec![Value::Int(4), Value::Float(0.0), Value::Float(1.0), Value::Float(0.0)]),
        event(0, "SetSpeedFraction", vec![Value::Int(4), Value::Float(1.0)])]);
    assert_eq!(super::events::destiny(rx.try_recv().unwrap()), vec![event(0, "SetBallPosition", vec![
        Value::Int(3), Value::Float(7.0), Value::Float(0.0), Value::Float(0.0)])]);
    assert!(s.missile_flights.contains_key(&3), "the real Flight is registered");
    // The birth appended inside the prefix lands only at the next boundary.
    s.apply_scene_prefix(collisions);
    let actual = super::events::destiny(rx.try_recv().unwrap());
    let mut expected = vec![crate::space::add_balls_event(0, s.ball(6).unwrap(), collisions)];
    super::events::reuse_checked_damage_time(&actual[0], &mut expected[0], before..=crate::client::filetime_now());
    assert_eq!(actual, expected, "the deferred birth arrives with its own current state");
    // An observer's ordinary bubble exit never finishes a live Flight.
    let token3 = s.ball_token(3).unwrap();
    s.set_ball_position(1, [3.0 * crate::space::system::interest::BUBBLE_EDGE, 0.0, 0.0], |_, _| true);
    s.apply_scene_prefix(collisions);
    assert_eq!(super::events::destiny(rx.try_recv().unwrap()),
        vec![crate::space::system::tick::remove_balls_event(0, &[2, 3, 4, 6])]);
    assert!(s.missile_flights.contains_key(&3) && s.ball_token(3) == Some(token3));
    // Returning re-delivers the cohort's current state, missile included.
    s.set_ball_position(1, [0.0, 0.0, 0.0], |_, _| true);
    s.apply_scene_prefix(collisions);
    // The single observer lost every other bubble holder on the way out: the
    // return is one transfer Add carrying the cohort's current state.
    let cohort = [s.ball_token(2).unwrap(), token3, s.ball_token(4).unwrap(), s.ball_token(6).unwrap()];
    let actual = super::events::destiny(rx.try_recv().unwrap());
    let mut expected = vec![crate::space::system::tick::add_balls_batch(0, &s.balls, &cohort, collisions)];
    super::events::reuse_checked_damage_time(&actual[0], &mut expected[0], before..=crate::client::filetime_now());
    assert_eq!(actual, expected, "the return Add carries the live cohort's current state exactly once");
    assert!(rx.try_recv().is_err());
    assert!(s.missile_flights.contains_key(&3) && s.ball_token(3) == Some(token3));
    // The real expiry, not the observer's departure, ends the flight once.
    s.tick(5_000);
    assert!(missile_rx.try_recv().unwrap().is_none(), "expiry reports exactly one miss");
    assert!(!s.missile_flights.contains_key(&3) && s.ball(3).is_none());
    assert!(missile_rx.try_recv().is_err());
    // A zero-observer system still runs the real bomb timer to exactly one impact.
    let mut z = System::new(0, Arc::default(), 0);
    z.insert(ball(1, None, 0.0));
    let (impact_tx, impact_rx) = std::sync::mpsc::channel();
    z.launch_bomb(missile(4), [1.0, 0.0, 0.0], 1_000, move |_, center| { impact_tx.send(center.is_some()).unwrap(); });
    z.tick(1_000);
    z.tick(2_000);
    assert_eq!(impact_rx.try_recv(), Ok(true));
    z.tick(3_000);
    assert!(impact_rx.try_recv().is_err() && missile_rx.try_recv().is_err(),
        "one impact and one real expiry, each terminal exactly once");
}

/// A live-SDE system whose source ship carries a live combat record, the same
/// shape an admitted smartbomb source has when a pulse reaches admission.
fn live_source() -> System {
    let sde = crate::sde::types::tests::combat_fixture();
    let mut s = System::new(0, Arc::default(), 0);
    s.projection_sde = Some(sde.clone());
    s.insert(ball(1, None, 0.0));
    let fit = crate::services::fit::Fit::for_space_type(sde, 1, Vec::new());
    let stats = crate::services::combat::groups_health::ShipStats::from_fit(&fit, crate::services::fit::Key::Ship);
    s.install_condition(1, crate::services::combat::groups_health::Condition {
        now: stats.max, stats, shield_at: crate::client::filetime_now() }, fit);
    s
}

/// A projectile-only pulse admits its source once, then the in-range
/// damageable missile takes the shared capability's damage and dies without a
/// ship combat record; the out-of-range missile and the ledger stay untouched.
#[test]
fn projectile_only_area_pulse_damages_in_range_and_spares_out_of_range() {
    crate::tests::init_config();
    let mut s = live_source();
    let (tx, rx) = channel();
    s.launch_missile(missile(3), 1, 1, 60_000.0, move |_, i| tx.send(i).unwrap());
    let mut far = missile(4);
    far.pos = [500_000.0, 0.0, 0.0];
    s.launch_missile(far, 1, 1, 60_000.0, |_, _| {});
    let source = s.prepare_combat_source(1, None, None, Some(4),
        crate::lua::events::resources::FactOrigin::Natural,
        crate::space::targeting::next_generation(), 1, 1, |_| true).unwrap();
    s.observe_combat_utc().unwrap();
    let at = s.combat_observed_time();
    let projectiles: Vec<i64> = crate::services::effects::aoe::victims(&s, 1, [0.0; 3], 1_000.0)
        .into_iter().map(|victim| victim.0).filter(|&id| s.projectile_damageable(id)).collect();
    assert_eq!(projectiles, vec![3]);
    assert!(s.admit_area_source(source, &mut [], at, |_| true).unwrap());
    for id in projectiles { s.apply_projectile_damage(id, [200.0, 0.0, 0.0, 0.0]); }
    assert!(rx.try_recv().unwrap().is_none());
    assert!(s.ball(3).is_none() && !s.projectile_damageable(3) && s.projectile_damageable(4));
    assert!(!s.actor_combat.contains_key(&3) && !s.combat_facts.as_ref().unwrap().is_dirty()
        && !s.combat_facts.as_ref().unwrap().has_pending_attempts());
}

/// A refused or replayed source admission leaves every projectile unchanged;
/// the attempt is still consumed once.
#[test]
fn refused_or_replayed_area_pulse_leaves_projectile_unchanged() {
    crate::tests::init_config();
    let mut s = live_source();
    let (tx, rx) = channel();
    s.launch_missile(missile(3), 1, 1, 60_000.0, move |_, i| tx.send(i).unwrap());
    let source = s.prepare_combat_source(1, None, None, Some(4),
        crate::lua::events::resources::FactOrigin::Natural,
        crate::space::targeting::next_generation(), 1, 1, |_| true).unwrap();
    s.observe_combat_utc().unwrap();
    let at = s.combat_observed_time();
    assert_eq!(s.admit_area_source(source, &mut [], at, |_| false), Ok(false));
    assert!(s.projectile_damageable(3) && s.ball(3).is_some() && rx.try_recv().is_err());
    assert!(matches!(s.admit_area_source(source, &mut [], at, |_| true),
        Err(crate::space::combat_facts::Refusal::Replay)));
    assert!(s.projectile_damageable(3) && s.ball(3).is_some() && rx.try_recv().is_err());
}

/// A non-lethal pulse keeps the reduced structure in flight and records no
/// ship combat state for the projectile.
#[test]
fn nonlethal_area_damage_keeps_projectile_flying_without_ship_facts() {
    crate::tests::init_config();
    let mut s = live_source();
    let (tx, rx) = channel();
    s.launch_missile(missile(3), 1, 1, 60_000.0, move |_, i| tx.send(i).unwrap());
    let source = s.prepare_combat_source(1, None, None, Some(4),
        crate::lua::events::resources::FactOrigin::Natural,
        crate::space::targeting::next_generation(), 1, 1, |_| true).unwrap();
    s.observe_combat_utc().unwrap();
    let at = s.combat_observed_time();
    assert!(s.admit_area_source(source, &mut [], at, |_| true).unwrap());
    let outcome = s.apply_projectile_damage(3, [40.0, 0.0, 0.0, 0.0]).unwrap();
    assert_eq!((outcome.dealt, outcome.remaining_structure, outcome.destroyed), (40.0, 60.0, false));
    assert!(s.ball(3).is_some() && s.projectile_damageable(3) && rx.try_recv().is_err());
    assert!(!s.actor_combat.contains_key(&3) && !s.combat_facts.as_ref().unwrap().is_dirty());
}
