//! Pure development-index mechanics: segment-scoped continuous decay, atomic
//! occurrence credits at the current band modifier, the capped client level
//! arithmetic, strategic claim age and the additive occupancy defense multiplier.
//! Mirrors `sovSvc.py::GetIndexLevel` and `occupancyCalculator.py`; every table
//! comes from compiled `Definitions`, never from a copied constant.

use infinity_core::consts::FILETIME_SECOND;
use crate::{
    dao::{progress::sovereignty::Increment, sovereignty::{development::DevelopmentRow, policy::PolicyRow, systems::SystemRow}},
    lua::content::indices::{Definitions, LevelBand, LevelBonus},
    services::{db_err, CallError},
};
use super::{activation::PolicyHistory, model::{DevelopmentView, IndexLevel, PointState}};

/// Client `GetIndexLevel` saturates at this hard-coded level once the last bound
/// is passed, for both the six-band index axes and the five strategic thresholds.
const SATURATED_LEVEL: i64 = 5;

fn invalid(reason: &str) -> CallError {
    db_err(anyhow::anyhow!("invalid development index state: {reason}"))
}

fn finite_nonnegative(value: f64) -> bool {
    value.is_finite() && value >= 0.0
}

/// First bound above the value selects the level; the remainder is the fraction
/// through that band. Negative inputs keep a negative remainder like the client;
/// passing the last bound saturates at `SATURATED_LEVEL` with zero remainder.
fn index_level(points: f64, bound: impl Fn(usize) -> f64, count: usize) -> IndexLevel {
    match (0..count).find(|index| points < bound(*index)) {
        Some(index) => {
            let min = if index == 0 { 0.0 } else { bound(index - 1) };
            IndexLevel { level: index as i64, remainder: (points - min) / (bound(index) - min) }
        }
        None => IndexLevel { level: SATURATED_LEVEL, remainder: 0.0 },
    }
}


fn cap(bands: &[LevelBand]) -> Result<f64, CallError> {
    Ok(bands.last().ok_or_else(|| invalid("index level bands are empty"))?.max_points)
}

/// The band holding the current points; past the last bound the last band's
/// modifier keeps applying, matching the saturated client level.
fn band_at<'a>(points: f64, bands: &'a [LevelBand]) -> Result<&'a LevelBand, CallError> {
    bands.iter().find(|band| points < band.max_points).or_else(|| bands.last())
        .ok_or_else(|| invalid("index level bands are empty"))
}
/// Continuous decay inside retained policy segments only: each segment applies
/// its own definitions for the overlap of its active span with [at, to).
pub(crate) fn advance(mut state: PointState, to: i64, history: &PolicyHistory) -> Result<PointState, CallError> {
    if to < state.at || !finite_nonnegative(state.military) || !finite_nonnegative(state.industrial) {
        return Err(invalid("point anchor"));
    }
    for (index, segment) in history.segments.iter().enumerate() {
        if segment.starts_at >= to { break; }
        let end = history.segments.get(index + 1).map_or(to, |next| next.starts_at.min(to));
        let start = state.at.max(segment.starts_at);
        if start < end {
            let elapsed = end.checked_sub(start).ok_or_else(|| invalid("decay span"))?;
            let loss = segment.definitions.decay_points * (elapsed as f64 / segment.definitions.decay_interval_ticks as f64);
            if !loss.is_finite() { return Err(invalid("decay magnitude")); }
            state.military = (state.military - loss).max(0.0);
            state.industrial = (state.industrial - loss).max(0.0);
        }
        // A later segment caps carried points from its first instant, even when
        // `to` lands exactly on the boundary; nothing is rewritten backwards.
        if let Some(next) = history.segments.get(index + 1) {
            if next.starts_at <= to {
                state.military = state.military.min(cap(&next.definitions.military)?);
                state.industrial = state.industrial.min(cap(&next.definitions.industrial)?);
            }
        }
    }
    state.at = to;
    Ok(state)
}

/// One atomic contribution at the modifier of the band the points are already
/// in, capped at the last band's maximum. The unit count is compared against
/// the remaining cap before the multiply, so a finite magnitude never overflows.
pub(crate) fn credit(mut state: PointState, increment: Increment, definitions: &Definitions) -> Result<PointState, CallError> {
    if !finite_nonnegative(state.military) || !finite_nonnegative(state.industrial) {
        return Err(invalid("point anchor"));
    }
    let (military_axis, bands, units, per_unit) = match increment {
        Increment::NpcDeath => (true, &definitions.military, 1.0, definitions.military_points_per_death),
        Increment::MinedM3(volume) => {
            if !volume.is_finite() || volume <= 0.0 { return Err(invalid("mined volume")); }
            (false, &definitions.industrial, volume, definitions.industrial_points_per_m3)
        }
    };
    let cap = cap(bands)?;
    let points = if military_axis { &mut state.military } else { &mut state.industrial };
    let factor = per_unit * band_at(*points, bands)?.modifier;
    if !factor.is_finite() || factor <= 0.0 { return Err(invalid("credit magnitude")); }
    let room = cap - *points;
    if room > 0.0 {
        *points = if units >= room / factor { cap } else { (*points + units * factor).min(cap) };
    }
    Ok(state)
}

