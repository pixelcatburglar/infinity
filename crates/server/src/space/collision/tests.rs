//! Focused boundaries for the native sphere collision mechanic.
use super::*;

fn sphere(id: i64, pos: V3, velocity: V3) -> Snapshot {
    let ball = super::super::tests::destiny_reference::ship(id, pos, velocity);
    Snapshot::new(&ball, true)
}

#[test]
fn stationary_contact_wakes_and_disabled_targets_do_not() {
    crate::tests::init_config();
    let mut system = system::System::new(0, std::sync::Arc::default(), 0);
    let a = super::super::tests::destiny_reference::ship(1, [0.0; 3], [0.0; 3]);
    let mut b = super::super::tests::destiny_reference::ship(2, [20.0, 0.0, 0.0], [0.0; 3]);
    b.free = false;
    b.massive = false;
    system.insert(a);
    system.insert(b.clone());
    assert!(system.is_cold(true));
    b.massive = true;
    b.cloak = 1;
    system.insert(b.clone());
    assert!(system.is_cold(true));
    b.cloak = 0;
    system.insert(b);
    assert!(!system.is_cold(true));
    system.tick_with_collisions(1000, true);
    assert_ne!(system.ball(1).unwrap().pos, [0.0; 3]);
}

#[test]
fn sweep_tangent_miss_and_fast_crossing() {
    let zero = [0.0; 3];
    assert_eq!(collide_two_spheres([-2.0, 1.0, 0.0], [2.0, 1.0, 0.0], zero, zero, 1.0), Some(0.5));
    assert_eq!(collide_two_spheres([-2.0, 1.1, 0.0], [2.0, 1.1, 0.0], zero, zero, 1.0), None);
    assert_eq!(collide_two_spheres([-10.0, 0.0, 0.0], [10.0, 0.0, 0.0], zero, zero, 1.0), Some(0.45));
    assert!(quadratic(1.0, 0.0, 1.0).is_none());
}

#[test]
fn broadphase_keeps_boundaries_and_uncertainty() {
    let zero = [0.0; 3];
    assert!(swept_aabb(zero, zero, [2.0, 0.0, 0.0], [2.0, 0.0, 0.0], 2.0));
    assert!(!swept_aabb(zero, zero, [3.0, 0.0, 0.0], [3.0, 0.0, 0.0], 2.0));
    assert!(swept_aabb([f64::NAN, 0.0, 0.0], zero, [3.0, 0.0, 0.0], [3.0, 0.0, 0.0], 2.0));
}

#[test]
fn source_cloak_is_asymmetric_and_nonmassive_excluded() {
    let mut a = sphere(1, [0.0; 3], [0.0; 3]);
    let mut b = sphere(2, [20.0, 0.0, 0.0], [0.0; 3]);
    a.cloak = 1;
    assert!(eligible(&a, &b));
    assert!(!eligible(&b, &a));
    b.massive = false;
    assert!(!eligible(&a, &b));
    assert!(!eligible(&b, &a));
}

#[test]
fn latest_and_equal_strongest_fold_is_deterministic() {
    let me = sphere(1, [0.0; 3], [100.0, 0.0, 0.0]);
    let mut first = sphere(2, [30.0, 0.0, 0.0], [0.0; 3]);
    let mut later = sphere(3, [50.0, 0.0, 0.0], [0.0; 3]);
    first.free = false;
    later.free = false;
    let all = gradient(&me, &[me, first, later]);
    assert_eq!(all, gradient(&me, &[me, later]));
    assert_eq!(all, gradient(&me, &[later, first, me]));
    let mut state = Fold { collision: 0.0, acceleration: [1.0, 0.0, 0.0] };
    let overlap = sphere(4, [10.0, 0.0, 0.0], [0.0; 3]);
    potential(me, overlap, 0, &mut state);
    assert!(dot(state.acceleration, state.acceleration) > 1.0);
}

