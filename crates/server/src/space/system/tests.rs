use super::*;
mod jamming;
mod jump_cloak;
mod drone;
mod visibility;
pub(crate) mod events;
#[path = "../effects/tests.rs"]
mod effect_actions;

fn ball(id: i64, pilot: Option<i64>, x: f64) -> Ball {
    crate::tests::init_config();
    Ball {
        id, type_id: 1, owner_id: 1, corp_id: 1, alliance_id: -1,
        char_id: pilot, signature_radius: pilot.map(|_| 1.0), mode: MODE_STOP, radius: 1.0, mass: 1e6,
        max_vel: 100.0, agility: 1.0, speed_fraction: 0.0,
        pos: [x, 0.0, 0.0], vel: [0.0; 3], goto: [0.0; 3],
        old_pos: [x, 0.0, 0.0], old_vel: [0.0; 3], segment_acceleration: None,
        follow_id: 0, follow_range: 0.0, free: true, global: false, interactive: pilot.is_some(),
        massive: true, warp: WarpState::default(), warp_factor: 0, last_ms: 0, interpolation_anchor_ms: 0, effect_stamp: 0,
        cloak: 0, cloak_until: 0, health: [1.0; 3], launcher: 0,
        slim: Vec::new(),
    }
}

#[test]
fn projection_replacement_expiry_and_departure_cleanup() {
    use crate::services::effects::{projection::Projection, EffectKind, JammingType};
    let mut system = System::new(1, Arc::default(), 1000);
    for id in 1..=3 { system.insert(ball(id, None, 0.0)); }
    let projection = |module, deadline| Projection {
        source_ship: 1, source_module: module, source_type: 1, effect_id: 1,
        kind: EffectKind::Projected { jamming: JammingType::Webify }, until_ms: deadline,
        values: Vec::new(), charge_values: Vec::new(), charge_type: None,
    };
    system.upsert_projection(2, projection(10, 1500));
    system.upsert_projection(3, projection(10, 2000));
    assert!(system.projections_on(2).is_empty());
    assert_eq!(system.projections_on(3).len(), 1);
    let revision = system.projection_revision(3);
    system.tick(1500);
    assert_eq!(system.projections_on(3).len(), 1);
    assert_eq!(system.projection_revision(3), revision);
    system.tick(2000);
    assert!(system.projections_on(3).is_empty());
    assert_ne!(system.projection_revision(3), revision);
    system.upsert_projection(2, projection(10, 3000));
    system.upsert_projection(3, projection(11, 3000));
    system.remove_ball(1);
    assert!(system.projections_on(2).is_empty());
    assert!(system.projections_on(3).is_empty());
}

#[test]
fn completed_segment_samples_use_shared_drag_and_clamp() {
    let mut s = System::new(0, Arc::default(), 1_000);
    let mut moving = ball(1, None, 5.0);
    moving.vel = [10.0, 0.0, 0.0];
    s.insert(moving);
    assert_eq!(s.sample_position(1), Some([5.0, 0.0, 0.0]));
    s.tick(1_900);
    assert_eq!(s.sample_position(1), Some([5.0, 0.0, 0.0]));
    s.tick(2_000);
    let b = s.ball(1).unwrap();
    let endpoint = b.pos;
    let midpoint = b.position_at(2_500);
    let mut bytes = Vec::new();
    b.write(&mut bytes, true);
    for elapsed in [0, 100, 500, 900, 1000, 900, 100, 500, 0, 1000] {
        let actual = b.position_at(2_000 + elapsed);
        let expected = if elapsed == 0 { b.old_pos } else if elapsed == 1000 { endpoint } else {
            integrate(b.old_pos, b.old_vel, [0.0; 3], 1e6, DESTINY_FRICTION, 0.0, ms_to_seconds(elapsed as f64)).0
        };
        assert_eq!(actual, expected);
    }
    assert_ne!(midpoint[0], (5.0 + endpoint[0]) / 2.0);
    assert_eq!(b.position_at(i64::MIN), b.old_pos);
    assert_eq!(b.position_at(i64::MAX), endpoint);
    let mut materialized = b.clone();
    materialized.pos = b.position_at(3_000);
    let mut sampled_bytes = Vec::new();
    materialized.write(&mut sampled_bytes, true);
    assert_eq!(bytes, sampled_bytes);
    s.tick(2_500);
    assert_eq!(s.sampled_ball(1).unwrap().pos, s.sample_position(1).unwrap());
    assert_ne!(s.sample_position(1).unwrap(), s.ball(1).unwrap().pos);
    assert!(s.sample_position(99).is_none());
    assert!(s.sampled_ball(99).is_none());
    let removed = s.destroy_ball(1).unwrap();
    assert_eq!(removed.pos, midpoint);
    assert_ne!(removed.pos, endpoint);
    assert!(s.ball(1).is_none());
}

