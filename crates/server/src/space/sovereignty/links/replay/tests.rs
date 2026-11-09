//! Paid-interval replay mechanics without a database.
use super::*;
use infinity_core::consts::FILETIME_SECOND as S;
use crate::services::sovereignty::CaptureContributor;

fn row() -> CaptureRow {
    CaptureRow { target_item_id: 1, system_id: 1, ownership_epoch: 1, controlling_team: None,
        scoring_team: None, defender_team: 0, score: 0.0, rate: 0.0, blocked: false,
        score_sim_time: 0, checkpoint_wall_time: 0, revision: 0, capture_time_seconds: 10.0,
        defensive_regen_rate: 0.0, checkpoint_interval_ms: 1000,
        content_namespace: "test".into(), content_version: "test".into() }
}
fn segment(team: i64, character: i64, revision: i64, start: i64, end: i64) -> Segment {
    Segment { contributor: CaptureContributor { character_id: character, ship_id: character,
        module_id: character, corporation_id: team, alliance_id: team, source_revision: revision }, start: start * S, end: end * S }
}
#[test]
fn same_team_does_not_stack_and_three_teams_block() {
    let segments = [segment(1, 1, 1, 0, 8), segment(1, 2, 1, 0, 8), segment(2, 3, 1, 3, 5), segment(3, 4, 1, 3, 5)];
    let state = State { segments: segments.into(), ..State::default() };
    let (r, _) = replay(&state, &row(), 8 * S);
    assert!((r.score - 0.6).abs() < 1e-9);
    assert_eq!(r.rate, 0.0);
}
#[test]
fn opponent_reverses_before_new_control() {
    let segments = [segment(1, 1, 1, 0, 5), segment(2, 2, 1, 5, 20)];
    let state = State { segments: segments.into(), ..State::default() };
    let (r, endpoint) = replay(&state, &row(), 20 * S);
    assert_eq!(endpoint, Some(state.segments[1].contributor.clone()));
    assert_eq!(r.score_sim_time, 20 * S);
    assert_eq!(r.controlling_team, Some(2));
    assert_eq!(r.score, 1.0);
}
#[test]
fn proof_includes_stopped_replaced_generations() {
    let mut state = State::default();
    state.anchor = Some(row());
    state.segments = vec![segment(1, 1, 1, 0, 2), segment(1, 1, 2, 2, 4), segment(1, 2, 1, 4, 5)];
    let (r, _) = replay(&state, state.anchor.as_ref().unwrap(), 5 * S);
    let obs = observation(&state, r, 1, None);
    assert_eq!(obs.contributors.len(), 3);
    assert_eq!(obs.contributors[0].contributor.source_revision, 1);
    assert_eq!(obs.contributors[1].contributor.source_revision, 2);
}
#[test]
fn delayed_earlier_change_cuts_even_with_higher_watermark() {
    let mut system = System::default();
    system.sovereignty.links.anchor = Some(row());
    system.sovereignty.links.current = Some(row());
    system.sovereignty.links.segments.push(segment(1, 1, 1, 0, 8));
    system.sovereignty.links.watermarks.insert(1, 9);
    super::super::invalidate(&mut system, &[SourceChange { character_id: 1, revision: 2, effective_at: 2 * S }]);
    assert_eq!(system.sovereignty.links.segments[0].end, 2 * S);
}

#[test]
fn invalidation_closes_existing_interval_while_under_pressure() {
    let mut system = System::default();
    system.sovereignty.links.anchor = Some(row());
    system.sovereignty.links.current = Some(row());
    system.sovereignty.links.pressure = true;
    system.sovereignty.links.segments.push(segment(1, 1, 1, 0, 8));
    super::super::invalidate(&mut system, &[SourceChange { character_id: 1, revision: 2, effective_at: 3 * S }]);
    assert!(system.sovereignty.links.pressure);
    assert_eq!(system.sovereignty.links.segments.len(), 1);
    assert_eq!(system.sovereignty.links.segments[0].end, 3 * S);
}

#[test]
fn boundary_start_proves_post_boundary_rate_and_end_proves_history_only() {
    let mut state = State::default();
    state.anchor = Some(row());
    state.segments = vec![segment(1, 1, 1, 0, 5), segment(1, 2, 1, 5, 9)];
    let (r, _) = replay(&state, state.anchor.as_ref().unwrap(), 5 * S);
    assert_eq!(r.scoring_team, Some(1));
    assert_eq!(r.rate, 1.0);
    let obs = observation(&state, r, 1, None);
    assert_eq!(obs.contributors.len(), 2);
    assert_eq!(obs.contributors[0].through, 5 * S);
    assert_eq!(obs.contributors[1].through, 5 * S);
    state.anchor.as_mut().unwrap().score_sim_time = 5 * S;
    let (r, _) = replay(&state, state.anchor.as_ref().unwrap(), 5 * S);
    let obs = observation(&state, r, 1, None);
    assert_eq!(obs.contributors.len(), 1);
    assert_eq!(obs.contributors[0].contributor.character_id, 2);
}

#[test]
fn contributions_merge_identical_intervals_and_clip_to_observation() {
    let mut state = State::default();
    state.anchor = Some(row());
    state.segments = vec![segment(1, 1, 1, 0, 2), segment(1, 1, 1, 3, 9), segment(1, 2, 1, 7, 9)];
    let (r, _) = replay(&state, state.anchor.as_ref().unwrap(), 5 * S);
    let obs = observation(&state, r, 1, None);
    assert_eq!(obs.contributors.len(), 1);
    assert_eq!(obs.contributors[0].through, 5 * S);
}