#[test]
fn effective_flags_preserve_other_bits_and_missile_exception() {
    let mut ball = super::super::tests::destiny_reference::ship(1, [0.0; 3], [0.0; 3]);
    for mode in [MODE_STOP, MODE_RIGID, MODE_GOTO, MODE_FOLLOW, MODE_ORBIT] {
        ball.mode = mode;
        for free in [false, true] {
            ball.free = free;
            ball.global = true;
            let on = ball.flags(true);
            let off = ball.flags(false);
            assert_eq!(on ^ off, FLAG_MASSIVE);
            let mut a = Vec::new();
            let mut b = Vec::new();
            ball.write(&mut a, true);
            ball.write(&mut b, false);
            let offset = 8 + 1 + 4 + 3 * 8;
            assert_eq!(a[offset] ^ b[offset], FLAG_MASSIVE);
            a[offset] = b[offset];
            assert_eq!(a, b);
        }
    }
    ball.mode = MODE_MISSILE;
    assert_eq!(ball.flags(false), ball.flags(true));
    assert_ne!(ball.flags(false) & FLAG_MASSIVE, 0);
    ball.massive = false;
    assert_eq!(ball.flags(false) & FLAG_MASSIVE, 0);
    assert_eq!(ball.flags(true) & FLAG_MASSIVE, 0);
    let target = super::super::tests::destiny_reference::ship(2, [10.0, 0.0, 0.0], [0.0; 3]);
    ball.massive = true;
    ball.free = true;
    ball.follow_id = 2;
    ball.launcher = 1;
    ball.id = 3;
    assert!(eligible(&Snapshot::new(&ball, false), &Snapshot::new(&target, false)));
}

#[test]
fn gated_tick_passes_through_ships_and_station_and_cold_overlap() {
    crate::tests::init_config();
    let ship = super::super::tests::destiny_reference::ship;
    for rigid in [false, true] {
        let mut results = Vec::new();
        for (collisions, obstacle) in [(false, false), (false, true), (true, true)] {
            let mut s = system::System::new(0, std::sync::Arc::default(), 0);
            s.insert(ship(1, [-15.0, 0.0, 0.0], [100.0, 0.0, 0.0]));
            if obstacle {
                let mut other = ship(2, [15.0, 0.0, 0.0], if rigid { [0.0; 3] } else { [-100.0, 0.0, 0.0] });
                if rigid { other.mode = MODE_RIGID; other.free = false; }
                s.insert(other);
            }
            s.tick_with_collisions(1000, collisions);
            results.push(s.ball(1).unwrap().pos);
        }
        assert_eq!(results[0], results[1]);
        assert_ne!(results[0], results[2]);
    }
    let mut s = system::System::new(0, std::sync::Arc::default(), 0);
    s.insert(ship(1, [0.0; 3], [0.0; 3]));
    s.insert(ship(2, [10.0, 0.0, 0.0], [0.0; 3]));
    assert!(s.is_cold(false));
    assert!(!s.is_cold(true));
}

#[test]
fn uncloak_resets_only_owner_after_uncloak() {
    crate::tests::init_config();
    for collisions in [false, true] {
        let mut s = system::System::new(0, std::sync::Arc::default(), 0);
        let mut ball = super::super::tests::destiny_reference::ship(1, [0.0; 3], [0.0; 3]);
        ball.cloak = CLOAK_RESTORE;
        s.insert(ball);
        let (own_tx, mut own) = tokio::sync::mpsc::unbounded_channel();
        let (other_tx, mut other) = tokio::sync::mpsc::unbounded_channel();
        s.observe(1, 1, own_tx);
        s.observe(2, 2, other_tx);
        s.apply_uncloak_with_collisions(1, 7, collisions);
        let fx = special_fx(7, 1, None, None, "effects.Uncloak", 1, 0, 5, None);
        let mut expected = vec![event(7, "UncloakBall", vec![Value::Int(1)])];
        if !collisions { expected.push(reset_event(1, 7, false).unwrap()); }
        expected.push(fx.clone());
        assert_eq!(own.try_recv().unwrap(), expected);
        assert_eq!(other.try_recv().unwrap(), vec![add_balls_event(7, s.ball(1).unwrap(), collisions), fx]);
    }
}

