//! Activation pricing and route correction tests with synthetic fitted attributes.
use super::*;
use crate::services::combat::groups_health::{Condition, ShipStats};
use crate::services::fit::attributes::tests::ship_fit;
use crate::space::system::warp::WarpDestination;

fn fixture(id: i64, coefficient: f64, capacity: f64) -> System {
    crate::tests::init_config();
    let mut system = System::new(1, Arc::default(), 0);
    system.insert(Ball {
        id, type_id: 1, owner_id: 1, corp_id: 1, alliance_id: -1,
        char_id: Some(1), signature_radius: Some(1.0), mode: MODE_STOP,
        radius: 1.0, mass: 1_000_000.0, max_vel: 100.0, agility: 1.0,
        speed_fraction: 0.0, pos: [0.0; 3], vel: [100.0, 0.0, 0.0],
        old_pos: [0.0; 3], old_vel: [100.0, 0.0, 0.0], segment_acceleration: None,
        goto: [0.0; 3], follow_id: 0, follow_range: 0.0, free: true, global: false,
        massive: true, interactive: true, warp: WarpState::default(), last_ms: 0,
        interpolation_anchor_ms: 0, effect_stamp: 0, cloak: 0, cloak_until: 0,
        health: [1.0; 3], launcher: 0, slim: Vec::new(),
    });
    let fit = ship_fit(&[(ATTR_WARP_CAPACITOR_NEED, coefficient), (ATTR_CAPACITOR_CAPACITY, capacity), (ATTR_RECHARGE_RATE, 1000.0)]);
    let stats = ShipStats::from_fit(&fit, Key::Ship);
    system.install_condition(id, Condition { now: stats.max, stats, shield_at: crate::client::filetime_now() }, fit);
    system
}

#[test]
fn zero_full_partial_and_empty_activation() {
    for (id, coefficient, capacity, expected) in [
        (-930_001, 0.0, 0.0, 1.0),
        (-930_002, 0.000001, 1.0, 1.0),
        (-930_003, 0.000001, 0.25, 0.25),
        (-930_004, 0.000001, 0.0, 0.0),
    ] {
        let mut system = fixture(id, coefficient, capacity);
        assert!(system.queue_warp(id, 3000, WarpDestination::Exact([AU, 0.0, 0.0]), None));
        system.tick(1000);
        let ball = system.ball(id).unwrap();
        if expected == 0.0 {
            assert_eq!(ball.mode, MODE_STOP);
            assert!(!system.warp_exits.contains_key(&id));
        } else {
            assert!(ball.is_warping());
            assert!((ball.warp.length / AU - expected).abs() < 1e-12);
            assert_eq!(system.warp_exits[&id].full, expected == 1.0);
        }
        crate::services::combat::forget_ship(id);
    }
}

#[test]
fn invalid_cost_cancels_instead_of_free_warp() {
    let id = -930_005;
    let mut system = fixture(id, -1.0, 100.0);
    assert!(system.queue_warp(id, 3000, WarpDestination::Exact([AU, 0.0, 0.0]), None));
    system.tick(1000);
    assert_eq!(system.ball(id).unwrap().mode, MODE_STOP);
    assert!(!system.warp_exits.contains_key(&id));
    crate::services::combat::forget_ship(id);
}