/// The additive occupancy defense multiplier: base plus the per-level bonuses of
/// the projected levels plus the capital bonus when this system is the owner's
/// current capital, clamped to the policy range. Unknown levels yield no bonus.
fn defense_multiplier(definitions: &Definitions, military: i64, industrial: i64, strategic: i64, capital: bool) -> f64 {
    let bonus = |bonuses: &[LevelBonus], level: i64| bonuses.iter().find(|bonus| bonus.level == level).map_or(0.0, |bonus| bonus.bonus);
    let occupancy = &definitions.occupancy;
    let total = occupancy.base + bonus(&occupancy.military, military) + bonus(&occupancy.industrial, industrial)
        + bonus(&occupancy.strategic, strategic) + if capital { occupancy.capital } else { 0.0 };
    total.clamp(occupancy.min, occupancy.max)
}

/// Projects one system's committed rows to a development view at `now`.
/// Points only decay analytically; no fact is manufactured. A stored row that
/// disagrees with its system or carries corrupt values is an error, never a
/// silent zero.
pub(crate) fn project(
    development: Option<&DevelopmentRow>,
    system: Option<&SystemRow>,
    capital: Option<&PolicyRow>,
    history: &PolicyHistory,
    system_id: i64,
    now: i64,
) -> Result<DevelopmentView, CallError> {
    let segment = history.segments.iter().rev().find(|segment| segment.starts_at <= now)
        .ok_or_else(|| invalid("no active policy segment"))?;
    let definitions = &segment.definitions;
    if let Some(system) = system {
        if system.system_id != system_id { return Err(invalid("system row identity")); }
    }
    let state = match development {
        Some(row) => {
            if row.system_id != system_id || !finite_nonnegative(row.military_points) || !finite_nonnegative(row.industrial_points)
                || row.evaluated_at <= 0 || row.trend_start_at <= 0 || row.trend_start_at > row.evaluated_at
                || !finite_nonnegative(row.military_trend_base) || !finite_nonnegative(row.industrial_trend_base) {
                return Err(invalid("development row"));
            }
            advance(PointState { at: row.evaluated_at, military: row.military_points, industrial: row.industrial_points }, now, history)?
        }
        None => PointState { at: now, military: 0.0, industrial: 0.0 },
    };
    // The approved display policy: increasing only when the points exceed their
    // baseline at the start of the current UTC-aligned decay interval, or at the
    // current activation when it began inside the interval.
    let interval_start = now.checked_sub(now.rem_euclid(definitions.decay_interval_ticks)).ok_or_else(|| invalid("interval alignment"))?;
    let trend_start = interval_start.max(segment.starts_at);
    let (military_base, industrial_base) = match development {
        Some(row) if row.trend_start_at == trend_start => (row.military_trend_base, row.industrial_trend_base),
        Some(row) if row.trend_start_at < trend_start => {
            let start = advance(PointState { at: row.evaluated_at, military: row.military_points, industrial: row.industrial_points }, trend_start, history)?;
            (start.military, start.industrial)
        }
        Some(_) => return Err(invalid("trend anchor")),
        None => (0.0, 0.0),
    };
    let claim_time = system.and_then(|system| system.claim_time);
    let strategic_seconds = match claim_time {
        Some(claim) if claim <= 0 => return Err(invalid("claim time")),
        Some(claim) => now.checked_sub(claim).ok_or_else(|| invalid("claim age"))?.max(0) as f64 / FILETIME_SECOND as f64,
        None => 0.0,
    };
    let military = index_level(state.military, |index| definitions.military[index].max_points, definitions.military.len());
    let industrial = index_level(state.industrial, |index| definitions.industrial[index].max_points, definitions.industrial.len());
    let strategic = index_level(strategic_seconds, |index| definitions.strategic_seconds[index] as f64, definitions.strategic_seconds.len());
    let capital = capital.is_some_and(|policy| policy.current_capital_system == Some(system_id));
    Ok(DevelopmentView {
        system_id,
        military_points: state.military,
        industrial_points: state.industrial,
        strategic_seconds,
        military_increasing: state.military > military_base,
        industrial_increasing: state.industrial > industrial_base,
        strategic_increasing: claim_time.is_some(),
        military, industrial, strategic,
        defense_multiplier: defense_multiplier(definitions, military.level, industrial.level, strategic.level, capital),
    })
}

#[cfg(all(test, feature = "tests-content"))]
mod tests;
