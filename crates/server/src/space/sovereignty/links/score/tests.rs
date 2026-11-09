//! Unit checks for neutral capture mechanics and client FILETIME rounding.
use super::{advance, capture_timestamp, select, ticks};
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
    assert_eq!(capture_timestamp(&r), Some(25 * FILETIME_SECOND));
    advance(&mut r, 17 * FILETIME_SECOND);
    assert_eq!(r.controlling_team, Some(2));
    assert!((r.score - 0.2).abs() < 1e-12);
    assert_eq!(advance(&mut r, 25 * FILETIME_SECOND), Some(25 * FILETIME_SECOND));
    assert_eq!(r.score, 1.0);
}

#[test]
fn timestamp_rounding_preserves_large_filetime_anchor() {
    assert_eq!(ticks(0.49 / FILETIME_SECOND as f64), Some(0));
    assert_eq!(ticks(0.5 / FILETIME_SECOND as f64), Some(1));
    assert_eq!(ticks(0.51 / FILETIME_SECOND as f64), Some(1));
    let mut r = row();
    r.score_sim_time = 130_000_000_000_000_000;
    r.capture_time_seconds = 0.5 / FILETIME_SECOND as f64;
    select(&mut r, Some(1), false);
    let anchor = r.score_sim_time;
    assert_eq!(capture_timestamp(&r), Some(anchor + 1));
    assert_eq!(advance(&mut r, anchor + 1), Some(anchor + 1));
    let mut r = row();
    r.score_sim_time = i64::MAX - 1;
    select(&mut r, Some(1), false);
    assert_eq!(capture_timestamp(&r), None);
    assert_eq!(advance(&mut r, i64::MAX), None);
    assert!(r.score < 1.0);
}

#[test]
fn invalid_and_extreme_durations_never_make_a_fake_endpoint() {
    let mut r = row();
    select(&mut r, Some(1), false);
    r.capture_time_seconds = f64::INFINITY;
    assert_eq!(capture_timestamp(&r), None);
    assert_eq!(advance(&mut r, FILETIME_SECOND), None);
    r.capture_time_seconds = f64::MAX;
    assert_eq!(capture_timestamp(&r), None);
    assert_eq!(advance(&mut r, 2 * FILETIME_SECOND), None);
}