#[test]
fn partial_correction_has_completed_stamp_and_preserves_identity() {
    let id = -930_006;
    let mut system = fixture(id, 0.000001, 0.25);
    let (tx, mut rx) = mpsc::unbounded_channel();
    system.observe(1, id, tx);
    assert!(system.queue_warp(id, 3000, WarpDestination::Exact([AU, 0.0, 0.0]), None));
    system.tick(1000);
    let events: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).flatten().collect();
    let additions: Vec<_> = events.iter().filter_map(|value| {
        let fields = value.items()?;
        let call = fields.get(1)?.items()?;
        (call.first()?.as_text()?.as_ref() == "AddBalls").then_some((fields, call))
    }).collect();
    assert_eq!(additions.len(), 1);
    let (fields, call) = additions[0];
    assert_eq!(fields[0].as_int(), Some(1));
    let args = call[1].items().unwrap();
    let chunk = args[0].items().unwrap();
    let bytes = chunk[0].as_bytes().unwrap();
    assert_eq!(bytes[0], DESTINY_BALLS);
    assert_eq!(i32::from_le_bytes(bytes[1..5].try_into().unwrap()), 1);
    assert_eq!(system.ball(id).unwrap().effect_stamp, 0);
    assert_eq!(system.ball(id).unwrap().warp.factor, 3000);
    assert_eq!(system.warp_corrections, vec![id]);
    crate::services::combat::forget_ship(id);
}

#[test]
fn approach_completes_shared_arrival_at_action_boundary() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let id = -930_007;
    let mut system = fixture(id, 0.000001, 0.0);
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let arrival = system.register_warp_arrival(Box::new(move |s| {
        assert_eq!(s.current_time(), 0);
        observed.fetch_add(1, Ordering::SeqCst);
    }));
    assert!(system.queue_warp(id, 3000, WarpDestination::Exact([1000.0, 0.0, 0.0]), Some(arrival)));
    system.tick(1000);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(system.ball(id).unwrap().mode, MODE_GOTO);
    assert!(system.warp_arrivals.is_empty());
    assert!(!system.retain_warp_arrival(arrival));
    system.release_warp_arrival(arrival);
    crate::services::combat::forget_ship(id);
}

#[test]
fn partial_routes_have_no_minimum_distance() {
    for (id, distance) in [(-930_008, 50_000.0), (-930_009, 0.5), (-930_010, 0.000001)] {
        let mut system = fixture(id, 0.000001, distance / AU);
        assert!(system.queue_warp(id, 3000, WarpDestination::Exact([AU, 0.0, 0.0]), None));
        system.tick(1000);
        let ball = system.ball(id).unwrap();
        assert!(ball.warp.length > 0.0);
        assert!((ball.warp.length - distance).abs() < distance * 1e-12);
        assert_eq!(system.warp_corrections, vec![id]);
        crate::services::combat::forget_ship(id);
    }
}

#[test]
fn partial_member_cannot_consume_full_member_arrival() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let partial = -930_011;
    let full = -930_012;
    let mut system = fixture(partial, 0.000001, 0.25);
    let mut other = fixture(full, 0.000001, 1.0);
    let ball = other.take(full).unwrap();
    system.insert(ball);
    let fit = ship_fit(&[(ATTR_WARP_CAPACITOR_NEED, 0.000001), (ATTR_CAPACITOR_CAPACITY, 1.0), (ATTR_RECHARGE_RATE, 1000.0)]);
    let stats = ShipStats::from_fit(&fit, Key::Ship);
    system.install_condition(full, Condition { now: stats.max, stats, shield_at: crate::client::filetime_now() }, fit);
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let arrival = system.register_warp_arrival(Box::new(move |_| { observed.fetch_add(1, Ordering::SeqCst); }));
    for id in [partial, full] {
        assert!(system.queue_warp(id, 3000, WarpDestination::Exact([AU, 0.0, 0.0]), Some(arrival)));
    }
    system.release_warp_arrival(arrival);
    system.tick(1000);
    assert!(!system.warp_exits[&partial].full);
    assert!(system.warp_exits[&full].full);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    for second in 2..=500 {
        system.tick(second * 1000);
        if !system.warp_exits.contains_key(&partial) && system.warp_exits.contains_key(&full) {
            assert_eq!(calls.load(Ordering::SeqCst), 0);
        }
        if calls.load(Ordering::SeqCst) == 1 { break; }
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(system.warp_arrivals.is_empty());
    crate::services::combat::forget_ship(partial);
    crate::services::combat::forget_ship(full);
}

