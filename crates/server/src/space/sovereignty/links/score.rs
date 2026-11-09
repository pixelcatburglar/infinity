//! Neutral Entosis score interpolation at simulation FILETIME boundaries.
//! The client entosisController interpolates a linear score and clamps it to [0, 1].
//! Only neutral, two-directional capture is modelled here.
use infinity_core::consts::FILETIME_SECOND;
use crate::dao::sovereignty::capture::CaptureRow;

/// Convert a positive duration to FILETIME ticks, matching Python 2 round ties.
fn ticks(seconds: f64) -> Option<i64> {
    let value = seconds * FILETIME_SECOND as f64;
    if !value.is_finite() || value >= i64::MAX as f64 { None }
    else if value <= 0.0 { Some(0) }
    else { Some(value.round() as i64) }
}

/// Client entosisController._GetCaptureTimestamp, including the neutral reversal.
pub(super) fn capture_timestamp(row: &CaptureRow) -> Option<i64> {
    if !row.score.is_finite() || !row.rate.is_finite() || row.rate == 0.0
        || !row.capture_time_seconds.is_finite() || row.capture_time_seconds <= 0.0 {
        return None;
    }
    let score = row.score.clamp(0.0, 1.0);
    let remaining = if row.rate < 0.0 { score + 1.0 } else { 1.0 - score };
    let duration = remaining * (row.capture_time_seconds / row.rate.abs());
    row.score_sim_time.checked_add(ticks(duration)?)
}

/// Advance an unchanged neutral scoring team to `until`, switching only at zero.
/// Returns the exact simulated capture endpoint, if reached.
pub(super) fn advance(row: &mut CaptureRow, until: i64) -> Option<i64> {
    let until = until.max(row.score_sim_time);
    if row.rate == 0.0 || !row.rate.is_finite() || !row.capture_time_seconds.is_finite()
        || row.capture_time_seconds <= 0.0 || !row.score.is_finite() {
        row.score_sim_time = until;
        return None;
    }
    let duration = row.capture_time_seconds / row.rate.abs();
    if !duration.is_finite() { row.score_sim_time = until; return None; }
    if row.rate < 0.0 {
        let zero = ticks(row.score.clamp(0.0, 1.0) * duration)
            .and_then(|delta| row.score_sim_time.checked_add(delta));
        if let Some(zero) = zero.filter(|&zero| zero <= until) {
            row.score = 0.0;
            row.score_sim_time = zero;
            row.controlling_team = row.scoring_team;
            row.rate = 1.0;
            return advance(row, until);
        }
    } else {
        let full = ticks((1.0 - row.score.clamp(0.0, 1.0)) * duration)
            .and_then(|delta| row.score_sim_time.checked_add(delta));
        if let Some(full) = full.filter(|&full| full <= until) {
            row.score = 1.0;
            row.score_sim_time = full;
            return Some(full);
        }
    }
    let elapsed = until.saturating_sub(row.score_sim_time) as f64 / FILETIME_SECOND as f64;
    row.score = (row.score + elapsed / row.capture_time_seconds * row.rate).clamp(0.0, 1.0);
    row.score_sim_time = until;
    None
}

/// Select the rate after resolving the distinct live teams at a boundary.
pub(super) fn select(row: &mut CaptureRow, team: Option<i64>, blocked: bool) {
    row.blocked = blocked;
    row.scoring_team = if blocked { None } else { team };
    row.rate = match row.scoring_team {
        None => 0.0,
        Some(team) if row.controlling_team.is_none() || row.score <= 0.0 => {
            row.controlling_team = Some(team);
            1.0
        }
        Some(team) if row.controlling_team == Some(team) => 1.0,
        Some(_) => -1.0,
    };
}

#[cfg(test)]
mod tests;
