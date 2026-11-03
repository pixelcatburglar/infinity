use super::*;
use crate::dao::skills::training::Skill;

fn skill(type_id: i32, level: i32, skill_points: i64) -> Skill {
    Skill { type_id, level, skill_points }
}

#[test]
fn snapshot_preserves_true_zero_and_missing_levels() {
    let snapshot = from_trained(&[skill(10, 0, 0)]);
    assert_eq!(snapshot.level(10), 0);
    assert!(snapshot.get(10).is_some());
    assert!(snapshot.get(11).is_none());
    assert_eq!(snapshot.level(11), 0);
}

#[test]
fn snapshot_effective_level_uses_trained_level() {
    let snapshot = from_trained(&[
        skill(10, 1, 250),
        skill(20, 4, 16_000),
        skill(30, 5, 256_000),
    ]);
    assert_eq!(snapshot.level(10), 1);
    assert_eq!(snapshot.level(20), 4);
    assert_eq!(snapshot.level(30), 5);
    assert_eq!(snapshot.get(20).unwrap().effective_level(), 4);
}

#[test]
fn snapshot_iteration_is_sorted_by_type_id() {
    let snapshot = from_trained(&[skill(30, 3, 0), skill(10, 2, 0), skill(20, 1, 0)]);
    let ids: Vec<_> = snapshot.iter().map(|(type_id, _)| type_id).collect();
    assert_eq!(ids, [10, 20, 30]);
}
