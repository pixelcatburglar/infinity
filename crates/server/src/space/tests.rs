use super::*;

fn session(character: i64, session_id: i64) -> infinity_core::session::OnlineSession {
    infinity_core::session::OnlineSession { character_id: character, user_id: None, session_id, ship_id: None,
        station_id: None, system_id: None, corporation_id: None, alliance_id: None, faction_id: None,
        fleet_id: None, role: 0, corporation_role: 0 }
}

#[test]
fn presence_tracks_live_replaced_and_removed_channels() {
    let pilots = Pilots::default();
    let (first, first_rx) = tokio::sync::mpsc::unbounded_channel();
    let (closed, closed_rx) = tokio::sync::mpsc::unbounded_channel();
    pilots.register_pilot(1, first);
    pilots.register_pilot(2, closed);
    drop(closed_rx);
    assert!(pilots.is_online(1));
    assert!(!pilots.is_online(2));
    assert_eq!(pilots.online_pilots(), vec![1]);
    let (replacement, replacement_rx) = tokio::sync::mpsc::unbounded_channel();
    pilots.register_pilot(1, replacement);
    drop(first_rx);
    assert!(pilots.is_online(1));
    assert_eq!(pilots.online_pilots(), vec![1]);
    drop(replacement_rx);
    assert!(!pilots.is_online(1));
    assert!(pilots.online_pilots().is_empty());
    let (new_tx, _new_rx) = tokio::sync::mpsc::unbounded_channel();
    pilots.register_pilot(1, new_tx);
    pilots.unregister_pilot_generation(1, None);
    assert!(!pilots.is_online(1));
    assert!(pilots.online_pilots().is_empty());
}

#[test]
fn stale_generation_cannot_unregister_replacement() {
    let pilots = Pilots::default();
    let (old_tx, old_rx) = tokio::sync::mpsc::unbounded_channel();
    pilots.register_pilot(1, old_tx);
    pilots.set_online_session(1, Some(session(1, 2)));
    drop(old_rx);
    let (new_tx, _new_rx) = tokio::sync::mpsc::unbounded_channel();
    pilots.register_pilot(1, new_tx);
    pilots.set_online_session(1, Some(session(1, 3)));
    assert_eq!(pilots.online_sessions(), vec![session(1, 3)]);
    assert!(!pilots.unregister_pilot_generation(1, Some(2)));
    assert_eq!(pilots.online_session(1).unwrap().session_id, 3);
    assert_eq!(pilots.online_sessions(), vec![session(1, 3)]);
    assert!(pilots.unregister_pilot_generation(1, Some(3)));
    assert!(!pilots.unregister_pilot_generation(1, Some(3)));
}

#[test]
fn stale_session_update_cannot_replace_live_generation() {
    let pilots = Pilots::default();
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    pilots.register_pilot(1, tx);
    let session = |sid, ship| infinity_core::session::OnlineSession {
        character_id: 1, user_id: Some(9), session_id: sid, ship_id: Some(ship),
        station_id: None, system_id: None, corporation_id: None, alliance_id: None,
        faction_id: None, fleet_id: None, role: 0, corporation_role: 0,
    };
    pilots.set_online_session(1, Some(session(2, 20)));
    pilots.set_online_session(1, Some(session(1, 10)));
    assert_eq!(pilots.online_session(1), Some(session(2, 20)));
    pilots.set_online_session(1, None);
    assert_eq!(pilots.online_session(1), Some(session(2, 20)));
}

#[test]
fn station_guests_only_include_live_matching_sessions() {
    let pilots = Pilots::default();
    let session = |id, station| infinity_core::session::OnlineSession {
        character_id: id, user_id: None, session_id: 0, ship_id: None, station_id: station, system_id: None, corporation_id: None,
        alliance_id: None, faction_id: None, fleet_id: None, role: 0, corporation_role: 0,
    };
    let mut receivers = Vec::new();
    for (key, value) in [(1, Some(session(1, Some(10)))), (2, Some(session(2, Some(20)))),
        (3, None), (4, Some(session(5, Some(10)))), (5, Some(session(5, Some(10))))] {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        pilots.register_pilot(key, tx);
        pilots.set_online_session(key, value);
        receivers.push(rx);
    }
    drop(receivers.pop());
    assert_eq!(pilots.station_guests(10), vec![session(1, Some(10))]);
    assert_eq!(pilots.station_guests(20), vec![session(2, Some(20))]);
    let mut online = pilots.online_sessions();
    online.sort_by_key(|s| s.character_id);
    assert_eq!(online, vec![session(1, Some(10)), session(2, Some(20))]);
    pilots.unregister_pilot_generation(2, None);
    assert_eq!(pilots.online_sessions(), vec![session(1, Some(10))]);
    pilots.unregister_pilot_generation(1, None);
    assert!(pilots.station_guests(10).is_empty());
}