#[test]
fn object_standoff_is_priced_once_and_exact_is_not_adjusted() {
    for (id, destination, expected) in [
        (-930_013, WarpDestination::Ordinary {
            point: [AU, 0.0, 0.0], object_radius: Some(1000.0), min_range: 5000.0,
        }, AU - 6001.0),
        (-930_014, WarpDestination::Exact([AU, 0.0, 0.0]), AU),
    ] {
        let mut system = fixture(id, 0.000001, 2.0);
        assert!(system.queue_warp(id, 3000, destination, None));
        system.tick(1000);
        assert_eq!(system.ball(id).unwrap().warp.length, expected);
        assert!(system.warp_exits[&id].full);
        crate::services::combat::forget_ship(id);
    }
}

#[test]
fn unaligned_ticks_do_not_debit_and_deadline_charges_once() {
    let id = -930_015;
    let mut system = fixture(id, 0.000001, 0.25);
    system.command(id, |ball, _, _| { ball.vel = [0.0; 3]; ball.max_vel = 0.0; Vec::new() });
    assert!(system.queue_warp(id, 3000, WarpDestination::Exact([AU, 0.0, 0.0]), None));
    system.tick(1000);
    assert!(system.warp_exits[&id].pending);
    assert!(system.warp_corrections.is_empty());
    for second in 2..=180 {
        system.tick(second * 1000);
        assert!(system.warp_exits[&id].pending);
    }
    system.tick(181_000);
    assert!(!system.warp_exits[&id].pending);
    assert_eq!(system.warp_corrections, vec![id]);
    assert_eq!(system.ball(id).unwrap().effect_stamp, 180);
    system.tick(182_000);
    assert!(system.warp_corrections.is_empty());
    crate::services::combat::forget_ship(id);
}

#[test]
fn arbitrarily_small_positive_coefficient_is_not_free() {
    let id = -930_016;
    let mut system = fixture(id, 1e-18, 0.0);
    assert!(system.queue_warp(id, 3000, WarpDestination::Exact([AU, 0.0, 0.0]), None));
    system.tick(1000);
    assert_eq!(system.ball(id).unwrap().mode, MODE_STOP);
    assert!(system.warp_corrections.is_empty());
    assert!(!system.warp_exits.contains_key(&id));
    crate::services::combat::forget_ship(id);
}

#[test]
fn zero_resolved_travel_does_not_charge_or_cancel() {
    let id = -930_017;
    let mut system = fixture(id, 0.000001, 0.0);
    assert!(system.queue_warp(id, 3000, WarpDestination::Ordinary {
        point: [1_000_000.0, 0.0, 0.0], object_radius: None, min_range: 1_000_000.0,
    }, None));
    system.tick(1000);
    assert_eq!(system.ball(id).unwrap().warp.length, 0.0);
    assert!(system.warp_corrections.is_empty());
    crate::services::combat::forget_ship(id);
}

#[test]
fn missing_actor_fit_cancels_without_free_warp() {
    let id = -930_018;
    let mut system = fixture(id, 0.000001, 1.0);
    system.actor_combat.remove(&id);
    assert!(system.queue_warp(id, 3000, WarpDestination::Exact([AU, 0.0, 0.0]), None));
    system.tick(1000);
    assert_eq!(system.ball(id).unwrap().mode, MODE_STOP);
    assert!(!system.warp_exits.contains_key(&id));
    crate::services::combat::forget_ship(id);
}

