use super::*;
#[test]
fn queued_cloak_and_position_obey_guards_and_emit_native_actions() {
    let mut s = System::new(0, Arc::default(), 1_000);
    s.insert(ball(1, Some(100), 0.0));
    let mut own = watch(&mut s, 1, 1);
    let mut other = watch(&mut s, 2, 2);
    s.set_ball_position(1, [10.0, 20.0, 30.0], |_, _| false);
    assert!(s.begin_module_cloak(1, 10, Arc::new(std::sync::atomic::AtomicBool::new(false)), 0, 0, |_| false, None));
    s.tick(2_000);
    assert_eq!(s.ball(1).unwrap().pos, [0.0; 3]);
    assert_eq!(s.ball(1).unwrap().cloak, 0);
    assert!(own.try_recv().is_err());
    s.set_ball_position(1, [10.0, 20.0, 30.0], |_, _| true);
    assert!(s.begin_module_cloak(1, 10, Arc::new(std::sync::atomic::AtomicBool::new(false)), 0, 0, |_| true, None));
    s.tick(3_000);
    assert_eq!(s.ball(1).unwrap().pos, [10.0, 20.0, 30.0]);
    assert_eq!(s.ball(1).unwrap().cloak, 1);
    let events = own.try_recv().unwrap();
    assert_eq!(name(&events[0]), b"SetBallPosition");
    assert_eq!(name(&events[1]), b"CloakBall");
    let events = other.try_recv().unwrap();
    assert_eq!(name(&events[0]), b"SetBallPosition");
    assert_eq!(name(&events[1]), b"CloakBall");
}

#[test]
fn queued_cloak_rechecks_obstruction_before_success_callback() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let mut s = System::new(0, Arc::default(), 1000);
    s.insert(ball(1, Some(100), 0.0));
    s.insert(ball(2, None, 3000.0));
    let mut own = watch(&mut s, 1, 1);
    let generation = Arc::new(AtomicBool::new(false));
    let called = Arc::new(AtomicBool::new(false));
    s.command(2, |ball, _, _| { ball.pos = [100.0, 0.0, 0.0]; Vec::new() });
    let signal = called.clone();
    assert!(s.begin_module_cloak(1, 10, generation.clone(), 3000, 1000,
        |s| !s.cloak_obstructed(1), Some(Box::new(move |_| signal.store(true, Ordering::Relaxed)))));
    assert!(!called.load(Ordering::Relaxed));
    s.tick(2000);
    assert!(generation.load(Ordering::Relaxed));
    assert!(!s.has_module_cloak(1));
    assert_eq!(s.ball(1).unwrap().cloak, 0);
    assert!(own.try_recv().is_err());
    assert!(!called.load(Ordering::Relaxed));
    assert!(s.cloak_reactivation_ready(10));
    assert!(s.cloak_targeting_ready(1));

    s.command(2, |ball, _, _| { ball.pos = [3000.0, 0.0, 0.0]; Vec::new() });
    let signal = called.clone();
    assert!(s.begin_module_cloak(1, 10, Arc::new(AtomicBool::new(false)), 0, 0,
        |s| !s.cloak_obstructed(1), Some(Box::new(move |s| {
            assert_eq!(s.ball(1).unwrap().cloak, 1);
            signal.store(true, Ordering::Relaxed);
        }))));
    assert!(!called.load(Ordering::Relaxed));
    s.tick(3000);
    assert!(called.load(Ordering::Relaxed));
    assert_eq!(name(&own.try_recv().unwrap()[0]), b"CloakBall");
}

#[test]
fn queued_warp_blocks_position_commit() {
    let mut s = System::new(0, Arc::default(), 1000);
    s.insert(ball(1, Some(100), 0.0));
    let mut own = watch(&mut s, 1, 1);
    s.command(1, |ball, _, _| { ball.mode = MODE_WARP; Vec::new() });
    s.set_ball_position(1, [10.0, 20.0, 30.0], |ball, _| ball.mode != MODE_WARP);
    s.command(1, |ball, _, _| { ball.mode = MODE_STOP; Vec::new() });
    s.tick(2000);
    assert_eq!(s.ball(1).unwrap().pos, [0.0; 3]);
    assert!(own.try_recv().is_err());
}