#[test]
fn sampled_ball_uses_accelerating_segment_state_at_actor_clock() {
    let mut s = System::new(0, Arc::default(), 1_000);
    let mut moving = ball(1, None, 5.0);
    moving.vel = [10.0, 2.0, 0.0];
    moving.speed_fraction = 1.0;
    moving.goto_direction([1.0, 0.0, 0.0]);
    s.insert(moving);
    s.tick(2_000);
    let source = s.ball(1).unwrap().clone();
    let acceleration = source.segment_acceleration.unwrap();
    assert_ne!(acceleration, [0.0; 3]);
    let expected = integrate(source.old_pos, source.old_vel, acceleration,
        source.mass * source.agility as f64, DESTINY_FRICTION, 0.0, 0.5);
    assert_eq!(source.state_at(2_500), expected);
    assert_eq!(source.position_at(2_500), expected.0);
    assert_eq!(source.state_at(2_000), (source.old_pos, source.old_vel));
    assert_eq!(source.state_at(3_000), (source.pos, source.vel));
    s.tick(2_500);
    let sampled = s.sampled_ball(1).unwrap();
    assert_eq!((sampled.pos, sampled.vel), expected);
    assert_ne!(sampled.vel, source.vel);
    let unchanged = s.ball(1).unwrap();
    assert_eq!((unchanged.pos, unchanged.vel, unchanged.old_pos, unchanged.old_vel, unchanged.segment_acceleration),
        (source.pos, source.vel, source.old_pos, source.old_vel, source.segment_acceleration));
    assert!(s.sampled_ball(99).is_none());
}

#[test]
fn static_modes_stay_fixed() {
    let positions = HashMap::new();
    let mut fixed = ball(2, None, 7.0);
    fixed.mode = MODE_RIGID;
    fixed.free = false;
    fixed.evolve_behavior_for_ball(1000, 1000, &positions, 0);
    fixed.evolve_position(1000, integration_factor(fixed.mass * fixed.agility as f64, DESTINY_FRICTION));
    assert_eq!(fixed.position_at(1500), [7.0, 0.0, 0.0]);
}

#[test]
fn stationary_command_replaces_previous_segment_at_application() {
    let mut s = System::new(0, Arc::default(), 1000);
    let mut moving = ball(1, None, 0.0);
    moving.vel = [10.0, 0.0, 0.0];
    s.insert(moving);
    s.tick(2000);
    assert_ne!(s.ball(1).unwrap().position_at(2000), s.ball(1).unwrap().pos);
    s.command(1, |b, _, _| {
        b.pos = [42.0, 0.0, 0.0];
        b.vel = [0.0; 3];
        b.mass = 2e6;
        Vec::new()
    });
    s.tick(3000);
    assert_eq!(s.sample_position(1), Some([42.0, 0.0, 0.0]));
    assert_eq!(s.ball(1).unwrap().position_at(3500), [42.0, 0.0, 0.0]);
}

#[test]
fn health_and_uncloak_leave_completed_segment_intact() {
    let mut s = System::new(0, Arc::default(), 1000);
    let mut moving = ball(1, None, 0.0);
    moving.vel = [10.0, 0.0, 0.0];
    moving.cloak = 1;
    s.insert(moving);
    s.tick(2000);
    let before = s.ball(1).unwrap().position_at(2500);
    s.set_health(1, [0.5; 3]);
    s.apply_uncloak(1, 2);
    assert_eq!(s.ball(1).unwrap().position_at(2500), before);
}

#[test]
fn pilot_index_tracks_insert_take_and_capsule_before_ship_removal() {
    let mut system = System::default();
    system.insert(ball(1, None, 0.0));
    system.insert(ball(2, Some(100), 0.0));
    assert_eq!(system.pilot_ball(100).unwrap().id, 2);
    assert_eq!(system.pilots().count(), 1);
    assert!(system.contains(1));
    assert_eq!(system.balls().count(), 2);

    system.insert(ball(3, Some(100), 0.0));
    assert_eq!(system.take(2).unwrap().char_id, Some(100));
    assert_eq!(system.pilot_ball(100).unwrap().id, 3);
    assert_eq!(system.pilots().count(), 1);
    system.take(3).unwrap();
    assert!(system.pilot_ball(100).is_none());
    assert_eq!(system.pilots().count(), 0);
    assert!(system.take(3).is_none());

    system.insert(ball(4, Some(101), 0.0));
    system.insert(ball(4, Some(102), 0.0));
    assert!(system.pilot_ball(101).is_none());
    assert_eq!(system.pilot_ball(102).unwrap().id, 4);
    system.insert(ball(4, None, 0.0));
    assert!(system.pilot_ball(102).is_none());
    assert!(system.contains(4));
}