#[test]
fn integrate_zero_time_preserves_state_before_division() {
    let position = [1.0, -2.0, 3.0];
    let velocity = [4.0, 5.0, -6.0];
    assert_eq!(integrate(position, velocity, [1.0; 3], 0.0, 0.0, 0.0, 0.0), (position, velocity));
}
#[test]
fn integrate_uses_full_factor_only_for_whole_tick() {
    let velocity = [2.0, 0.0, 0.0];
    let full = integrate([0.0; 3], velocity, [0.0; 3], 1.0, 1.0, 0.25, 1.0);
    assert_eq!(full.1[0], 0.5);
    let partial = integrate([0.0; 3], velocity, [0.0; 3], 1.0, 1.0, 0.25, 0.5);
    assert_eq!(partial.1[0], 2.0 * (-0.5_f64).exp());
}


#[test]
fn integrate_taylor_low_drag_approaches_linear_motion() {
    let velocity = [2.0, -3.0, 4.0];
    let (position, actual_velocity) = integrate([0.0; 3], velocity, [0.0; 3], 1.0, 1e-11, integration_factor(1.0, 1e-11), 1.0);
    for i in 0..3 {
        assert!((position[i] - velocity[i]).abs() < 1e-9);
        assert!((actual_velocity[i] - velocity[i]).abs() < 1e-9);
    }
}
#[test]
fn seconds_to_milliseconds_preserves_fractions() {
    assert_eq!(seconds_to_ms_f64(0.0015), 1.5);
    assert_eq!(seconds_to_ms_f64(-0.0015), -1.5);
    assert_eq!(seconds_to_ms(0.0015), 1);
}


#[test]
fn lock_time_clamps_floors_and_calculates_interior() {
    crate::tests::init_config();
    assert_eq!(lock_time_ms(1.0, 1.0), 60_000);
    assert_eq!(lock_time_ms(1e9, 1e9), 500);
    assert_eq!(lock_time_ms(-10.0, 0.0), lock_time_ms(1.0, 1.0));
    let expected = (40_000.0 / (200.0 * 50.0_f64.asinh().powi(2)) * 1000.0) as u64;
    assert_eq!(lock_time_ms(200.0, 50.0), expected);
}

fn effect_stamp_balls() -> (Ball, Ball) {
    let missile = Ball {
        id: 123, type_id: 456, owner_id: 789, corp_id: 12, alliance_id: 34,
        char_id: None, mode: MODE_MISSILE, radius: 2.5, signature_radius: None,
        mass: 10.0, max_vel: 20.0, agility: 0.5, speed_fraction: 0.75,
        pos: [1.0, 2.0, 3.0], vel: [4.0, 5.0, 6.0], goto: [7.0, 8.0, 9.0],
        old_pos: [1.0, 2.0, 3.0], old_vel: [4.0, 5.0, 6.0], segment_acceleration: None,
        follow_id: 987, follow_range: 42.0, free: true, global: false, massive: false, interactive: false,
        warp: WarpState::default(), last_ms: 1_234_567, interpolation_anchor_ms: 1_234_567, effect_stamp: 1_234, cloak: 0, cloak_until: 0,
        health: [1.0; DAMAGE_LAYER_COUNT], launcher: 654, slim: vec![],
    };
    let mut warp = missile.clone();
    warp.mode = MODE_WARP;
    warp.interactive = true;
    warp.warp.length = 20.0;
    warp.warp.min_range = 10.0;
    warp.warp.factor = 3;
    warp.effect_stamp = 2_345;
    (missile, warp)
}

#[test]
fn missile_and_warp_wire_bytes_match_explicit_interactivity() {
    let (missile, warp) = effect_stamp_balls();
    for (ball, expected) in [
        (missile, "7b000000000000000500002040000000000000f03f0000000000000040000000000000084001000000000000244000ffffffffffffffff0c000000220000000000a0410000000000001040000000000000144000000000000018400000003f0000403fffdb03000000000000000028428e02000000000000d20400000000000000001c4000000000000020400000000000002240"),
        (warp, "7b000000000000000300002040000000000000f03f0000000000000040000000000000084009000000000000244000ffffffffffffffff0c000000220000000000a0410000000000001040000000000000144000000000000018400000003f0000403fff0000000000001c400000000000002040000000000000224029090000000000000000344000000000000024400300000000000000"),
    ] {
        let mut bytes = Vec::new();
        ball.write(&mut bytes, true);
        let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
        assert_eq!(hex, expected);
    }
}

