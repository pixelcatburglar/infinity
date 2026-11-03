use super::*;
use crate::dao::skills::training::Skill;
use std::collections::BTreeMap;

fn skill(type_id: i32, level: i32, skill_points: i64) -> Skill {
    Skill { type_id, level, skill_points }
}

fn installation(type_id: i32, installed_at: i64, expires_at: i64) -> Installation {
    Installation { character_id: 1, type_id, installed_at, expires_at }
}

fn definitions(rows: &[(i32, &[(i32, i32)])]) -> BTreeMap<i32, Vec<(i32, i32)>> {
    rows.iter().map(|&(type_id, skills)| (type_id, skills.to_vec())).collect()
}

#[test]
fn project_uses_half_open_active_intervals() {
    let grants = definitions(&[(900, &[(10, 4)])]);
    let installations = [installation(900, 10, 20)];
    let at_start = project_core(&[], &installations, |type_id| {
        grants.get(&type_id).map(Vec::as_slice)
    }, 10);
    assert_eq!(at_start.level(10), 4);

    let at_end = project_core(&[], &installations, |type_id| {
        grants.get(&type_id).map(Vec::as_slice)
    }, 20);
    assert!(at_end.get(10).is_none());
}

#[test]
fn project_uses_maximum_overlap_and_falls_back_after_expiry() {
    let grants = definitions(&[
        (901, &[(10, 5)]),
        (902, &[(10, 3)]),
    ]);
    let installations = [
        installation(901, 10, 30),
        installation(902, 15, 40),
    ];
    let during_overlap = project_core(&[], &installations, |type_id| {
        grants.get(&type_id).map(Vec::as_slice)
    }, 25);
    assert_eq!(during_overlap.level(10), 5);

    let after_high_expires = project_core(&[], &installations, |type_id| {
        grants.get(&type_id).map(Vec::as_slice)
    }, 35);
    assert_eq!(after_high_expires.level(10), 3);
}

#[test]
fn project_distinguishes_virtual_only_from_trained_zero_and_preserves_skill_points() {
    let grants = definitions(&[(900, &[(11, 2)])]);
    let trained = [skill(10, 0, 123)];
    let snapshot = project_core(&trained, &[installation(900, 10, 20)], |type_id| {
        grants.get(&type_id).map(Vec::as_slice)
    }, 10);

    let real_zero = snapshot.get(10).expect("trained zero must remain present");
    assert_eq!(real_zero.trained.as_ref().map(|skill| skill.level), Some(0));
    assert_eq!(real_zero.trained.as_ref().map(|skill| skill.skill_points), Some(123));
    assert!(real_zero.virtual_level.is_none());

    let virtual_only = snapshot.get(11).expect("active grant must create a skill");
    assert!(virtual_only.trained.is_none());
    assert_eq!(virtual_only.virtual_level, Some(2));
}

#[test]
fn project_output_is_sorted_by_type_id() {
    let grants = definitions(&[(900, &[(20, 1)])]);
    let trained = [skill(30, 3, 0), skill(10, 2, 0)];
    let snapshot = project_core(&trained, &[installation(900, 10, 20)], |type_id| {
        grants.get(&type_id).map(Vec::as_slice)
    }, 10);
    let ids: Vec<_> = snapshot.iter().map(|(type_id, _)| type_id).collect();
    assert_eq!(ids, [10, 20, 30]);
}