#[test]
fn nearest_hostile_via_pilot_index_matches_full_scan() {
    let mut system = System::default();
    system.insert(ball(1, None, 1.0));
    system.insert(ball(2, Some(100), 2.0));
    let mut cloaked = ball(3, Some(101), 3.0);
    cloaked.cloak = 1;
    system.insert(cloaked);
    let mut warping = ball(4, Some(102), 4.0);
    warping.mode = MODE_WARP;
    system.insert(warping);
    system.insert(ball(5, Some(103), 5.0));
    system.insert(ball(6, Some(104), 9.0));
    system.insert(ball(7, Some(105), 20.0));

    fn nearest<'a>(balls: impl Iterator<Item = &'a Ball>, aggro: f64) -> Option<i64> {
        balls
            .filter(|b| b.char_id.is_some_and(|ch| ch != 100)
                && b.cloak == 0 && b.mode != MODE_WARP && len(b.pos) <= aggro)
            .min_by(|a, b| len(a.pos).total_cmp(&len(b.pos)))
            .map(|b| b.id)
    }
    for (range, expected) in [(4.0, None), (5.0, Some(5)), (30.0, Some(5))] {
        let indexed = nearest(system.pilots(), range);
        assert_eq!(indexed, nearest(system.balls(), range));
        assert_eq!(indexed, expected);
    }
    system.take(5);
    assert_eq!(nearest(system.pilots(), 30.0), Some(6));
    assert_eq!(nearest(system.pilots(), 30.0), nearest(system.balls(), 30.0));
}

fn watch(s: &mut System, client: i64, ego: i64) -> mpsc::UnboundedReceiver<Vec<Value>> {
    let (tx, rx) = mpsc::unbounded_channel();
    s.observe(client, ego, tx);
    rx
}

fn name(event: &Value) -> &[u8] {
    let Value::Tuple(parts) = event else { panic!("event tuple") };
    let Value::Tuple(call) = &parts[1] else { panic!("call tuple") };
    let Value::Bytes(name) = &call[0] else { panic!("event name") };
    name
}

#[test]
fn command_stamps_seconds_and_borrows_other_balls() {
    let mut s = System::new(0, Arc::default(), 12_345);
    s.insert(ball(1, Some(100), 0.0));
    s.insert(ball(2, None, 10.0));
    let mut rx = watch(&mut s, 1, 1);
    s.command(1, |b, stamp, others| {
        assert!(others.ball(1).is_none());
        b.goto = others.ball(2).unwrap().pos;
        vec![event(stamp, "GotoDirection", vec![Value::Int(b.id)])]
    });
    assert_eq!(s.ball(1).unwrap().goto, [0.0; 3]);
    assert!(rx.try_recv().is_err());
    s.tick(12_999);
    assert!(rx.try_recv().is_err());
    s.tick(13_000);
    let events = rx.try_recv().unwrap();
    let Value::Tuple(parts) = &events[0] else { panic!("event") };
    assert_eq!(parts[0].as_int(), Some(12));
    assert_eq!(s.ball(1).unwrap().goto, [10.0, 0.0, 0.0]);
}


#[test]
fn missile_impact_delay_starts_at_launch_boundary() {
    let mut s = System::new(0, Arc::default(), 12_100);
    let mut rx = watch(&mut s, 1, 1);
    s.insert(ball(5, None, 0.0));
    s.insert(ball(4, None, 1000.0));
    let fired = Arc::new(Mutex::new(Vec::new()));
    let observed = fired.clone();
    let mut missile = ball(3, None, 0.0);
    missile.massive = false;
    s.launch_missile(missile, 4, 5, 500, move |s| {
        assert_eq!(s.ball(3).unwrap().mode, MODE_MISSILE);
        observed.lock().unwrap().push(s.now_ms());
    });
    let b = s.ball(3).unwrap();
    assert_eq!(b.mode, MODE_STOP);
    assert_eq!(b.speed_fraction, 0.0);
    assert_eq!(b.follow_id, 0);
    assert_eq!(b.launcher, 0);
    s.tick(12_600);
    assert!(fired.lock().unwrap().is_empty());
    assert!(rx.try_recv().is_err());
    s.tick(13_000);
    let events = rx.try_recv().unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(name(&events[0]), b"AddBalls");
    assert_eq!(name(&events[1]), b"LaunchMissile");
    let Value::Tuple(parts) = &events[1] else { panic!("event") };
    assert_eq!(parts[0].as_int(), Some(12));
    let b = s.ball(3).unwrap();
    assert_eq!((b.mode, b.follow_id, b.launcher, b.effect_stamp), (MODE_MISSILE, 4, 5, 12));
    assert_eq!(b.speed_fraction, 1.0);
    assert!(fired.lock().unwrap().is_empty());
    s.tick(13_499);
    assert!(fired.lock().unwrap().is_empty());
    s.tick(13_500);
    assert_eq!(*fired.lock().unwrap(), [13_500]);
}

