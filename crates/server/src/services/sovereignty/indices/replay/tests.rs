//! Strict-prefix checkpoint and interval-baseline fold mechanics.

use super::*;
use crate::lua::content::indices::{Definitions, LevelBand, Occupancy};
use crate::dao::progress::sovereignty::Increment;
use super::super::activation::PolicySegment;

fn history() -> PolicyHistory {
    let band = |level: i64| LevelBand { level, min_points: level as f64 * 100.0, max_points: (level + 1) as f64 * 100.0, modifier: 1.0 };
    let definitions = Arc::new(Definitions {
        military_attribute: 11, industrial_attribute: 22, strategic_attribute: 33,
        military: (0..6).map(band).collect(), industrial: (0..6).map(band).collect(),
        strategic_seconds: vec![10, 20, 40, 80, 160], military_points_per_death: 10.0,
        industrial_points_per_m3: 1.0, decay_points: 1.0, decay_interval_ticks: 100,
        occupancy: Occupancy { base: 1.0, min: 1.0, max: 6.0, capital: 0.0, military: vec![], industrial: vec![], strategic: vec![] },
    });
    PolicyHistory { identities: Arc::new(Vec::new()), segments: vec![
        PolicySegment { ordinal: 1, starts_at: 1, definitions: definitions.clone() },
        PolicySegment { ordinal: 2, starts_at: 250, definitions },
    ] }
}

fn row() -> DevelopmentRow {
    DevelopmentRow { system_id: 1, military_points: 0.0, industrial_points: 0.0, evaluated_at: 1, revision: 0, activation_ordinal: 1, trend_start_at: 1, military_trend_base: 0.0, industrial_trend_base: 0.0, tail: None }
}

fn slice(at: i64, id: i64) -> SourceSlice {
    SourceSlice { order: FactOrder { occurred_at: at, source_namespace: "business".into(), source_key: format!("{id:04}"), ordinal: 0, slice_id: id }, increment: if id % 2 == 0 { Increment::NpcDeath } else { Increment::MinedM3(2.5) } }
}

fn run(row: &DevelopmentRow, history: &PolicyHistory, slices: &[SourceSlice], now: i64) -> FoldOut {
    let anchor = PointState { at: row.evaluated_at, military: row.military_points, industrial: row.industrial_points };
    let mut fold = Fold::new(row, anchor, row.tail.as_ref().map(|t| t.occurred_at), row.tail.clone(), history, now).unwrap();
    for fact in slices { fold.push(fact).unwrap(); }
    fold.finish().unwrap()
}

#[test]
fn checkpoint_is_strict_prefix_at_same_timestamp() {
    let history = history();
    let row = row();
    let facts = vec![slice(150, 1), slice(200, 2), slice(200, 3), slice(260, 4)];
    let full = run(&row, &history, &facts, 300);
    let checkpoint = full.checkpoints.iter().find(|cp| cp.checkpoint_at == 200).unwrap();
    assert_eq!(checkpoint.activation_ordinal, 1);
    let anchor = PointState { at: checkpoint.checkpoint_at, military: checkpoint.military_points, industrial: checkpoint.industrial_points };
    let mut resumed = Fold::new(&row, anchor, None, Some(facts[0].order.clone()), &history, 300).unwrap();
    for fact in &facts[1..] { resumed.push(fact).unwrap(); }
    let resumed = resumed.finish().unwrap();
    assert_eq!(resumed.next, full.next);
    assert_eq!(crossed_boundary(&history, 200, 250), Some(250));
    assert_eq!(crossed_boundary(&history, 250, 300), Some(300));
}

#[test]
fn append_at_anchor_preserves_interval_baseline_and_tail() {
    let history = history();
    let first = slice(300, 1);
    let second = slice(300, 2);
    let initial = run(&row(), &history, std::slice::from_ref(&first), 300).next;
    let appended = run(&initial, &history, std::slice::from_ref(&second), 300);
    let combined = run(&row(), &history, &[first, second], 300);
    assert_eq!(appended.next.military_trend_base, combined.next.military_trend_base);
    assert_eq!(appended.next.industrial_trend_base, combined.next.industrial_trend_base);
    assert_eq!(appended.next.tail, combined.next.tail);
    assert_eq!(run(&appended.next, &history, &[], 300).next.tail, appended.next.tail);
}
