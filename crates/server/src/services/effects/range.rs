//! Shared projected-effect range scaling from dogma optimal and falloff.
//! Mirrors the falloff curve used by client dogma effects; ranges are supplied
//! by SDE effect metadata. This is deterministic strength, not a hit roll.

/// Dogma's strength at one falloff beyond optimal.
const FALLOFF_STRENGTH: f64 = 0.5;

/// Optimal is inclusive. Without falloff, the effect ends immediately beyond it.
pub(crate) fn range_multiplier(distance: f64, optimal: f64, falloff: f64) -> f64 {
    if !distance.is_finite() || !optimal.is_finite() || !falloff.is_finite() { return 0.0; }
    if distance <= optimal.max(0.0) { return 1.0; }
    if falloff <= 0.0 { return 0.0; }
    let ratio = (distance - optimal.max(0.0)) / falloff;
    FALLOFF_STRENGTH.powf(ratio * ratio)
}

#[cfg(all(test, feature = "tests-effects"))] mod tests;