#[test]
fn cloak_visibility_and_tick_lapse_are_batched_per_observer() {
    let mut s = System::default();
    let mut b = ball(1, Some(100), 0.0);
    b.cloak = CLOAK_RESTORE;
    b.cloak_until = crate::client::filetime_from_unix_ms(1000);
    s.insert(b);
    let mut own = watch(&mut s, 1, 1);
    let mut other = watch(&mut s, 2, 2);
    s.broadcast(1, vec![event(0, "private", vec![])]);
    assert_eq!(name(&own.try_recv().unwrap()[0]), b"private");
    assert!(other.try_recv().is_err());
    s.tick(999);
    assert!(own.try_recv().is_err());
    s.tick(1000);
    assert_ne!(s.ball(1).unwrap().cloak, 0);
    s.tick(2000);
    assert_eq!(s.ball(1).unwrap().cloak, 0);
    assert_eq!(name(&own.try_recv().unwrap()[0]), b"UncloakBall");
    assert_eq!(name(&other.try_recv().unwrap()[0]), b"AddBalls");
    assert!(other.try_recv().is_err());
}

#[test]
fn pilot_removal_issues_jump_queue_key() {
    let registry = Arc::new(Pilots { capacity: 1, ..Pilots::default() });
    let (tx, mut rx) = mpsc::unbounded_channel();
    registry.register_pilot(200, tx);
    let mut s = System::new(42, registry, 0);
    s.insert(ball(1, Some(100), 0.0));
    assert_eq!(s.request_entry(200), Err(1));
    rx.try_recv().unwrap();
    s.remove_ball(1).unwrap();
    let (name, args) = rx.try_recv().unwrap();
    assert_eq!(name, "OnJumpQueueUpdate");
    let Value::Dict(keys) = &args[3] else { panic!("jump keys") };
    assert_eq!(keys[0].0.as_int(), Some(200));
    assert_eq!(s.request_entry(200), Ok(()));
}

#[test]
fn closed_observers_are_pruned_even_when_cold() {
    let mut s = System::default();
    drop(watch(&mut s, 1, 1));
    s.tick(100);
    assert!(s.observers.is_empty());
    drop(watch(&mut s, 2, 2));
    s.broadcast(0, vec![event(0, "test", vec![])]);
    assert!(s.observers.is_empty());
}

#[test]
fn warp_advances_on_logical_second_boundaries() {
    let mut s = System::default();
    let mut b = ball(1, None, 0.0);
    b.vel = [100.0, 0.0, 0.0];
    assert_eq!(b.warp_to([1_000_000.0, 0.0, 0.0], 0.0, 3000), WarpOutcome::Align);
    s.insert(b);
    s.tick(600);
    assert_eq!(s.ball(1).unwrap().effect_stamp, -1);
    s.tick(1000);
    assert!(s.ball(1).unwrap().is_warping());
    assert_eq!(s.ball(1).unwrap().effect_stamp, 0);
}

#[test]
fn set_state_contains_visible_balls_ego_damage_and_system() {
    let mut s = System::new(42, Arc::default(), 0);
    s.insert(ball(1, Some(100), 0.0));
    let mut hidden = ball(2, Some(200), 1.0);
    hidden.cloak = CLOAK_RESTORE;
    s.insert(hidden);
    let (tx, _rx) = mpsc::unbounded_channel();
    let state = s.observe(1, 1, tx);
    assert_eq!(name(&state), b"SetState");
    let Value::Tuple(parts) = state else { panic!("event") };
    let Value::Tuple(call) = &parts[1] else { panic!("call") };
    let Value::Tuple(args) = &call[1] else { panic!("args") };
    let Value::Instance(_, bag) = &args[0] else { panic!("bag") };
    let Value::Dict(fields) = &**bag else { panic!("fields") };
    let field = |key: &str| &fields.iter().find(|(k, _)| k == &Value::str(key)).unwrap().1;
    assert_eq!(field("ego").as_int(), Some(1));
    let Value::List(slims) = field("slims") else { panic!("slims") };
    assert_eq!(slims.len(), 1);
    let Value::Dict(damage) = field("damageState") else { panic!("damage") };
    assert_eq!(damage.len(), 1);
    assert_eq!(damage[0].0.as_int(), Some(1));
    let Value::Bytes(bytes) = field("state") else { panic!("state") };
    assert_eq!(bytes[0], DESTINY_FULLSTATE);
    assert_eq!(i64::from_le_bytes(bytes[5..13].try_into().unwrap()), 1);
    let Value::Instance(_, sol) = field("solItem") else { panic!("system") };
    let Value::Dict(sol) = &**sol else { panic!("system fields") };
    assert!(sol.iter().any(|(k,v)| k == &Value::str("itemID") && v.as_int() == Some(42)));
}