#[test]
fn replacement_and_stop_release_arrival_without_payment() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let id = -930_019;
    let mut system = fixture(id, 0.000001, 1.0);
    system.command(id, |ball, _, _| { ball.vel = [0.0; 3]; ball.max_vel = 0.0; Vec::new() });
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let arrival = system.register_warp_arrival(Box::new(move |_| { observed.fetch_add(1, Ordering::SeqCst); }));
    assert!(system.queue_warp(id, 3000, WarpDestination::Exact([AU, 0.0, 0.0]), Some(arrival)));
    system.release_warp_arrival(arrival);
    system.tick(1000);
    assert!(system.warp_exits[&id].pending);
    system.command(id, |ball, _, _| { ball.set_max_speed(100.0); Vec::new() });
    system.tick(2000);
    assert!(system.warp_exits.contains_key(&id));
    system.command(id, |ball, _, _| { ball.stop(); Vec::new() });
    system.tick(3000);
    assert!(!system.warp_exits.contains_key(&id));
    assert!(system.warp_arrivals.is_empty());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    crate::services::combat::forget_ship(id);
}

#[test]
fn activation_uses_current_mass_and_fit_not_requested_factor() {
    let id = -930_020;
    let mut system = fixture(id, 0.0, 1.0);
    assert!(system.queue_warp(id, 4320, WarpDestination::Exact([AU, 0.0, 0.0]), None));
    system.actor_combat.get_mut(&id).unwrap().fit = ship_fit(&[
        (ATTR_WARP_CAPACITOR_NEED, 0.000001),
        (ATTR_CAPACITOR_CAPACITY, 1.0),
        (ATTR_RECHARGE_RATE, 2000.0),
    ]);
    system.command(id, |ball, _, _| { ball.set_ball_mass(2_000_000.0); Vec::new() });
    system.tick(1000);
    let ball = system.ball(id).unwrap();
    assert!((ball.warp.length / AU - 0.5).abs() < 1e-12);
    assert_eq!(ball.warp.factor, 4320);
    assert!(!system.warp_exits[&id].full);
    crate::services::combat::forget_ship(id);
}

#[test]
fn shared_arrival_survives_empty_members_and_all_terminal_cases() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    for (case, capacities, expected_calls) in [
        (0, [0.0, 1.0], 1), (1, [1.0, 1.0], 1),
        (2, [0.25, 0.5], 0), (3, [0.0, 0.0], 0),
    ] {
        let first = -930_100 - case * 2;
        let second = first - 1;
        let mut system = fixture(first, 0.000001, capacities[0]);
        let mut other = fixture(second, 0.000001, capacities[1]);
        system.insert(other.take(second).unwrap());
        let fit = ship_fit(&[(ATTR_WARP_CAPACITOR_NEED, 0.000001),
            (ATTR_CAPACITOR_CAPACITY, capacities[1]), (ATTR_RECHARGE_RATE, 1000.0)]);
        let stats = ShipStats::from_fit(&fit, Key::Ship);
        system.install_condition(second, Condition { now: stats.max, stats, shield_at: crate::client::filetime_now() }, fit);
        let calls = Arc::new(AtomicUsize::new(0));
        let observed = calls.clone();
        let id = system.register_warp_arrival(Box::new(move |_| { observed.fetch_add(1, Ordering::SeqCst); }));
        for ship in [first, second] {
            assert!(system.queue_warp(ship, 3000, WarpDestination::Exact([AU, 0.0, 0.0]), Some(id)));
        }
        system.release_warp_arrival(id);
        for second_tick in 1..=500 {
            system.tick(second_tick * 1000);
            if !system.warp_exits.contains_key(&first) && !system.warp_exits.contains_key(&second) { break; }
        }
        assert_eq!(calls.load(Ordering::SeqCst), expected_calls);
        assert!(system.warp_arrivals.is_empty());
        assert!(!system.warp_exits.contains_key(&first) && !system.warp_exits.contains_key(&second));
        crate::services::combat::forget_ship(first);
        crate::services::combat::forget_ship(second);
    }
}

