//! Unit checks for the actor's neutral capture mechanics.
use super::score::{advance, select};
use infinity_core::consts::FILETIME_SECOND;
use crate::dao::sovereignty::capture::CaptureRow;

fn row() -> CaptureRow {
    CaptureRow {
        target_item_id: 1, system_id: 1, ownership_epoch: 1,
        controlling_team: None, scoring_team: None, defender_team: 0,
        score: 0.0, rate: 0.0, blocked: false, score_sim_time: 0,
        checkpoint_wall_time: 0, revision: 0, capture_time_seconds: 10.0,
        defensive_regen_rate: 0.0, checkpoint_interval_ms: 1,
        content_namespace: "test".into(), content_version: "test".into(),
    }
}

#[test]
fn neutral_progress_blocks_and_reverses_before_switching() {
    let mut r = row();
    select(&mut r, Some(1), false);
    assert_eq!(advance(&mut r, 5 * FILETIME_SECOND), None);
    assert_eq!(r.score, 0.5);
    select(&mut r, None, true);
    assert_eq!(r.rate, 0.0);
    advance(&mut r, 10 * FILETIME_SECOND);
    assert_eq!(r.score, 0.5);
    select(&mut r, Some(2), false);
    assert_eq!(r.rate, -1.0);
    advance(&mut r, 17 * FILETIME_SECOND);
    assert_eq!(r.controlling_team, Some(2));
    assert!((r.score - 0.2).abs() < 1e-12);
    assert_eq!(advance(&mut r, 25 * FILETIME_SECOND), Some(25 * FILETIME_SECOND));
    assert_eq!(r.score, 1.0);
}

#[test]
fn timestamp_rounding_preserves_large_filetime_anchor() {
    let mut r = row();
    r.score_sim_time = 130_000_000_000_000_000;
    r.capture_time_seconds = 0.5 / FILETIME_SECOND as f64;
    select(&mut r, Some(1), false);
    let anchor = r.score_sim_time;
    assert_eq!(advance(&mut r, anchor + 1), Some(anchor + 1));
    let mut r = row();
    r.score_sim_time = i64::MAX - 1;
    select(&mut r, Some(1), false);
    assert_eq!(advance(&mut r, i64::MAX), None);
    assert!(r.score < 1.0);
}

#[test]
fn stopping_a_module_preserves_completed_and_other_modules_intervals() {
    use super::{Segment, State, truncate};
    use crate::services::sovereignty::CaptureContributor;
    let contributor = |module_id| CaptureContributor {
        character_id: 1, ship_id: 1, module_id, corporation_id: 1,
        alliance_id: 1, source_revision: 1,
    };
    let mut state = State::default();
    state.segments.push(Segment { contributor: contributor(1), start: 1, end: 3 });
    state.segments.push(Segment { contributor: contributor(1), start: 3, end: 10 });
    state.segments.push(Segment { contributor: contributor(2), start: 3, end: 10 });
    truncate(&mut state, 1, 5);
    assert_eq!(state.segments.iter().map(|segment| segment.end).collect::<Vec<_>>(), [3, 5, 10]);
    truncate(&mut state, 1, 2);
    assert_eq!(state.segments.iter().map(|segment| segment.end).collect::<Vec<_>>(), [2, 3, 10]);
}

#[test]
fn requested_stop_and_old_activation_do_not_remove_replacement() {
    use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
    use super::{LinkCycle, blocks, cycle, stop};
    use crate::{services::sovereignty::CaptureContributor, space::System};
    let mut system = System::default();
    let current = Arc::new(AtomicBool::new(false));
    let old = Arc::new(AtomicBool::new(false));
    system.sovereignty.links.active.insert(1, LinkCycle {
        target_item_id: 1, target_epoch: 1,
        contributor: CaptureContributor { character_id: 1, ship_id: 2, module_id: 1,
            corporation_id: 1, alliance_id: 1, source_revision: 1 },
        membership_generation: 1, activation: current.clone(), range_m: 1.0,
        scoring: false, expires_sim: 1,
    });
    current.store(true, Ordering::Relaxed);
    assert!(blocks(&system, 2));
    assert!(cycle(&system, 1, &old).is_none());
    let saved = cycle(&system, 1, &current).unwrap();
    assert_eq!(saved.contributor.source_revision, 1);
    assert_eq!(saved.membership_generation, 1);
    stop(&mut system, 1, &old);
    assert!(blocks(&system, 2));
}

#[test]
fn frozen_endpoint_rejects_paid_cycle_without_changing_history_or_active_link() {
    use std::sync::{Arc, atomic::AtomicBool};
    use super::{LinkCycle, Segment, blocks, paid_cycle};
    use crate::{services::sovereignty::{CaptureContributor, CaptureObservation}, space::System};
    let mut system = System::default();
    system.sovereignty.readiness = Some(Arc::new(AtomicBool::new(true)));
    let contributor = CaptureContributor { character_id: 1, ship_id: 2, module_id: 1,
        corporation_id: 1, alliance_id: 1, source_revision: 1 };
    let cycle = LinkCycle { target_item_id: 1, target_epoch: 1,
        contributor: contributor.clone(), membership_generation: 1,
        activation: Arc::new(AtomicBool::new(false)), range_m: 1.0,
        scoring: true, expires_sim: i64::MAX };
    system.sovereignty.links.active.insert(1, cycle.clone());
    system.sovereignty.links.segments.push(Segment { contributor: contributor.clone(), start: 1, end: 2 });
    system.sovereignty.links.frozen = Some(CaptureObservation {
        target: row(), system_revision: 1, contributors: Vec::new(), claimant: Some(contributor),
    });
    assert!(!paid_cycle(&mut system, cycle.clone()));
    assert_eq!(system.sovereignty.links.segments.len(), 1);
    assert_eq!(system.sovereignty.links.segments[0].end, 2);
    assert!(blocks(&system, 2));
    system.sovereignty.links.frozen = None;
    system.sovereignty.readiness.as_ref().unwrap().store(false, std::sync::atomic::Ordering::Release);
    assert!(!paid_cycle(&mut system, cycle));
    assert_eq!(system.sovereignty.links.segments.len(), 1);
    assert!(blocks(&system, 2));
}