#[test]
fn wire_interactivity_is_independent_of_free() {
    let (mut ball, _) = effect_stamp_balls();
    ball.global = true;
    ball.massive = true;
    for free in [false, true] {
        ball.free = free;
        ball.mode = if free { MODE_STOP } else { MODE_RIGID };
        ball.interactive = false;
        let mut noninteractive = Vec::new();
        ball.write(&mut noninteractive, true);
        ball.interactive = true;
        let mut interactive = Vec::new();
        ball.write(&mut interactive, true);

        let flag_offset = 8 + 1 + 4 + 3 * 8;
        let other_flags = FLAG_GLOBAL | FLAG_MASSIVE | if free { FLAG_FREE } else { 0 };
        assert_eq!(noninteractive[flag_offset], other_flags);
        assert_eq!(interactive[flag_offset], other_flags | FLAG_INTERACTIVE);
        noninteractive[flag_offset] |= FLAG_INTERACTIVE;
        assert_eq!(interactive, noninteractive);
    }
}

#[test]
fn goto_direction_stores_absolute_point_and_serializes_it() {
    let (_, mut ball) = effect_stamp_balls();
    ball.pos = [1e16, 2e16, 3e16];
    ball.goto_direction([0.0, 3.0, 0.0]);
    assert_eq!(ball.mode, MODE_GOTO);
    assert!(!ball.is_warping());
    assert_eq!((ball.warp.length, ball.warp.min_range, ball.warp.factor), (20.0, 10.0, 3));
    assert_eq!(ball.goto, [1e16, 1.2e17, 3e16]);
    let mut bytes = Vec::new();
    ball.write(&mut bytes, true);
    let expected: Vec<u8> = ball.goto.iter().flat_map(|c| c.to_le_bytes()).collect();
    assert_eq!(&bytes[bytes.len() - expected.len()..], expected);

    ball.goto_direction([0.0, 1e-10, 0.0]);
    assert_eq!(ball.goto, [1e16, 1.2e17, 3e16]);
    ball.goto_direction([0.0; 3]);
    assert_eq!(ball.goto, ball.pos);
}

#[test]
fn goto_tick_steers_toward_absolute_point_and_zero_has_no_thrust() {
    let (_, mut ball) = effect_stamp_balls();
    ball.pos = [1e16, 2e16, 3e16];
    ball.vel = [0.0; 3];
    ball.goto_direction([0.0, -2.0, 0.0]);
    let ms = ball.last_ms + 1000;
    ball.evolve_behavior_for_ball(ms, 1000, &HashMap::new(), 0);
    ball.evolve_position(ms, integration_factor(ball.mass * ball.agility as f64, DESTINY_FRICTION));
    let target = ball.segment_acceleration.unwrap();
    assert_eq!(target[0], 0.0);
    assert!((target[1] + 3_000_000.0).abs() < 1e-9);
    assert_eq!(target[2], 0.0);

    ball.vel = [0.0; 3];
    ball.goto_direction([0.0; 3]);
    let ms = ball.last_ms + 1000;
    ball.evolve_behavior_for_ball(ms, 2000, &HashMap::new(), 1);
    ball.evolve_position(ms, integration_factor(ball.mass * ball.agility as f64, DESTINY_FRICTION));
    assert_eq!(ball.segment_acceleration, Some([0.0; 3]));
    assert_eq!(ball.vel, [0.0; 3]);
}

#[test]
fn goto_arrival_and_follow_validation_and_radii() {
    let (mut b, mut target) = effect_stamp_balls();
    b.id = 1;
    b.pos = [0.0; 3];
    b.mass = DESTINY_FRICTION;
    b.agility = 1.0;
    b.max_vel = 20.0;
    b.speed_fraction = 1.0;
    assert!(b.goto_point([10.0, 0.0, 0.0]));
    assert_eq!(b.evolve_goto(), [1.25, 0.0, 0.0]);
    assert!(!b.goto_point([f64::NAN, 0.0, 0.0]));
    target.id = 2;
    target.pos = b.pos;
    target.radius = 5.0;
    b.radius = 2.0;
    target.cloak = 1;
    assert!(!b.follow_ball(&target, 3.0));
    target.cloak = 0;
    assert!(!b.follow_ball(&target, f32::NAN));
    let self_target = b.clone();
    assert!(!b.follow_ball(&self_target, 3.0));
    assert!(b.follow_ball(&target, 3.0));
    b.evolve_follow(&TargetState { pos: target.pos, vel: target.vel, radius: target.radius });
    assert_eq!(b.goto, [10.0, 0.0, 0.0]);
}

#[test]
fn missing_follow_target_stops_and_clears_follow_state() {
    let (mut b, _) = effect_stamp_balls();
    b.mode = MODE_FOLLOW;
    b.follow_id = 9;
    b.follow_range = 12.0;
    let ms = b.last_ms + 1000;
    b.evolve_behavior_for_ball(ms, 1000, &HashMap::new(), 0);
    b.evolve_position(ms, integration_factor(b.mass * b.agility as f64, DESTINY_FRICTION));
    assert_eq!(b.mode, MODE_STOP);
    assert_eq!(b.follow_id, 0);
    assert_eq!(b.follow_range, 0.0);
    assert_eq!(b.segment_acceleration, Some([0.0; 3]));
}

pub(super) mod destiny_reference;
mod kinematics_reference;
mod collision_reference;