#[test]
fn duplicate_of_shortened_route_cannot_restore_full_arrival() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let ship = -930_110;
    let mut system = fixture(ship, 0.000001, 0.25);
    assert!(system.queue_warp(ship, 3000, WarpDestination::Exact([AU, 0.0, 0.0]), None));
    system.tick(1000);
    let endpoint = system.ball(ship).unwrap().goto;
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let id = system.register_warp_arrival(Box::new(move |_| { observed.fetch_add(1, Ordering::SeqCst); }));
    assert!(system.queue_warp(ship, 4320, WarpDestination::Exact(endpoint), Some(id)));
    system.release_warp_arrival(id);
    system.tick(2000);
    assert!(!system.warp_exits[&ship].full);
    assert!(system.warp_exits[&ship].arrival.is_none());
    assert_eq!(system.ball(ship).unwrap().warp.factor, 3000);
    assert!(system.warp_arrivals.is_empty());
    for second in 3..=500 {
        system.tick(second * 1000);
        if !system.warp_exits.contains_key(&ship) { break; }
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    crate::services::combat::forget_ship(ship);
}

#[test]
fn catchup_and_clamp_emit_one_correction_at_its_own_step() {
    for (sequence, due, collisions) in [
        (0, 1, false), (1, 2, false), (2, 5, false), (3, 6, false),
        (4, 1, true), (5, 2, true), (6, 5, true), (7, 6, true),
    ] {
        let id = -930_200 - sequence;
        let mut system = fixture(id, 0.000001, 0.25);
        let (tx, mut rx) = mpsc::unbounded_channel();
        system.observe(1, id, tx);
        assert!(system.queue_warp(id, 3000, WarpDestination::Exact([AU, 0.0, 0.0]), None));
        system.tick_with_collisions(due * 1000, collisions);
        assert_eq!(system.current_time(), if due > 5 { 1 } else { due });
        let events: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).flatten().collect();
        let corrections: Vec<_> = events.iter().filter(|event| {
            event.items().and_then(|fields| fields.get(1)).and_then(Value::items)
                .and_then(|call| call.first()).and_then(Value::as_text).as_deref() == Some("AddBalls")
        }).collect();
        assert_eq!(corrections.len(), 1);
        assert_eq!(corrections[0].items().unwrap()[0].as_int(), Some(1));
        assert_eq!(system.ball(id).unwrap().effect_stamp, 0);
        assert_eq!(system.ball(id).unwrap().warp.factor, 3000);
        crate::services::combat::forget_ship(id);
    }
}

#[test]
fn callback_motion_uses_completed_stamp_after_partial_correction() {
    let partial = -930_300;
    let full = -930_301;
    let mut system = fixture(partial, 0.000001, 0.25);
    let mut other = fixture(full, 0.0, 0.0);
    system.insert(other.take(full).unwrap());
    let fit = ship_fit(&[(ATTR_WARP_CAPACITOR_NEED, 0.0), (ATTR_CAPACITOR_CAPACITY, 0.0)]);
    let stats = ShipStats::from_fit(&fit, Key::Ship);
    system.install_condition(full, Condition { now: stats.max, stats, shield_at: crate::client::filetime_now() }, fit);
    let (tx, mut rx) = mpsc::unbounded_channel();
    system.observe(1, partial, tx);
    let arrival = system.register_warp_arrival(Box::new(move |s| {
        assert_eq!(s.current_time(), 1);
        s.command(full, move |ball, stamp, _| {
            ball.goto_point([1000.0, 0.0, 0.0]);
            vec![event(stamp, "GotoPoint", vec![Value::Int(full), Value::Float(1000.0), Value::Float(0.0), Value::Float(0.0)])]
        });
    }));
    assert!(system.queue_warp(partial, 3000, WarpDestination::Exact([AU, 0.0, 0.0]), None));
    assert!(system.queue_warp(full, 3000, WarpDestination::Ordinary {
        point: [1_000_000.0, 0.0, 0.0], object_radius: None, min_range: 999_999.5,
    }, Some(arrival)));
    system.release_warp_arrival(arrival);
    system.tick(2000);
    let events: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).flatten().collect();
    let positions: Vec<_> = events.iter().filter_map(|event| {
        let fields = event.items()?;
        let call = fields.get(1)?.items()?;
        let name = call.first()?.as_text()?;
        (name == "AddBalls" || name == "GotoPoint").then(|| (fields[0].as_int().unwrap(), name.into_owned()))
    }).collect();
    assert_eq!(positions, vec![(1, "AddBalls".to_string()), (1, "GotoPoint".to_string())]);
    crate::services::combat::forget_ship(partial);
    crate::services::combat::forget_ship(full);
}