#[test]
fn compressor_range_uses_live_matching_modules() {
    use std::sync::atomic::{AtomicBool, Ordering};

    let mut s = System::new(0, Arc::default(), 1_000);
    s.insert(ball(1, Some(100), 0.0));
    let stopped = Arc::new(AtomicBool::new(false));
    s.set_compressor(1, 10, stopped.clone(), 7, 80.0);
    s.set_compressor(1, 11, Arc::new(AtomicBool::new(false)), 7, 20.0);
    s.set_compressor(1, 12, Arc::new(AtomicBool::new(false)), 8, 30.0);
    s.set_compressor(1, 13, Arc::new(AtomicBool::new(false)), 9, 100.0);
    stopped.store(true, Ordering::Release);

    assert_eq!(s.compressor_range_matching(1, |list| list == 7 || list == 8), Some(30.0));
    assert_eq!(s.compressor_range_matching(1, |list| list == 7), Some(20.0));
    assert_eq!(s.compressor_range_matching(1, |list| list == 10), None);
    assert_eq!(s.compressor_range_matching(2, |_| true), None);
}

#[test]
fn compression_combines_ranges_and_removes_only_matching_generation() {
    use std::sync::atomic::AtomicBool;
    let mut s = System::new(0, Arc::default(), 1000);
    s.insert(ball(1, Some(100), 0.0));
    let a = Arc::new(AtomicBool::new(false));
    let b = Arc::new(AtomicBool::new(false));
    let stale = Arc::new(AtomicBool::new(false));
    s.set_compressor(1, 10, a.clone(), 7, 20.0);
    s.set_compressor(1, 11, b.clone(), 7, 30.0);
    s.set_compressor(1, 12, stale.clone(), 8, 40.0);
    let marker = |s: &System| s.ball(1).unwrap().slim.iter().find(|(key, _)| *key == "compression_facility_typelists").map(|(_, value)| value.clone());
    assert_eq!(marker(&s), Some(Value::Dict(vec![(Value::Int(7), Value::Float(30.0)), (Value::Int(8), Value::Float(40.0))])));
    s.remove_compressor(1, 11, &a);
    assert_eq!(marker(&s), Some(Value::Dict(vec![(Value::Int(7), Value::Float(30.0)), (Value::Int(8), Value::Float(40.0))])));
    s.remove_compressor(1, 11, &b);
    assert_eq!(marker(&s), Some(Value::Dict(vec![(Value::Int(7), Value::Float(20.0)), (Value::Int(8), Value::Float(40.0))])));
    s.remove_compressor(1, 10, &a);
    s.remove_compressor(1, 12, &stale);
    assert_eq!(marker(&s), None);
}

#[test]
fn module_cloak_uses_independent_delays_and_generation() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let mut s = System::new(0, Arc::default(), 1000);
    s.insert(ball(1, Some(100), 0.0));
    let generation = Arc::new(AtomicBool::new(false));
    let stale = Arc::new(AtomicBool::new(false));
    assert!(s.begin_module_cloak(1, 10, generation.clone(), 3000, 1000, |_| true, None));
    s.tick(2000);
    assert_eq!(s.ball(1).unwrap().cloak, 1);
    s.stop_module_cloak(1, 10, &stale);
    assert!(s.has_module_cloak(1));
    s.stop_module_cloak(1, 10, &generation);
    assert!(generation.load(Ordering::Relaxed));
    assert!(!s.cloak_reactivation_ready(10));
    assert!(!s.cloak_targeting_ready(1));
    s.tick(3000);
    assert!(s.cloak_targeting_ready(1));
    assert!(!s.cloak_reactivation_ready(10));
    s.tick(5000);
    assert!(s.cloak_reactivation_ready(10));
    assert_eq!(s.ball(1).unwrap().cloak, 0);
}

