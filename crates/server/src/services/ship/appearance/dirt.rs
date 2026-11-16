//! Dirt ratio and timestamp math mirroring the client `evegraphics/utils.py`
//! `RemapDirtLevel` piecewise curve and `CalcDirtLevelFromAge` week formula.
//! Timestamps are non-negative FILETIME values: `0` is the finite saturation
//! for a maximally aged hull, never a clean hull. The client's exact `0.55`
//! model-value debug sentinel does not apply here: every ratio in `[0, 1]` is
//! an ordinary point on the numeric curve. Only the age difference uses
//! floating point; absolute FILETIME values stay integers.
use infinity_core::consts::FILETIME_DAY;
use crate::client::{filetime_ticks_from_ms, ms_from_filetime_ticks};
use crate::services::{notify_error, CallError};
use infinity_core::messages::ship as text;

/// Client `RemapDirtLevel`: breakpoint between the two curve segments.
const RATIO_BREAKPOINT: f64 = 0.5;
/// Client `RemapDirtLevel`: low segment `level = -2 + 4 * ratio`.
const LOW_LEVEL_OFFSET: f64 = -2.0;
/// Client `RemapDirtLevel`: low-segment slope.
const LOW_LEVEL_SLOPE: f64 = 4.0;
/// Client `RemapDirtLevel`: high segment `level = 1.4 * (ratio - 0.5)`.
const HIGH_LEVEL_SLOPE: f64 = 1.4;
/// Client `CalcDirtLevelFromAge`: asymptotic level ceiling.
const MAX_LEVEL: f64 = 0.7;
/// Client `CalcDirtLevelFromAge`: age exponent.
const AGE_EXPONENT: f64 = 0.65;
/// Client `CalcDirtLevelFromAge`: age offset 1 / 2.7.
const AGE_OFFSET: f64 = 1.0 / 2.7;
/// Client `CalcDirtLevelFromAge`: one week is seven days.
const DAYS_PER_WEEK: i64 = 7;
const FILETIME_WEEK: i64 = DAYS_PER_WEEK * FILETIME_DAY;

/// A timestamp plus the model values the stored age actually produces.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct DirtSetting {
    pub timestamp: i64,
    pub attained_level: f64,
    pub attained_ratio: f64,
    /// True when the timestamp hit the finite lower boundary `0`, so the
    /// requested level could only be approximated, never reached exactly.
    pub saturated: bool,
}

pub(in crate::services::ship) fn refuse() -> CallError { notify_error(text::CLEANING_INACCESSIBLE) }

/// The timestamp matching `ratio` at `now`; `0` is the finite boundary reached
/// only when the requested level is at or beyond what the maximum representable
/// age produces. A non-finite or negative age, an age beyond the representable
/// domain or a subtraction overflow is a computation failure and refuses
/// instead of posing as saturation.
fn timestamp_for(ratio: f64, now: i64) -> Result<i64, CallError> {
    if ratio == 0.0 { return Ok(now); }
    if ratio == 1.0 { return Ok(0); }
    let level = if ratio < RATIO_BREAKPOINT {
        LOW_LEVEL_OFFSET + LOW_LEVEL_SLOPE * ratio
    } else {
        HIGH_LEVEL_SLOPE * (ratio - RATIO_BREAKPOINT)
    };
    let week_ms = ms_from_filetime_ticks(FILETIME_WEEK) as f64;
    let max_age_ms = ms_from_filetime_ticks(now);
    let max_weeks = max_age_ms as f64 / week_ms;
    let max_level = MAX_LEVEL - 1.0 / (max_weeks.powf(AGE_EXPONENT) + AGE_OFFSET);
    if level >= max_level { return Ok(0); }
    let weeks = (1.0 / (MAX_LEVEL - level) - AGE_OFFSET).powf(1.0 / AGE_EXPONENT);
    let age_ms = weeks * week_ms;
    // `filetime_ticks_from_ms` casts through `as i64`, which saturates silently;
    // bound the input explicitly before converting.
    if !age_ms.is_finite() || age_ms < 0.0 || age_ms > max_age_ms as f64 || age_ms > ms_from_filetime_ticks(i64::MAX) as f64 {
        return Err(refuse());
    }
    match now.checked_sub(filetime_ticks_from_ms(age_ms)) {
        Some(stamp) if stamp >= 0 => Ok(stamp),
        _ => Err(refuse()),
    }
}

/// The setting a stored timestamp actually produces at `now`: the model level
/// of its whole-millisecond age and the ratio that level maps back to.
/// A timestamp beyond `now` reads as age 0, matching the client's clamp of a
/// negative `TimeDiffInMs` difference. A stored `0` reports `saturated`.
pub(crate) fn describe(timestamp: i64, now: i64) -> Result<DirtSetting, CallError> {
    if now < 0 || timestamp < 0 { return Err(refuse()); }
    let age_ms = ms_from_filetime_ticks(now.checked_sub(timestamp).unwrap_or(0)).max(0);
    let weeks = age_ms as f64 / ms_from_filetime_ticks(FILETIME_WEEK) as f64;
    let attained_level = MAX_LEVEL - 1.0 / (weeks.powf(AGE_EXPONENT) + AGE_OFFSET);
    let attained_ratio = if attained_level < 0.0 {
        (attained_level - LOW_LEVEL_OFFSET) / LOW_LEVEL_SLOPE
    } else {
        attained_level / HIGH_LEVEL_SLOPE + RATIO_BREAKPOINT
    };
    Ok(DirtSetting { timestamp, attained_level, attained_ratio, saturated: timestamp == 0 })
}

/// Resolve a requested ratio to its timestamp and achieved setting.
/// The ratio must be finite and within `[0, 1]`; out-of-domain input is
/// refused, never clamped. `0` writes `now` exactly; `1` and any request beyond
/// the representable age saturate at timestamp `0` and report the real attained
/// values rather than the asymptotic `0.7` level.
pub(crate) fn from_ratio(ratio: f64, now: i64) -> Result<DirtSetting, CallError> {
    if !ratio.is_finite() || !(0.0..=1.0).contains(&ratio) || now < 0 { return Err(refuse()); }
    describe(timestamp_for(ratio, now)?, now)
}

#[cfg(all(test, feature = "tests-ships"))]
mod tests;