#[test]
fn constant_target_integration_is_step_size_independent() {
    let mut one = ball(1, None, 7.0);
    one.goto_direction([1.0, 2.0, 3.0]);
    one.speed_fraction = 0.75;
    one.vel = [-3.0, 4.0, 2.0];
    let mut ten = one.clone();
    let positions = HashMap::new();
    one.evolve_behavior_for_ball(1000, 1000, &positions, 0);
    one.evolve_position(1000, integration_factor(one.mass * one.agility as f64, DESTINY_FRICTION));
    for ms in (100..=1000).step_by(100) {
        ten.evolve_behavior_for_ball(ms, ms, &positions, 0);
        ten.evolve_position(ms, integration_factor(ten.mass * ten.agility as f64, DESTINY_FRICTION));
    }
    for (a, b) in one.pos.into_iter().zip(ten.pos).chain(one.vel.into_iter().zip(ten.vel)) {
        assert!((a - b).abs() <= 1e-6 * a.abs().max(1.0));
    }
}

#[test]
fn timers_fire_by_deadline_not_insertion_order_without_firing_early() {
    crate::tests::init_config();
    let mut s = System::default();
    let fired = Arc::new(Mutex::new(Vec::new()));
    s.tick(1000);
    for (delay, label) in [(250, 3), (101, 2), (50, 1)] {
        let fired = fired.clone();
        s.schedule(delay, move |s| fired.lock().unwrap().push((label, s.ms)));
    }
    s.tick(1000);
    assert!(fired.lock().unwrap().is_empty());
    s.tick(1100);
    assert_eq!(*fired.lock().unwrap(), [(1, 1100)]);
    s.tick(1300);
    assert_eq!(*fired.lock().unwrap(), [(1, 1100), (2, 1300), (3, 1300)]);
    s.tick(1400);
    assert_eq!(fired.lock().unwrap().len(), 3);
}

#[test]
fn same_due_timers_fire_in_scheduling_order_including_nested_schedule() {
    crate::tests::init_config();
    let mut s = System::default();
    let fired = Arc::new(Mutex::new(Vec::new()));
    let first = fired.clone();
    s.schedule(100, move |s| {
        first.lock().unwrap().push(1);
        s.schedule(0, move |_| first.lock().unwrap().push(3));
    });
    let second = fired.clone();
    s.schedule(100, move |_| second.lock().unwrap().push(2));
    s.tick(100);
    assert_eq!(*fired.lock().unwrap(), [1, 2, 3]);
}

#[test]
fn timer_for_removed_ball_is_harmless_and_other_timers_still_fire() {
    let mut s = System::default();
    s.insert(ball(1, None, 0.0));
    s.schedule(100, |s| {
        assert!(s.destroy_ball(1).is_none());
        assert!(!s.contains(1));
    });
    s.schedule(100, |s| s.insert(ball(2, None, 0.0)));
    s.remove_ball(1).unwrap();
    s.tick(100);
    assert!(s.contains(2));
    assert!(s.timers.is_empty());
}

#[test]
fn future_timer_allows_cold_skip_until_due() {
    crate::tests::init_config();
    let mut s = System::default();
    s.schedule(1000, |s| assert_eq!(s.ms, 1000));
    assert!(s.is_cold(true));
    s.tick(100);
    assert_eq!(s.ms, 100);
    assert!(s.is_cold(true));
    s.tick(1000);
    assert!(s.timers.is_empty());
    assert!(s.is_cold(true));
}

#[test]
fn absolute_deadlines_preserve_cadence_and_tie_order() {
    crate::tests::init_config();
    let mut s = System::default();
    let result = Arc::new(Mutex::new(Vec::new()));
    let first = result.clone();
    s.schedule_at(150, move |s| {
        first.lock().unwrap().push((1, s.now_ms()));
        s.schedule_at(300, move |s| first.lock().unwrap().push((3, s.now_ms())));
    });
    let second = result.clone();
    s.schedule_at(150, move |s| second.lock().unwrap().push((2, s.now_ms())));
    s.tick(200);
    s.tick(300);
    assert_eq!(*result.lock().unwrap(), [(1, 200), (2, 200), (3, 300)]);
}

