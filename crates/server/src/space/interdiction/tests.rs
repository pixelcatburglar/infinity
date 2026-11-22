//! Field geometry, immunity and lifecycle tests over synthetic balls and fits.
use super::*;
use crate::services::combat::groups_health::{Condition, ShipStats};
use crate::services::fit::attributes::tests::ship_fit;
use crate::space::system::tests::ball;

fn condition(fit: &crate::services::fit::Fit) -> Condition {
    let stats = ShipStats::from_fit(fit, Key::Ship);
    Condition { now: stats.max, stats, shield_at: crate::client::filetime_now() }
}

fn aligning(id: i64, x: f64) -> Ball {
    let mut ship = ball(id, Some(200), x);
    ship.mode = MODE_WARP;
    ship.effect_stamp = -1;
    ship
}

#[test]
fn intersection_immunity_and_source_motion() {
    let mut s = System::new(1, Arc::default(), 0);
    s.insert(ball(1, Some(100), 0.0));
    assert!(s.begin_interdiction(1, 10, Arc::new(AtomicBool::new(false)), 10_000.0));
    // Hull surfaces count: the boundary is field radius plus ship radius, and
    // the source ship is caught inside its own field like everyone else.
    for (x, expected) in [(0.0, true), (10_001.0, true), (10_002.0, false)] {
        s.insert(ball(2, Some(200), x));
        assert_eq!(s.interdicted(2), expected, "x={x}");
        s.take(2);
    }
    assert!(s.interdicted(1));
    // The centre follows the source ball's sampled position.
    s.insert(ball(2, Some(200), 11_000.0));
    assert!(!s.interdicted(2));
    s.balls.get_mut(&1).unwrap().pos = [2_000.0, 0.0, 0.0];
    assert!(s.interdicted(2));
    s.take(2);
    // A resident positive warpBubbleImmune beats geometry in both queries.
    s.insert(ball(3, Some(300), 0.0));
    let fit = ship_fit(&[(ATTR_WARP_BUBBLE_IMMUNE, 1.0)]);
    s.install_condition(3, condition(&fit), fit);
    assert!(!s.interdicted(3));
    assert!(s.interdiction_landing(3, [0.0; 3], [5_000.0, 0.0, 0.0]).is_none());
}

#[test]
fn landing_selects_nearest_entry_within_catch_distance() {
    // (field centre x, side offset, radius) pairs; the route runs from the
    // origin toward a destination down +x. An origin inside a field clamps
    // its entry to the origin itself, still bound by the catch distance.
    for (fields, endpoint, expected) in [
        (vec![(750_000.0, 0.0, 50_000.0)], 1_000_000.0, Some(700_000.0)),
        (vec![(1_350_000.0, 0.0, 50_000.0)], 1_000_000.0, Some(1_300_000.0)),
        (vec![(1_600_000.0, 0.0, 50_000.0)], 1_000_000.0, None),
        (vec![(700_000.0, 60_000.0, 50_000.0)], 1_000_000.0, None),
        (vec![(-100_000.0, 0.0, 50_000.0)], 1_000_000.0, None),
        (vec![(50_000.0, 0.0, 10_000.0)], 1_000_000.0, None),
        (vec![(600_000.0, 0.0, 50_000.0), (750_000.0, 0.0, 50_000.0)], 1_000_000.0, Some(550_000.0)),
        (vec![(700_000.0, 0.0, 50_000.0), (660_000.0, 0.0, 10_000.0)], 1_000_000.0, Some(650_000.0)),
        (vec![(0.0, 0.0, 50_000.0)], 400_000.0, Some(0.0)),
        (vec![(0.0, 0.0, 50_000.0)], 600_000.0, None),
    ] {
        let mut s = System::new(1, Arc::default(), 0);
        s.insert(ball(1, Some(100), 0.0));
        for (index, &(x, y, radius)) in fields.iter().enumerate() {
            let source_id = 10 + index as i64;
            let mut source = ball(source_id, Some(200), x);
            source.pos[1] = y;
            s.insert(source);
            assert!(s.begin_interdiction(source_id, 20 + index as i64, Arc::new(AtomicBool::new(false)), radius), "{fields:?}");
        }
        let landing = s.interdiction_landing(1, [0.0; 3], [endpoint, 0.0, 0.0]);
        match expected {
            Some(x) => {
                let point = landing.unwrap_or_else(|| panic!("{fields:?} must catch"));
                assert!((point[0] - x).abs() <= 1.0 && point[1].abs() <= 1.0, "{fields:?} -> {point:?}");
            }
            None => assert!(landing.is_none(), "{fields:?} -> {landing:?}"),
        }
    }
}

#[test]
fn field_generation_departure_and_alignment() {
    let mut s = System::new(1, Arc::default(), 0);
    s.insert(ball(1, Some(100), 0.0));
    // Refusals leave no partial field behind.
    assert!(!s.begin_interdiction(99, 20, Arc::new(AtomicBool::new(false)), 10_000.0));
    assert!(!s.begin_interdiction(1, 20, Arc::new(AtomicBool::new(false)), 0.0));
    assert!(!s.begin_interdiction(1, 20, Arc::new(AtomicBool::new(true)), 10_000.0));
    assert!(s.interdiction.by_module.is_empty());
    s.insert(aligning(2, 5_000.0));
    let fit = ship_fit(&[]);
    s.install_condition(2, condition(&fit), fit);
    let generation = Arc::new(AtomicBool::new(false));
    assert!(s.begin_interdiction(1, 10, generation.clone(), 10_000.0));
    // A same-id replacement before the boundary is not the captured ship.
    s.take(2);
    s.insert(aligning(2, 5_000.0));
    s.install_condition(2, condition(&ship_fit(&[])), ship_fit(&[]));
    s.tick(1000);
    assert_eq!(s.ball(2).unwrap().mode, MODE_WARP);
    // The next raise still cancels the live replacement's alignment.
    assert!(s.begin_interdiction(1, 11, Arc::new(AtomicBool::new(false)), 10_000.0));
    s.tick(2000);
    assert_eq!(s.ball(2).unwrap().mode, MODE_STOP);
    // A set stop flag and a foreign generation both leave the fields live.
    generation.store(true, Ordering::Relaxed);
    s.end_interdiction(10, &Arc::new(AtomicBool::new(false)));
    assert!(s.interdicted(1));
    s.end_interdiction(10, &generation);
    assert!(s.interdicted(1));
    // Source departure retires everything it raised.
    s.take(1);
    assert!(s.interdiction.by_module.is_empty());
}
