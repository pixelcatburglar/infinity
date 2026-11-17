//! Pure mechanics against independent synthetic fixtures; no client tables.

use std::sync::Arc;
use super::{super::activation::PolicySegment, *};

fn bands(width: f64) -> Vec<LevelBand> {
    (0..6).map(|level| LevelBand { level, min_points: level as f64 * width, max_points: (level + 1) as f64 * width, modifier: (level + 1) as f64 }).collect()
}

fn definitions(decay_points: f64, military_width: f64) -> Definitions {
    let bonuses = |bonus: f64| (1..=5).map(|level| LevelBonus { level, bonus }).collect();
    Definitions {
        military_attribute: 11, industrial_attribute: 22, strategic_attribute: 33,
        military: bands(military_width), industrial: bands(20.0), strategic_seconds: vec![10, 20, 40, 80, 160],
        military_points_per_death: 100.0, industrial_points_per_m3: 2.0,
        decay_points, decay_interval_ticks: 100,
        occupancy: crate::lua::content::indices::Occupancy { base: 1.0, min: 1.0, max: 6.0, capital: 2.0, military: bonuses(0.5), industrial: bonuses(0.25), strategic: bonuses(0.1) },
    }
}

fn policies(segments: &[(i64, f64, f64)]) -> PolicyHistory {
    PolicyHistory {
        identities: Arc::new(Vec::new()),
        segments: segments.iter().enumerate().map(|(index, (starts_at, decay, width))| PolicySegment {
            ordinal: index as i64 + 1, starts_at: *starts_at,
            definitions: Arc::new(definitions(*decay, *width)),
        }).collect(),
    }
}

#[test]
fn index_level_matches_golden_boundaries() {
    let bounds = [10.0, 20.0, 40.0, 80.0, 160.0, 320.0];
    for (points, expected) in [
        (0.0, IndexLevel { level: 0, remainder: 0.0 }),
        (10.0, IndexLevel { level: 1, remainder: 0.0 }),
        (15.0, IndexLevel { level: 1, remainder: 0.5 }),
        (319.0, IndexLevel { level: 5, remainder: 159.0 / 160.0 }),
        (1.0e9, IndexLevel { level: 5, remainder: 0.0 }),
        (-1.0, IndexLevel { level: 0, remainder: -0.1 }),
    ] {
        assert_eq!(super::index_level(points, |index| bounds[index], bounds.len()), expected);
    }
}

#[test]
fn advance_decays_continuously_inside_each_policy_segment() {
    let history = policies(&[(0, 50.0, 200.0), (100, 200.0, 200.0)]);
    let state = PointState { at: 0, military: 1000.0, industrial: 60.0 };
    let after = advance(state, 200, &history).unwrap();
    assert_eq!((after.at, after.military, after.industrial), (200, 750.0, 0.0));
    let before = PointState { at: -50, military: 10.0, industrial: 5.0 };
    assert_eq!(advance(before, -10, &history).unwrap(), PointState { at: -10, military: 10.0, industrial: 5.0 });
    assert!(advance(state, -1, &history).is_err());
    assert!(advance(PointState { at: 0, military: f64::NAN, industrial: 0.0 }, 10, &history).is_err());
}


#[test]
fn advance_clamps_carryover_at_each_segment_boundary() {
    // The second policy's military cap is 30 (width 5): the carried 49 clamps at its first instant.
    let history = policies(&[(0, 10.0, 10.0), (100, 10.0, 5.0)]);
    let state = PointState { at: 0, military: 59.0, industrial: 0.0 };
    assert_eq!(advance(state, 100, &history).unwrap().military, 30.0);
    assert_eq!(advance(state, 150, &history).unwrap().military, 25.0);
}

#[test]
fn credit_uses_occurrence_band_modifier_and_cap() {
    // Death 100, width 200 and cap 1200: modifiers yield 5+100=105,
    // 205+200=405, and 1199.5+600 clamps to 1200.
    let definitions = definitions(50.0, 200.0);
    let state = PointState { at: 10, military: 5.0, industrial: 0.0 };
    assert_eq!(credit(state, Increment::NpcDeath, &definitions).unwrap().military, 105.0);
    assert_eq!(credit(PointState { military: 205.0, ..state }, Increment::NpcDeath, &definitions).unwrap().military, 405.0);
    assert_eq!(credit(PointState { military: 1199.5, ..state }, Increment::NpcDeath, &definitions).unwrap().military, 1200.0);
    let mined = credit(state, Increment::MinedM3(10.0), &definitions).unwrap();
    assert_eq!(mined.industrial, 20.0);
    assert_eq!(credit(PointState { industrial: 45.0, ..state }, Increment::MinedM3(10.0), &definitions).unwrap().industrial, 105.0);
    assert!(credit(state, Increment::MinedM3(0.0), &definitions).is_err());
    assert!(credit(PointState { military: f64::NAN, ..state }, Increment::NpcDeath, &definitions).is_err());
    assert_eq!(credit(state, Increment::MinedM3(1.0e300), &definitions).unwrap().industrial, 120.0);
}

#[test]
fn project_combines_decay_claim_age_and_capital() {
    let history = policies(&[(0, 50.0, 40.0)]);
    let row = DevelopmentRow {
        system_id: 7, military_points: 200.0, industrial_points: 100.0, evaluated_at: 100, revision: 0,
        activation_ordinal: 1, trend_start_at: 100, military_trend_base: 200.0, industrial_trend_base: 100.0, tail: None,
    };
    let system = SystemRow { system_id: 7, hub_item_id: Some(1), claim_corporation_id: Some(2), alliance_id: Some(3), claim_time: Some(90), ownership_epoch: 0, revision: 0 };
    let capital = PolicyRow { alliance_id: 3, current_capital_system: Some(7), last_capital_changed_at: None, capital_lost_at: None, capital_change_not_before: None, revision: 0 };
    let view = project(Some(&row), Some(&system), Some(&capital), &history, 7, 150).unwrap();
    assert_eq!((view.military_points, view.industrial_points), (175.0, 75.0));
    assert_eq!(view.strategic_seconds, 60.0 / FILETIME_SECOND as f64);
    assert!(view.strategic_increasing && !view.military_increasing && !view.industrial_increasing);
    assert_eq!((view.military.level, view.industrial.level, view.strategic.level), (4, 3, 0));
    assert_eq!(view.defense_multiplier, 1.0 + 0.5 + 0.25 + 2.0);
    let not_capital = project(Some(&row), Some(&system), None, &history, 7, 150).unwrap();
    assert_eq!(not_capital.defense_multiplier, 1.0 + 0.5 + 0.25);
    let unclaimed = project(Some(&row), None, None, &history, 7, 150).unwrap();
    assert_eq!((unclaimed.strategic_seconds, unclaimed.strategic_increasing), (0.0, false));
    let empty = project(None, None, None, &history, 7, 150).unwrap();
    assert_eq!((empty.military_points, empty.defense_multiplier, empty.military.level), (0.0, 1.0, 0));
    assert!(project(Some(&DevelopmentRow { system_id: 8, ..row.clone() }), Some(&system), None, &history, 7, 150).is_err());
    assert!(project(None, None, None, &policies(&[(1000, 50.0, 10.0)]), 7, 150).is_err());
}