#[test]
fn warp_exit_resets_after_step_with_or_without_callback() {
    crate::tests::init_config();
    for collisions in [false, true] {
        for callback in [false, true] {
            let mut s = system::System::new(0, std::sync::Arc::default(), 0);
            let ball = super::super::tests::destiny_reference::ship(1, [0.0; 3], [100.0, 0.0, 0.0]);
            s.insert(ball);
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
            s.observe(1, 1, tx);
            s.command(1, |b, _, _| { b.warp_to([1_000_000.0, 0.0, 0.0], 0.0, 3000); Vec::new() });
            let (done, result) = std::sync::mpsc::channel();
            if callback {
                s.on_warp_exit(1, move |s| {
                    let b = s.ball(1).unwrap();
                    let mass = b.mass * b.agility as f64;
                    assert_eq!((b.pos, b.vel), integrate(b.old_pos, b.old_vel, [0.0; 3], mass, DESTINY_FRICTION, integration_factor(mass, DESTINY_FRICTION), 1.0));
                    done.send(()).unwrap();
                });
            }
            for tick in 1..=100 {
                s.tick_with_collisions(tick * 1000, collisions);
                if s.ball(1).unwrap().mode == MODE_STOP { break; }
            }
            assert_eq!(s.ball(1).unwrap().mode, MODE_STOP);
            if callback { assert_eq!(result.try_recv(), Ok(())); }
            assert_eq!(s.ball(1).unwrap().flags(collisions) & FLAG_MASSIVE != 0, collisions);
            let mut updates = Vec::new();
            while let Ok(batch) = rx.try_recv() { updates.extend(batch); }
            let reset = reset_event(1, s.current_time(), false).unwrap();
            assert_eq!(updates.iter().filter(|item| **item == reset).count(), usize::from(!collisions));
            assert!(!updates.contains(&reset_event(1, s.current_time() - 1, false).unwrap()));
        }
    }
}

#[test]
#[ignore]
fn default_off_collision_lifecycle() {
    crate::config::init_game(Default::default()).expect("initializing default game configuration");
    assert!(!enabled());
    let mut s = system::System::new(0, std::sync::Arc::default(), 0);
    let ship = super::super::tests::destiny_reference::ship;
    let fullstate = |state: Value| {
        let Value::Tuple(parts) = state else { panic!("event") };
        let Value::Tuple(call) = &parts[1] else { panic!("call") };
        let Value::Tuple(args) = &call[1] else { panic!("args") };
        let Value::Instance(_, bag) = &args[0] else { panic!("bag") };
        let Value::Dict(fields) = &**bag else { panic!("fields") };
        let Value::Bytes(bytes) = &fields.iter().find(|(key, _)| key == &Value::str("state")).unwrap().1 else { panic!("state") };
        bytes.clone()
    };
    for (label, mode, free, global, interactive, radius) in [
        ("station", MODE_RIGID, false, true, false, 1000.0_f32),
        ("gate", MODE_RIGID, false, true, false, 200.0),
        ("player", MODE_GOTO, true, false, true, 30.0),
        ("npc", MODE_STOP, true, false, false, 40.0),
        ("drone", MODE_STOP, true, false, false, 5.0),
        ("asteroid", MODE_RIGID, false, false, false, 300.0),
        ("wreck", MODE_RIGID, false, false, false, 20.0),
        ("container", MODE_RIGID, false, false, false, 15.0),
    ] {
        let mut ball = ship(1, [0.0; 3], [0.0; 3]);
        ball.mode = mode;
        ball.free = free;
        ball.global = global;
        ball.interactive = interactive;
        ball.radius = radius;
        let mut single = system::System::new(0, std::sync::Arc::default(), 0);
        single.insert(ball);
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let bytes = fullstate(single.observe(1, 1, tx));
        assert_eq!(bytes[0], DESTINY_FULLSTATE, "{label}");
        assert_eq!(&bytes[5 + 8 + 1..5 + 8 + 1 + 4], &radius.to_le_bytes(), "{label}");
        let expected = (if free { FLAG_FREE } else { 0 })
            | (if global { FLAG_GLOBAL } else { 0 })
            | (if interactive { FLAG_INTERACTIVE } else { 0 });
        assert_eq!(bytes[5 + 8 + 1 + 4 + 3 * 8], expected, "{label}");
    }
    let mut stationary = system::System::new(0, std::sync::Arc::default(), 0);
    stationary.insert(ship(1, [0.0; 3], [0.0; 3]));
    stationary.insert(ship(2, [10.0, 0.0, 0.0], [0.0; 3]));
    assert!(stationary.is_cold(enabled()));
    stationary.tick(1000);
    assert!(stationary.is_cold(enabled()));
    assert_eq!(stationary.ball(1).unwrap().pos, [0.0; 3]);
    assert_eq!(stationary.ball(2).unwrap().pos, [10.0, 0.0, 0.0]);
    s.insert(ship(1, [-15.0, 0.0, 0.0], [100.0, 0.0, 0.0]));
    s.insert(ship(2, [15.0, 0.0, 0.0], [-100.0, 0.0, 0.0]));
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let bytes = fullstate(s.observe(1, 1, tx));
    assert_eq!(bytes[0], DESTINY_FULLSTATE);
    assert_eq!(bytes[5 + 8 + 1 + 4 + 3 * 8] & FLAG_MASSIVE, 0);
    let mut unobstructed = system::System::new(0, std::sync::Arc::default(), 0);
    unobstructed.insert(ship(1, [-15.0, 0.0, 0.0], [100.0, 0.0, 0.0]));
    unobstructed.tick(1000);
    s.tick(1000);
    assert_eq!(s.ball(1).unwrap().pos, unobstructed.ball(1).unwrap().pos);
    assert_eq!(s.ball(1).unwrap().flags(enabled()) & FLAG_MASSIVE, 0);
    let mut hidden = ship(3, [0.0; 3], [0.0; 3]);
    hidden.cloak = CLOAK_RESTORE;
    s.insert(hidden);
    let (tx, mut owner) = tokio::sync::mpsc::unbounded_channel();
    s.observe(3, 3, tx);
    assert!(s.uncloak(3));
    s.tick(2000);
    let batch = owner.try_recv().unwrap();
    assert_eq!(batch[0], event(1, "UncloakBall", vec![Value::Int(3)]));
    assert_eq!(batch[1], reset_event(3, 1, false).unwrap());
    let warper = ship(4, [0.0; 3], [100.0, 0.0, 0.0]);
    s.insert(warper);
    s.command(4, |b, _, _| { b.warp_to([1_000_000.0, 0.0, 0.0], 0.0, 3000); Vec::new() });
    for tick in 3..=102 {
        s.tick(tick * 1000);
        if s.ball(4).unwrap().mode == MODE_STOP { break; }
    }
    assert_eq!(s.ball(4).unwrap().mode, MODE_STOP);
    let mut updates = Vec::new();
    while let Ok(batch) = owner.try_recv() { updates.extend(batch); }
    assert!(updates.contains(&reset_event(4, s.current_time(), false).unwrap()));
}