#[test]
fn catchup_clamps_wall_delay_without_rebasing_logical_time() {
    let mut s = System::new(0, Arc::default(), 10_500);
    let mut b = ball(1, None, 0.0);
    b.vel = [1.0, 0.0, 0.0];
    s.insert(b);
    s.tick(15_000);
    assert_eq!(s.current_time(), 15);
    let before = s.ball(1).unwrap().pos[0];
    s.tick(30_000);
    assert_eq!(s.current_time(), 16);
    let b = s.ball(1).unwrap();
    assert_eq!(b.last_ms, 16_000);
    assert_eq!(b.interpolation_anchor_ms, 30_000);
    assert_eq!(b.old_pos[0], before);
    assert!(b.pos[0] > before);
    assert_eq!(b.position_at(29_999), b.old_pos);
    assert_eq!(b.position_at(31_001), b.pos);
    let midpoint = integrate(b.old_pos, b.old_vel, [0.0; 3], 1e6, DESTINY_FRICTION, 0.0, 0.5).0;
    assert_ne!(midpoint, b.pos);
    s.tick(30_500);
    assert_eq!(s.sample_position(1), Some(midpoint));
    s.tick(31_000);
    assert_eq!(s.current_time(), 17);
    let b = s.ball(1).unwrap();
    assert_eq!(b.last_ms, 17_000);
    assert_eq!(b.interpolation_anchor_ms, 31_000);
    let midpoint = integrate(b.old_pos, b.old_vel, [0.0; 3], 1e6, DESTINY_FRICTION, 0.0, 0.5).0;
    assert_ne!(midpoint, b.pos);
    s.tick(31_500);
    assert_eq!(s.sample_position(1), Some(midpoint));
}

#[test]
fn nonaligned_stall_resumes_on_next_wall_second() {
    let mut s = System::new(0, Arc::default(), 10_500);
    let mut b = ball(1, None, 0.0);
    b.vel = [1.0, 0.0, 0.0];
    s.insert(b);
    let mut rx = watch(&mut s, 1, 1);
    s.tick(15_000);
    s.tick(30_123);
    assert_eq!(s.current_time(), 16);
    let b = s.ball(1).unwrap();
    assert_eq!(b.last_ms, 16_000);
    assert_eq!(b.interpolation_anchor_ms, 30_123);
    let midpoint = integrate(b.old_pos, b.old_vel, [0.0; 3], 1e6, DESTINY_FRICTION, 0.0, 0.5).0;
    assert_ne!(midpoint, b.old_pos);
    s.tick(30_623);
    assert_eq!(s.sample_position(1), Some(midpoint));
    s.command(1, |b, stamp, _| {
        b.vel = [2.0, 0.0, 0.0];
        vec![event(stamp, "GotoDirection", vec![Value::Int(b.id)])]
    });
    let before = s.ball(1).unwrap().pos;
    s.tick(30_999);
    assert_eq!(s.current_time(), 16);
    assert_eq!(s.ball(1).unwrap().pos, before);
    assert!(rx.try_recv().is_err());
    s.tick(31_000);
    assert_eq!(s.current_time(), 17);
    let b = s.ball(1).unwrap();
    assert_eq!(b.last_ms, 17_000);
    assert_eq!(b.interpolation_anchor_ms, 31_000);
    assert_eq!(b.old_pos, before);
    assert!(b.pos[0] > before[0]);
    let events = rx.try_recv().unwrap();
    let Value::Tuple(parts) = &events[0] else { panic!("event") };
    assert_eq!(parts[0].as_int(), Some(16));
    assert_eq!(name(&events[0]), b"GotoDirection");
}

#[test]
fn ordinary_catchup_anchors_to_last_deadline() {
    let mut s = System::new(0, Arc::default(), 0);
    let mut b = ball(1, None, 0.0);
    b.vel = [10.0, 0.0, 0.0];
    s.insert(b);
    s.tick(2_500);
    let b = s.ball(1).unwrap();
    assert_eq!(s.current_time(), 2);
    assert_eq!(b.interpolation_anchor_ms, 2_000);
    let expected = integrate(b.old_pos, b.old_vel, [0.0; 3], 1e6, DESTINY_FRICTION, 0.0, 0.5).0;
    assert_eq!(s.sample_position(1), Some(expected));
    assert_ne!(expected, b.old_pos);
    assert_eq!(b.position_at(1_999), b.old_pos);
    assert_eq!(b.position_at(3_001), b.pos);
}