#[test]
fn cloak_proximity_uses_surfaces_and_cancels_cloak() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let mut s = System::new(0, Arc::default(), 1000);
    s.insert(ball(1, Some(100), 0.0));
    s.insert(ball(2, None, 2002.0));
    assert!(!s.cloak_obstructed(1));
    let generation = Arc::new(AtomicBool::new(false));
    assert!(s.begin_module_cloak(1, 10, generation.clone(), 3000, 1000, |_| true, None));
    s.tick(2000);
    s.command(2, |ball, _, _| { ball.pos = [2001.0, 0.0, 0.0]; Vec::new() });
    s.tick(3000);
    assert!(generation.load(Ordering::Relaxed));
    assert!(!s.has_module_cloak(1));
}

#[test]
fn heading_uses_velocity_then_goto_then_stationary_fallback() {
    let mut s = System::new(0, Arc::default(), 1000);
    let mut b = ball(1, Some(100), 0.0);
    b.vel = [3.0, 0.0, 0.0];
    s.insert(b);
    assert_eq!(s.heading(1), Some([1.0, 0.0, 0.0]));
    s.command(1, |ball, _, _| {
        ball.vel = [0.0; 3];
        ball.mode = MODE_GOTO;
        ball.goto = [0.0, 10.0, 0.0];
        Vec::new()
    });
    s.tick(2000);
    assert_eq!(s.heading(1), Some([0.0, 1.0, 0.0]));
    s.command(1, |ball, _, _| { ball.mode = MODE_STOP; ball.vel = [0.0; 3]; Vec::new() });
    s.tick(3000);
    assert_eq!(s.heading(1), Some([0.0, 0.0, -1.0]));
}

#[test]
fn tractor_exclusivity_generation_and_departure_restore_movement() {
    use std::sync::atomic::AtomicBool;
    let mut s = System::new(0, Arc::default(), 1000);
    s.insert(ball(1, None, 1000.0));
    let mut target = ball(2, None, 0.0);
    target.max_vel = 40.0;
    target.speed_fraction = 0.25;
    s.insert(target);
    let generation = Arc::new(AtomicBool::new(false));
    let stale = Arc::new(AtomicBool::new(false));
    assert!(s.begin_tractor(2, 10, generation.clone(), 20.0, 0.0, 1));
    assert!(!s.begin_tractor(2, 11, stale.clone(), 30.0, 0.0, 1));
    s.end_tractor(2, 10, &stale);
    s.tick(2000);
    assert_eq!(s.ball(2).unwrap().follow_id, 1);
    assert_eq!(s.ball(2).unwrap().max_vel, 20.0);
    s.remove_ball(1);
    s.tick(3000);
    let restored = s.ball(2).unwrap();
    assert_eq!(restored.mode, MODE_STOP);
    assert_eq!(restored.max_vel, 40.0);
    assert_eq!(restored.speed_fraction, 0.25);
}

#[test]
fn tractor_cancelled_before_boundary_never_changes_target() {
    use std::sync::atomic::AtomicBool;
    let mut s = System::new(0, Arc::default(), 1000);
    s.insert(ball(1, None, 1000.0));
    s.insert(ball(2, None, 0.0));
    let generation = Arc::new(AtomicBool::new(false));
    assert!(s.begin_tractor(2, 10, generation.clone(), 20.0, 0.0, 1));
    s.end_tractor(2, 10, &generation);
    s.tick(2000);
    assert_eq!(s.ball(2).unwrap().mode, MODE_STOP);
    assert_eq!(s.ball(2).unwrap().max_vel, 100.0);
}

#[test]
fn bomb_flight_and_timer_begin_at_same_boundary() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let mut s = System::new(0, Arc::default(), 1000);
    let mut bomb = ball(3, None, 0.0);
    bomb.massive = false;
    bomb.max_vel = 50.0;
    let fired = Arc::new(AtomicBool::new(false));
    let hit = fired.clone();
    s.launch_bomb(bomb, [1.0, 0.0, 0.0], 500, move |_| { hit.store(true, Ordering::Relaxed); });
    s.tick(1500);
    assert!(!fired.load(Ordering::Relaxed));
    assert_eq!(s.ball(3).unwrap().mode, MODE_STOP);
    s.tick(2000);
    let projectile = s.ball(3).unwrap();
    assert_eq!(projectile.mode, MODE_GOTO);
    assert_eq!(projectile.follow_id, 0);
    assert!(projectile.pos[0] > 0.0);
    assert!(!fired.load(Ordering::Relaxed));
    s.tick(2500);
    assert!(fired.load(Ordering::Relaxed));
}