#[test]
fn nonmassive_missile_launch_and_timer_survive_off_tick() {
    crate::tests::init_config();
    let ship = super::super::tests::destiny_reference::ship;
    let mut s = system::System::new(0, std::sync::Arc::default(), 0);
    s.insert(ship(1, [0.0; 3], [0.0; 3]));
    s.insert(ship(2, [100_000.0, 50_000.0, 0.0], [0.0; 3]));
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    s.observe(1, 1, tx);
    let mut missile = ship(3, [0.0; 3], [0.0; 3]);
    missile.massive = false;
    let (done, result) = std::sync::mpsc::channel();
    s.launch_missile(missile, 2, 1, 2000, move |_| { done.send(()).unwrap(); });
    s.tick_with_collisions(1000, false);
    let missile = s.ball(3).unwrap();
    assert!(!missile.massive);
    assert_eq!((missile.mode, missile.follow_id, missile.launcher), (MODE_MISSILE, 2, 1));
    assert!(missile.pos[0] > 0.0 && missile.pos[1] > 0.0);
    assert!(result.try_recv().is_err());
    let updates = rx.try_recv().unwrap();
    assert!(updates.contains(&event(0, "LaunchMissile", vec![
        Value::Int(3), Value::Int(2), Value::Int(1), Value::Bool(true), Value::Bool(false),
    ])));
    s.tick_with_collisions(2000, false);
    let missile = s.ball(3).unwrap();
    assert_eq!(missile.goto, [100_000.0, 50_000.0, 0.0]);
    assert!(missile.pos[0] > 0.0 && missile.pos[1] > 0.0);
    assert!(result.try_recv().is_err());
    s.tick_with_collisions(3000, false);
    assert_eq!(result.try_recv(), Ok(()));
    assert!(result.try_recv().is_err());
    s.tick_with_collisions(4000, false);
    assert!(result.try_recv().is_err());
}