#[test]
fn representable_submetre_partial_at_large_coordinate() {
    let id = -930_302;
    let base = 1_000_000_000_000.0_f64;
    let increment = f64::from_bits(base.to_bits() + 1) - base;
    let mut system = fixture(id, 0.000001, increment / AU);
    system.command(id, move |ball, _, _| { ball.pos = [base, 0.0, 0.0]; Vec::new() });
    assert!(system.queue_warp(id, 3000, WarpDestination::Exact([base + 1_000_000.0, 0.0, 0.0]), None));
    system.tick(1000);
    assert_eq!(system.ball(id).unwrap().warp.length, increment);
    assert_eq!(system.warp_corrections, vec![id]);
    crate::services::combat::forget_ship(id);
}

#[test]
fn successful_cloak_detaches_arrival_but_preserves_unpaid_warp() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    let ship = -930_303;
    let mut system = fixture(ship, 0.000001, 1.0);
    system.command(ship, |ball, _, _| { ball.vel = [0.0; 3]; ball.max_vel = 0.0; Vec::new() });
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let arrival = system.register_warp_arrival(Box::new(move |_| { observed.fetch_add(1, Ordering::SeqCst); }));
    assert!(system.queue_warp(ship, 3000, WarpDestination::Exact([AU, 0.0, 0.0]), Some(arrival)));
    system.release_warp_arrival(arrival);
    system.tick(1000);
    assert!(system.warp_exits[&ship].pending);
    let generation = Arc::new(AtomicBool::new(false));
    assert!(system.begin_module_cloak(ship, -930_304, generation, 0, 0, |_| true, None));
    system.tick(2000);
    assert!(system.warp_exits[&ship].pending);
    assert!(system.warp_exits[&ship].arrival.is_none());
    assert!(system.warp_arrivals.is_empty());
    system.command(ship, |ball, _, _| { ball.max_vel = 100.0; ball.vel = [100.0, 0.0, 0.0]; Vec::new() });
    system.tick(3000);
    assert!(system.ball(ship).unwrap().is_warping());
    assert!(!system.warp_exits[&ship].pending);
    for second in 4..=500 {
        system.tick(second * 1000);
        if !system.warp_exits.contains_key(&ship) { break; }
    }
    assert!(!system.warp_exits.contains_key(&ship));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    crate::services::combat::forget_ship(ship);
}

#[test]
fn stale_request_does_not_touch_a_moving_replacement() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    for (id, replacement_warp) in [(-930_305, true), (-930_306, false)] {
        let mut system = fixture(id, 0.000001, 1.0);
        let calls = Arc::new(AtomicUsize::new(0));
        let observed = calls.clone();
        let arrival = system.register_warp_arrival(Box::new(move |_| { observed.fetch_add(1, Ordering::SeqCst); }));
        assert!(system.queue_warp(id, 3000, WarpDestination::Exact([AU, 0.0, 0.0]), Some(arrival)));
        system.release_warp_arrival(arrival);
        let mut replacement = system.take(id).unwrap();
        let point = [2_000_000.0, 0.0, 0.0];
        if replacement_warp {
            assert_eq!(replacement.warp_to(point, 0.0, 4320), WarpOutcome::Align);
            replacement.vel = [0.0; 3];
        } else {
            replacement.goto_point(point);
        }
        system.insert(replacement);
        let fit = ship_fit(&[(ATTR_WARP_CAPACITOR_NEED, 0.000001),
            (ATTR_CAPACITOR_CAPACITY, 1.0), (ATTR_RECHARGE_RATE, 1000.0)]);
        let stats = ShipStats::from_fit(&fit, Key::Ship);
        system.install_condition(id, Condition { now: stats.max, stats, shield_at: crate::client::filetime_now() }, fit);
        system.tick(1000);
        assert_eq!(system.ball(id).unwrap().goto, point);
        assert_eq!(system.ball(id).unwrap().mode, if replacement_warp { MODE_WARP } else { MODE_GOTO });
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert!(system.warp_arrivals.is_empty());
        assert!(!system.warp_exits.contains_key(&id));
        crate::services::combat::forget_ship(id);
    }
}