#[test]
fn membership_and_uncloak_are_fifo_and_captured() {
    let mut s = System::new(0, Arc::default(), 5_200);
    let mut rx = watch(&mut s, 1, 99);
    let mut b = ball(1, None, 0.0);
    b.last_ms = 999_999;
    s.add_ship(b);
    assert_eq!(s.ball(1).unwrap().last_ms, 5_000);
    s.remove_ball(1).unwrap();
    let mut hidden = ball(2, None, 0.0);
    hidden.cloak = CLOAK_RESTORE;
    s.add_ship(hidden);
    assert!(s.uncloak(2));
    assert!(!s.uncloak(2));
    assert_ne!(s.ball(2).unwrap().cloak, 0);
    assert!(rx.try_recv().is_err());
    s.tick(6_000);
    let events = rx.try_recv().unwrap();
    assert_eq!(events.iter().map(name).collect::<Vec<_>>(), vec![b"AddBalls".as_slice(), b"RemoveBalls", b"AddBalls", b"OnSpecialFX"]);
    for event in events {
        let Value::Tuple(parts) = event else { panic!("event") };
        assert_eq!(parts[0].as_int(), Some(5));
    }
    assert_eq!(s.ball(2).unwrap().cloak, 0);
}

fn queue_warp(s: &mut System, point: V3) {
    s.command(1, move |b, _, _| { b.warp_to(point, 0.0, 3000); Vec::new() });
}

#[test]
fn warp_exit_fires_once_after_stop_and_defers_work() {
    let mut s = System::new(0, Arc::default(), 0);
    let mut b = ball(1, None, 0.0);
    b.vel = [100.0, 0.0, 0.0];
    s.insert(b);
    let (jobs, mut worker) = mpsc::unbounded_channel();
    s.deferred_jobs = Some(jobs);
    let (tx, mut rx) = mpsc::unbounded_channel();
    queue_warp(&mut s, [1_000_000.0, 0.0, 0.0]);
    s.on_warp_exit(1, move |s| {
        let b = s.ball(1).unwrap();
        assert_eq!(b.mode, MODE_STOP);
        assert!(b.pos[0] > 900_000.0);
        s.defer(async move { tx.send(()).unwrap(); });
    });
    for tick in 1..=100 {
        s.tick(tick * 1000);
        if s.ball(1).unwrap().mode == MODE_STOP { break; }
    }
    assert_eq!(s.ball(1).unwrap().mode, MODE_STOP);
    assert!(rx.try_recv().is_err());
    let job = worker.try_recv().unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
    runtime.block_on(job);
    assert_eq!(rx.try_recv(), Ok(()));
    s.tick(101_000);
    assert!(worker.try_recv().is_err());
}

#[test]
fn short_warp_callback_runs_at_fifo_boundary() {
    let mut s = System::new(0, Arc::default(), 0);
    s.insert(ball(1, None, 0.0));
    let (tx, mut rx) = mpsc::unbounded_channel();
    queue_warp(&mut s, [1000.0, 0.0, 0.0]);
    s.on_warp_exit(1, move |s| {
        assert_eq!(s.ball(1).unwrap().mode, MODE_GOTO);
        tx.send(()).unwrap();
    });
    assert!(rx.try_recv().is_err());
    s.tick(1000);
    assert_eq!(rx.try_recv(), Ok(()));
}

#[test]
fn warp_intent_is_replaced_cleared_or_cancelled_but_not_by_property_changes() {
    let mut s = System::new(0, Arc::default(), 0);
    s.insert(ball(1, None, 0.0));
    let target = [1_000_000.0, 0.0, 0.0];
    queue_warp(&mut s, target);
    s.on_warp_exit(1, |_| {});
    s.tick(1000);
    assert!(s.warp_exits.contains_key(&1));
    s.command(1, |b, _, _| { b.set_max_speed(200.0); Vec::new() });
    s.tick(2000);
    assert!(s.warp_exits.contains_key(&1));
    queue_warp(&mut s, [2_000_000.0, 0.0, 0.0]);
    s.on_warp_exit(1, |_| {});
    s.tick(3000);
    assert!(s.warp_exits.contains_key(&1));
    s.clear_warp_exit(1);
    queue_warp(&mut s, [3_000_000.0, 0.0, 0.0]);
    s.tick(4000);
    assert!(!s.warp_exits.contains_key(&1));
    s.on_warp_exit(1, |_| {});
    s.tick(5000);
    s.command(1, |b, _, _| { b.goto_point([10.0, 0.0, 0.0]); Vec::new() });
    s.tick(6000);
    assert!(!s.warp_exits.contains_key(&1));
    queue_warp(&mut s, target);
    s.on_warp_exit(1, |_| {});
    s.tick(7000);
    assert!(s.warp_exits.contains_key(&1));
    s.take(1);
    assert!(!s.warp_exits.contains_key(&1));
}

