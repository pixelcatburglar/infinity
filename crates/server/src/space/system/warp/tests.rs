use super::*;
use crate::space::{MODE_GOTO, MODE_WARP};

fn ship() -> Ball {
    let mut b = Ball {
        id: 123, type_id: 456, owner_id: 789, corp_id: 12, alliance_id: 34,
        char_id: None, mode: MODE_GOTO, radius: 50.0, signature_radius: None,
        mass: 1_000_000.0, max_vel: 200.0, agility: 3.0, speed_fraction: 0.0,
        pos: [10.0, 20.0, 30.0], vel: [4.0, 5.0, 6.0],
        old_pos: [1.0, 2.0, 3.0], old_vel: [7.0, 8.0, 9.0],
        segment_acceleration: Some([11.0, 12.0, 13.0]),
        goto: [0.0; 3], follow_id: 0, follow_range: 0.0,
        free: true, global: false, massive: false, interactive: false,
        warp: crate::space::WarpState::default(),
        last_ms: 1000, interpolation_anchor_ms: 500, effect_stamp: 42,
        cloak: 0, cloak_until: 0, health: [1.0; 3], launcher: 0, slim: vec![],
    };
    b.warp.length = 20.0;
    b.warp.min_range = 10.0;
    b.warp.factor = 3;
    b
}
#[test]
fn short_warp_uses_raw_point_and_preserves_motion_state() {
    let point = [100_009.0, 20.0, 30.0];
    for (object, min_range, throttle) in [
        (None, -5000.0, 0.0),
        (None, 5000.0, 0.4),
        (Some(10_000.0), 5000.0, 0.4),
    ] {
        let mut b = ship();
        b.speed_fraction = throttle;
        let (outcome, events) = warp_command(&mut b, 77, 3000, WarpDestination::Ordinary { point, object_radius: object, min_range });
        assert_eq!(outcome, WarpOutcome::Approach);
        assert_eq!(b.mode, MODE_GOTO);
        assert_eq!(b.goto, point);
        assert!(!b.is_warping());
        assert_eq!((b.warp.length, b.warp.min_range, b.warp.factor), (20.0, 10.0, 3));
        assert_eq!(b.speed_fraction, if throttle == 0.0 { 1.0 } else { throttle });
        assert_eq!(b.pos, [10.0, 20.0, 30.0]);
        assert_eq!(b.vel, [4.0, 5.0, 6.0]);
        assert_eq!(b.old_pos, [1.0, 2.0, 3.0]);
        assert_eq!(b.old_vel, [7.0, 8.0, 9.0]);
        assert_eq!(b.segment_acceleration, Some([11.0, 12.0, 13.0]));
        assert_eq!(b.last_ms, 1000);
        assert_eq!(b.interpolation_anchor_ms, 500);
        assert_eq!(b.effect_stamp, 42);
        assert_eq!(events, vec![event(77, "GotoPoint", vec![Value::Int(123), Value::Float(point[0]), Value::Float(point[1]), Value::Float(point[2])])]);
    }
}

#[test]
fn warp_threshold_uses_unadjusted_distance_and_keeps_long_warp_events() {
    for (distance, min_range) in [(100_000.0, 0.0), (100_001.0, 5000.0)] {
        let mut b = ship();
        let point = [b.pos[0] + distance, b.pos[1], b.pos[2]];
        let (outcome, events) = warp_command(&mut b, 77, 3000, WarpDestination::Ordinary { point, object_radius: None, min_range });
        assert_eq!(outcome, WarpOutcome::Align);
        assert_eq!(b.mode, MODE_WARP);
        assert_eq!((b.warp.length, b.warp.min_range, b.warp.factor), (20.0, min_range, 3000));
        assert_eq!(b.goto, point);
        assert_eq!(events, vec![
            event(77, "WarpTo", vec![Value::Int(123), Value::Float(point[0]), Value::Float(point[1]), Value::Float(point[2]), Value::Float(min_range), Value::Int(3000)]),
            event(77, "SetSpeedFraction", vec![Value::Int(123), Value::Float(1.0)]),
            special_fx(77, 123, None, None, None, "effects.Warping", false, 1, 0,
                Value::Int(SPECIAL_FX_UNSPECIFIED_DURATION), 0, None, 0, Value::None),
        ]);
    }
}