#[test]
fn pending_cloak_cancel_never_becomes_visible_as_cloaked() {
    use std::sync::atomic::AtomicBool;
    let mut s = System::new(0, Arc::default(), 1000);
    s.insert(ball(1, Some(100), 0.0));
    let generation = Arc::new(AtomicBool::new(false));
    assert!(s.begin_module_cloak(1, 10, generation.clone(), 3000, 1000, |_| true, None));
    s.stop_module_cloak(1, 10, &generation);
    s.tick(2000);
    assert_eq!(s.ball(1).unwrap().cloak, 0);
    assert!(s.cloak_reactivation_ready(10));
    assert!(s.cloak_targeting_ready(1));
}

#[test]
fn tractor_restores_previous_follow_and_target_removal_releases_owner() {
    use std::sync::atomic::AtomicBool;
    let mut s = System::new(0, Arc::default(), 1000);
    s.insert(ball(1, None, 1000.0));
    s.insert(ball(3, None, -1000.0));
    let mut target = ball(2, None, 0.0);
    target.mode = MODE_FOLLOW;
    target.follow_id = 3;
    target.follow_range = 12.0;
    target.speed_fraction = 0.5;
    s.insert(target);
    let generation = Arc::new(AtomicBool::new(false));
    assert!(s.begin_tractor(2, 10, generation.clone(), 20.0, 0.0, 1));
    s.tick(2000);
    s.end_tractor(2, 10, &generation);
    s.tick(3000);
    let restored = s.ball(2).unwrap();
    assert_eq!((restored.mode, restored.follow_id, restored.follow_range), (MODE_FOLLOW, 3, 12.0));
    assert_eq!(restored.speed_fraction, 0.5);
    assert!(s.begin_tractor(2, 10, generation.clone(), 20.0, 0.0, 1));
    s.remove_ball(2);
    s.insert(ball(2, None, 0.0));
    s.tick(4000);
    assert_eq!(s.ball(2).unwrap().mode, MODE_STOP);
}

#[test]
fn tractor_restores_follow_with_nonfinite_legacy_max_speed() {
    use std::sync::atomic::AtomicBool;
    for legacy_speed in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        let mut s = System::new(0, Arc::default(), 1000);
        s.insert(ball(1, None, 1000.0));
        s.insert(ball(3, None, -1000.0));
        let mut target = ball(2, None, 0.0);
        target.max_vel = legacy_speed;
        target.mode = MODE_FOLLOW;
        target.follow_id = 3;
        target.follow_range = 12.0;
        target.speed_fraction = 0.5;
        s.insert(target);
        let mut rx = watch(&mut s, 1, 2);
        let generation = Arc::new(AtomicBool::new(false));
        assert!(s.begin_tractor(2, 10, generation.clone(), 20.0, 0.0, 1));
        s.tick(2000);
        assert_eq!(s.ball(2).unwrap().max_vel, 20.0);
        let begin_events = rx.try_recv().unwrap();
        assert_eq!(begin_events.len(), 3);
        assert_eq!(name(&begin_events[0]), b"SetMaxSpeed");
        assert_eq!(name(&begin_events[1]), b"FollowBall");
        assert_eq!(name(&begin_events[2]), b"SetSpeedFraction");
        assert!(rx.try_recv().is_err());

        let restore_stamp = s.current_time();
        s.end_tractor(2, 10, &generation);
        assert!(rx.try_recv().is_err());
        s.tick(3000);
        let restored = s.ball(2).unwrap();
        assert_eq!(restored.max_vel, 20.0);
        assert_eq!((restored.mode, restored.follow_id, restored.follow_range), (MODE_FOLLOW, 3, 12.0));
        assert_eq!(restored.speed_fraction, 0.5);
        let events = rx.try_recv().unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(name(&events[0]), b"FollowBall");
        assert_eq!(name(&events[1]), b"SetSpeedFraction");
        for event in &events {
            let Value::Tuple(parts) = event else { panic!("event tuple") };
            assert_eq!(parts[0].as_int(), Some(restore_stamp));
        }
        assert!(rx.try_recv().is_err());
    }
}
