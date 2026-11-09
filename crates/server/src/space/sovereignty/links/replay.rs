//! Chronological replay of paid intervals from the last committed capture anchor.
//! Entosis score interpolation mirrors the client controller; interval provenance
//! comes from actor-owned paid cycles. A completed cycle includes its endpoint, but
//! a source change at that endpoint supersedes it before claimant selection.
//! Offline progression is not supported.
use super::{score, Segment, State};
use crate::{dao::sovereignty::{capture::CaptureRow, sources::SourceChange}, services::sovereignty::{CaptureContribution, CaptureContributor, CaptureObservation}, space::System};

fn teams(segments: &[Segment], at: i64) -> (Option<i64>, bool) {
    let mut team = None;
    for segment in segments.iter().filter(|s| s.start <= at && at < s.end) {
        let candidate = segment.contributor.alliance_id;
        if team.is_some_and(|t| t != candidate) { return (None, true); }
        team = Some(candidate);
    }
    (team, false)
}

fn eligible(state: &State, segment: &Segment) -> bool {
    !state.watermarks.get(&segment.contributor.character_id)
        .is_some_and(|&revision| revision > segment.contributor.source_revision)
}

fn replay(state: &State, anchor: &CaptureRow, until: i64) -> (CaptureRow, Option<CaptureContributor>) {
    let segments = &state.segments;
    let mut row = anchor.clone();
    let until = until.max(row.score_sim_time);
    let mut at = row.score_sim_time;
    loop {
        let (team, blocked) = teams(segments, at);
        score::select(&mut row, team, blocked);
        let next = segments.iter().flat_map(|s| [s.start, s.end]).filter(|&t| t > at && t < until).min().unwrap_or(until);
        if row.score >= 1.0 && row.rate > 0.0 {
            if let Some(segment) = segments.iter().find(|s| s.start <= at && at < s.end && Some(s.contributor.alliance_id) == team && eligible(state, s)) {
                return (row, Some(segment.contributor.clone()));
            }
            row.score_sim_time = next;
        } else if let Some(endpoint) = score::advance(&mut row, next) {
            if let Some(segment) = segments.iter().find(|s| s.start < endpoint && s.end >= endpoint && Some(s.contributor.alliance_id) == row.scoring_team && eligible(state, s)) {
                return (row, Some(segment.contributor.clone()));
            }
            at = endpoint;
            continue;
        }
        if next == until && at == until { break; }
        at = next;
    }
    let (team, blocked) = teams(segments, until);
    score::select(&mut row, team, blocked);
    (row, None)
}

pub(super) fn observation(state: &State, row: CaptureRow, system_revision: i64, claimant: Option<CaptureContributor>) -> CaptureObservation {
    let start = state.anchor.as_ref().map_or(row.score_sim_time, |a| a.score_sim_time);
    let mut contributors: Vec<CaptureContribution> = Vec::new();
    for segment in &state.segments {
        if segment.start > row.score_sim_time || segment.end <= start || segment.end <= segment.start { continue; }
        let through = segment.end.min(row.score_sim_time);
        if let Some(proof) = contributors.iter_mut().find(|v| v.contributor == segment.contributor) {
            proof.through = proof.through.max(through);
        } else {
            contributors.push(CaptureContribution { contributor: segment.contributor.clone(), through });
        }
    }
    CaptureObservation { target: row, system_revision, contributors, claimant }
}

pub(super) fn publish(system: &mut System, row: &CaptureRow) {
    let id = row.target_item_id;
    let value = super::super::entity::score_value(row);
    system.command(id, move |ball, stamp, _| {
        if let Some((_, score)) = ball.slim.iter_mut().find(|(key, _)| *key == "entosis_score") { *score = value; }
        vec![crate::space::event(stamp, "OnSlimItemChange", vec![marshal::Value::Int(id), ball.slim_item()])]
    });
}

pub(super) fn boundary(system: &mut System, until: i64) {
    let state = &mut system.sovereignty.links;
    if state.frozen.is_some() { return; }
    let Some(anchor) = state.anchor.as_ref() else { return };
    let (row, claimant) = replay(state, anchor, until);
    let changed = state.current.as_ref().is_none_or(|old| old.controlling_team != row.controlling_team || old.scoring_team != row.scoring_team || old.rate != row.rate || old.blocked != row.blocked || row.score_sim_time < old.score_sim_time || (old.score - row.score).abs() > 1e-12 && until == old.score_sim_time);
    let endpoint = claimant.is_some();
    if endpoint && state.frozen.is_none() {
        if let Some((_, revision)) = system.sovereignty.version { state.frozen = Some(observation(state, row.clone(), revision, claimant)); }
    }
    state.dirty |= changed || endpoint || row.score_sim_time > anchor.score_sim_time && !state.segments.is_empty();
    state.current = Some(row.clone());
    if changed || endpoint { publish(system, &row); }
}

pub(super) fn invalidate(system: &mut System, changes: &[SourceChange]) {
    let mut changed = system.sovereignty.links.frozen.as_ref().and_then(|observation| observation.claimant.as_ref())
        .is_some_and(|claimant| changes.iter().any(|change| change.character_id == claimant.character_id && change.revision > claimant.source_revision));
    for segment in &mut system.sovereignty.links.segments {
        for change in changes {
            if segment.contributor.character_id == change.character_id && segment.contributor.source_revision < change.revision && segment.end > change.effective_at {
                segment.end = change.effective_at.max(segment.start);
                changed = true;
            }
        }
    }
    if changed {
        system.sovereignty.links.frozen = None;
        let now = crate::client::filetime_from_unix_ms(system.now_ms());
        boundary(system, now);
        system.sovereignty.links.dirty = true;
    }
}

#[cfg(test)] mod tests;