#[test]
fn activating_warp_releases_followers_before_evolution() {
    let mut s = System::new(0, Arc::default(), 0);
    let mut target = ball(1, None, 0.0);
    target.vel = [100.0, 0.0, 0.0];
    target.warp_to([1_000_000.0, 0.0, 0.0], 0.0, 3000);
    let mut follower = ball(2, None, -1000.0);
    follower.mode = MODE_FOLLOW;
    follower.follow_id = 1;
    s.insert(target);
    s.insert(follower);
    s.tick(1000);
    assert!(s.ball(1).unwrap().is_warping());
    assert_eq!(s.ball(2).unwrap().mode, MODE_STOP);
}

#[test]
fn changed_warp_request_discards_old_callback_without_explicit_clear() {
    let mut s = System::new(0, Arc::default(), 0);
    s.insert(ball(1, None, 0.0));
    queue_warp(&mut s, [1_000_000.0, 0.0, 0.0]);
    s.on_warp_exit(1, |_| panic!("stale callback"));
    s.tick(1000);
    queue_warp(&mut s, [2_000_000.0, 0.0, 0.0]);
    s.tick(2000);
    assert!(!s.warp_exits.contains_key(&1));
}

#[test]
fn warp_activation_releases_missiles_on_both_sides_of_sorted_target() {
    let mut s = System::new(0, Arc::default(), 0);
    let mut target = ball(2, None, 0.0);
    target.owner_id = 7;
    target.speed_fraction = 1.0;
    target.vel = [100.0, 0.0, 0.0];
    target.warp_factor = 3000;
    assert_eq!(target.warp_to([1_000_000.0, 0.0, 0.0], 0.0, 3000), WarpOutcome::Align);
    s.insert(target);
    for id in [1, 3] {
        let mut missile = ball(id, None, 0.0);
        missile.owner_id = 7;
        missile.speed_fraction = 1.0;
        missile.vel = [100.0, 0.0, 0.0];
        missile.warp_factor = 3000;
        missile.mode = MODE_MISSILE;
        missile.follow_id = 2;
        missile.follow_range = -2.0;
        missile.launcher = 9;
        missile.effect_stamp = 42;
        missile.goto = [1e16, 0.0, 0.0];
        s.insert(missile);
    }
    s.tick(1000);
    assert!(s.ball(2).unwrap().is_warping());
    for id in [1, 3] {
        let missile = s.ball(id).unwrap();
        assert_eq!(missile.mode, MODE_GOTO);
        assert!(!missile.massive);
        assert_eq!((missile.effect_stamp, missile.follow_id, missile.launcher, missile.follow_range), (0, 0, 0, 0.0));
        assert_eq!(missile.owner_id, 7);
    }
}

#[test]
fn warp_exit_callback_sees_completed_extra_step_and_fractional_segment() {
    let mut s = System::new(0, Arc::default(), 0);
    let mut ship = ball(1, None, 0.0);
    ship.owner_id = 7;
    ship.speed_fraction = 1.0;
    ship.vel = [100.0, 0.0, 0.0];
    ship.warp_factor = 3000;
    s.insert(ship);
    let mut other = ball(2, None, 0.0);
    other.owner_id = 7;
    other.speed_fraction = 1.0;
    other.warp_factor = 3000;
    other.vel = [20.0, 0.0, 0.0];
    s.insert(other);
    s.command(1, |b, _, _| { b.warp_to([1_000_000.0, 0.0, 0.0], 0.0, 3000); Vec::new() });
    let (tx, rx) = std::sync::mpsc::channel();
    s.on_warp_exit(1, move |s| {
        let b = s.ball(1).unwrap();
        assert_eq!(b.mode, MODE_STOP);
        let mass = b.mass * b.agility as f64;
        assert_eq!((b.pos, b.vel), integrate(b.old_pos, b.old_vel, [0.0; 3], mass, DESTINY_FRICTION, integration_factor(mass, DESTINY_FRICTION), 1.0));
        assert_eq!(b.position_at(b.interpolation_anchor_ms + 500),
            integrate(b.old_pos, b.old_vel, [0.0; 3], mass, DESTINY_FRICTION, 0.0, 0.5).0);
        assert_eq!(s.ball(2).unwrap().last_ms, b.last_ms);
        tx.send(()).unwrap();
    });
    for tick in 1..=100 {
        s.tick(tick * 1000);
        if s.ball(1).unwrap().mode == MODE_STOP { break; }
    }
    assert_eq!(rx.try_recv(), Ok(()));
}