#[test]
fn claimant_is_the_exact_crossing_generation_after_source_cut() {
    let mut system = System::default();
    system.sovereignty.links.anchor = Some(row());
    system.sovereignty.links.current = Some(row());
    let old = segment(1, 1, 0, 0, 20);
    let mut new = segment(1, 1, 1, 4, 20);
    new.contributor.corporation_id = 2;
    system.sovereignty.links.segments = vec![old, new.clone()];
    super::super::invalidate(&mut system, &[SourceChange { character_id: 1, revision: 1, effective_at: 4 * S }]);
    let state = &system.sovereignty.links;
    let (r, endpoint) = replay(state, state.anchor.as_ref().unwrap(), 12 * S);
    assert_eq!(r.score_sim_time, 10 * S);
    let obs = observation(state, r, 1, endpoint);
    assert_eq!(obs.claimant, Some(new.contributor));
    assert_eq!(obs.contributors[0].through, 4 * S);
    assert_eq!(obs.contributors[1].through, 10 * S);
}

#[test]
fn out_of_order_source_changes_preserve_earliest_cut() {
    let mut system = System::default();
    system.sovereignty.links.anchor = Some(row());
    system.sovereignty.links.current = Some(row());
    system.sovereignty.links.segments.push(segment(1, 1, 0, 0, 10));
    super::super::invalidate(&mut system, &[SourceChange { character_id: 1, revision: 2, effective_at: 7 * S }]);
    super::super::invalidate(&mut system, &[SourceChange { character_id: 1, revision: 1, effective_at: 3 * S }]);
    assert_eq!(system.sovereignty.links.segments[0].end, 3 * S);
}

#[test]
fn natural_paid_expiry_at_goal_is_a_claim() {
    let mut state = State::default();
    state.segments.push(segment(1, 1, 0, 0, 10));
    let (r, claimant) = replay(&state, &row(), 10 * S);
    assert_eq!(r.score_sim_time, 10 * S);
    assert_eq!(claimant, Some(state.segments[0].contributor.clone()));
}

#[test]
fn source_cut_at_goal_prevents_freeze_until_current_cycle() {
    let mut system = System::default();
    system.sovereignty.version = Some((1, 1));
    system.sovereignty.links.anchor = Some(row());
    system.sovereignty.links.current = Some(row());
    system.sovereignty.links.segments.push(segment(1, 1, 0, 0, 15));
    super::super::invalidate(&mut system, &[SourceChange { character_id: 1, revision: 1, effective_at: 10 * S }]);
    boundary(&mut system, 10 * S);
    assert!(system.sovereignty.links.frozen.is_none());
    assert_eq!(system.sovereignty.links.current.as_ref().unwrap().score, 1.0);
    let current = segment(1, 1, 1, 12, 20);
    system.sovereignty.links.segments.push(current.clone());
    boundary(&mut system, 12 * S);
    let frozen = system.sovereignty.links.frozen.as_ref().unwrap();
    assert_eq!(frozen.target.score_sim_time, 12 * S);
    assert_eq!(frozen.claimant, Some(current.contributor));
}

#[test]
fn current_generation_wins_over_stale_crossing_segment_in_any_order() {
    let mut state = State::default();
    let old = segment(1, 1, 0, 0, 20);
    let mut current = segment(1, 1, 1, 0, 20);
    current.contributor.corporation_id = 2;
    state.watermarks.insert(1, 1);
    state.segments = vec![old, current.clone()];
    let (r, claimant) = replay(&state, &row(), 10 * S);
    assert_eq!(r.score_sim_time, 10 * S);
    assert_eq!(claimant, Some(current.contributor.clone()));
    state.segments.reverse();
    let (_, claimant) = replay(&state, &row(), 10 * S);
    assert_eq!(claimant, Some(current.contributor));
}

#[test]
fn opponent_reverses_an_unclaimable_full_score() {
    let mut state = State::default();
    state.watermarks.insert(1, 1);
    state.segments = vec![segment(1, 1, 0, 0, 10), segment(2, 2, 0, 10, 15)];
    let (r, claimant) = replay(&state, &row(), 15 * S);
    assert!(claimant.is_none());
    assert_eq!(r.score_sim_time, 15 * S);
    assert_eq!(r.scoring_team, None);
    assert!((r.score - 0.5).abs() < 1e-9);
}

#[test]
fn delayed_source_cut_before_goal_requires_current_claimant() {
    let mut system = System::default();
    system.sovereignty.links.anchor = Some(row());
    system.sovereignty.links.current = Some(row());
    system.sovereignty.links.segments = vec![segment(1, 1, 0, 0, 20), segment(1, 2, 0, 0, 20)];
    super::super::invalidate(&mut system, &[SourceChange { character_id: 1, revision: 1, effective_at: 5 * S }]);
    let state = &system.sovereignty.links;
    let (r, claimant) = replay(state, state.anchor.as_ref().unwrap(), 10 * S);
    assert_eq!(r.score_sim_time, 10 * S);
    assert_eq!(claimant, Some(state.segments[1].contributor.clone()));
}