#[test]
fn invalid_nonfinite_price_and_curve_cancel_before_debit() {
    for (id, need, capacity, tau) in [
        (-930_307, f64::NAN, 1.0, 1000.0),
        (-930_308, f64::INFINITY, 1.0, 1000.0),
        (-930_309, 0.000001, -1.0, 1000.0),
        (-930_310, 0.000001, f64::NAN, 1000.0),
        (-930_311, 0.000001, 1.0, -1.0),
        (-930_312, 0.000001, 1.0, f64::INFINITY),
        (-930_313, f64::MAX, 1.0, 1000.0),
    ] {
        let mut system = fixture(id, need, capacity);
        system.actor_combat.get_mut(&id).unwrap().fit = ship_fit(&[
            (ATTR_WARP_CAPACITOR_NEED, need), (ATTR_CAPACITOR_CAPACITY, capacity), (ATTR_RECHARGE_RATE, tau),
        ]);
        assert!(system.queue_warp(id, 3000, WarpDestination::Exact([AU, 0.0, 0.0]), None));
        system.tick(1000);
        assert_eq!(system.ball(id).unwrap().mode, MODE_STOP);
        assert!(!system.warp_exits.contains_key(&id));
        crate::services::combat::forget_ship(id);
    }
}

#[test]
fn duplicate_alignment_replaces_callback_without_repricing_or_reset() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let ship = -930_314;
    let mut system = fixture(ship, 0.000001, 1.0);
    system.command(ship, |ball, _, _| { ball.vel = [0.0; 3]; ball.max_vel = 0.0; Vec::new() });
    let calls = Arc::new(AtomicUsize::new(0));
    let first_calls = calls.clone();
    let first = system.register_warp_arrival(Box::new(move |_| { first_calls.fetch_add(1, Ordering::SeqCst); }));
    assert!(system.queue_warp(ship, 3000, WarpDestination::Exact([AU, 0.0, 0.0]), Some(first)));
    system.release_warp_arrival(first);
    system.tick(1000);
    assert_eq!(system.ball(ship).unwrap().effect_stamp, -2);
    let second_calls = calls.clone();
    let second = system.register_warp_arrival(Box::new(move |_| { second_calls.fetch_add(1, Ordering::SeqCst); }));
    assert!(system.queue_warp(ship, 4320, WarpDestination::Exact([AU, 0.0, 0.0]), Some(second)));
    system.release_warp_arrival(second);
    system.tick(2000);
    assert_eq!(system.ball(ship).unwrap().effect_stamp, -3);
    assert_eq!(system.ball(ship).unwrap().warp.factor, 3000);
    assert!(system.warp_exits[&ship].pending);
    assert_eq!(system.warp_exits[&ship].arrival, Some(second));
    assert!(!system.warp_arrivals.contains_key(&first));
    system.command(ship, |ball, _, _| { ball.stop(); Vec::new() });
    system.tick(3000);
    assert!(system.warp_arrivals.is_empty());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    crate::services::combat::forget_ship(ship);
}