#[test]
fn destination_validation_rejects_nonfinite_geometry() {
    for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert!(!WarpDestination::Exact([value, 0.0, 0.0]).is_valid());
        assert!(!WarpDestination::Ordinary { point: [0.0; 3], object_radius: Some(value), min_range: 0.0 }.is_valid());
        assert!(!WarpDestination::Ordinary { point: [0.0; 3], object_radius: None, min_range: value }.is_valid());
    }
}

#[test]
fn ordinary_standoff_uses_object_clearance_or_clamped_point_range() {
    for (object_radius, min_range, expected) in [
        (Some(10_000.0), 5_000.0, 10_000.0 + 50.0 + 5_000.0),
        (Some(10_000.0), -5_000.0, 10_000.0 + 50.0 + 2_500.0),
        (None, 5_000.0, 5_000.0),
        (None, -5_000.0, 0.0),
    ] {
        assert_eq!(ordinary_warp_standoff(object_radius, 50.0, min_range), expected);
    }
}

#[test]
fn a_live_fitted_mode_refuses_a_prequeued_positive_cost_warp_without_debit() {
    use crate::services::combat::groups_health::{self, Condition, ShipStats};
    use crate::services::fit::attributes::tests::module_fit;
    use crate::services::fit::Key;
    use infinity_core::consts::dogma::{ATTR_CAPACITOR_CAPACITY, ATTR_RECHARGE_RATE, ATTR_WARP_CAPACITOR_NEED};
    crate::tests::init_config();
    let mut body = ship();
    body.mode = MODE_STOP; body.effect_stamp = 0; body.vel = [body.max_vel as f64, 0.0, 0.0];
    let mut s = System::new(0, std::sync::Arc::default(), 12_345);
    s.insert(body);
    let mut fit = module_fit(crate::sde::types::tests::combat_fixture(), 456, 1, true);
    let patch = crate::services::fit::attributes::CommittedFitting::overridden_attributes(vec![(Key::Ship, ATTR_WARP_CAPACITOR_NEED, Some(0.000001)), (Key::Ship, ATTR_CAPACITOR_CAPACITY, Some(100.0)), (Key::Ship, ATTR_RECHARGE_RATE, Some(1_000.0))]);
    assert!(fit.apply_committed_fitting(&patch) && fit.value(Key::Ship, ATTR_WARP_CAPACITOR_NEED) > 0.0);
    let stats = ShipStats::from_fit(&fit, Key::Ship);
    s.install_condition(123, Condition { now: stats.max, stats, shield_at: crate::client::filetime_now() }, fit);
    assert!(s.queue_warp(123, 3000, WarpDestination::Exact([infinity_core::consts::AU, 20.0, 30.0]), None));
    s.apply_scene_prefix(false);
    assert!(s.actor_combat.get_mut(&123).expect("combat").fit.set_module_active(2, true));
    let generation = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let membership = s.combat_membership_generation(123).unwrap_or(0);
    crate::services::tests::fixture::install_running_mode(2, 123, 789, i64::from(infinity_core::consts::dogma::EFFECT_BASTION), membership, &generation);
    let at = crate::client::filetime_now();
    let (_, before) = groups_health::fill_capacitor(123, 100.0, 1_000.0, at);
    s.tick(13_345);
    assert_eq!(groups_health::fill_capacitor(123, 100.0, 1_000.0, at).0, before, "recharge accounted, no debit");
    assert_eq!(s.ball(123).unwrap().mode, MODE_STOP);
    assert!(!s.ball(123).unwrap().is_warping(), "no Warp start");
    crate::services::effects::active_cycle::finish_module(&mut s, 123, 2);
    crate::services::combat::forget_ship(123);
}
