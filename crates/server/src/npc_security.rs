//! Frozen NPC-kill security multiplier and composable new-generation gain.
//! No client protocol, SQL, configuration or skill formula lives here: callers
//! freeze a finite nonnegative multiplier at attack admission and a validated
//! base rate at accepted reward recording. Durable proof compares the canonical
//! stored state, never a recomputed history. Binary64 grouping is not bitwise
//! associative, so mathematical checks use a stated tolerance while receipts
//! compare exact canonical values. A finite log that overflows on merge becomes
//! Full, the only representable result once survival is below the range.


/// Non-floating sentinel for an exact full gain. Never reinterpret these bits.
const FULL: u64 = u64::MAX;
/// Positive zero: the canonical stored log of a zero new-generation gain.
const ZERO_BITS: u64 = 0;

/// Admission-frozen finite nonnegative security multiplier.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub(crate) struct FrozenMultiplier(u64);

impl FrozenMultiplier {
    /// Finite nonnegative values only. Negative zero stores as positive zero.
    pub(crate) fn new(value: f64) -> Option<Self> {
        if !value.is_finite() || value < 0.0 { return None; }
        Some(Self(if value == 0.0 { 0 } else { value.to_bits() }))
    }

    pub(crate) fn value(self) -> f64 { f64::from_bits(self.0) }
}


/// Composable new-generation security gain, or a legacy survival factor.
///
/// Finite state stores `H = sum(-ln(1-g))` as canonical bits. NULL storage is
/// Full; positive zero is a zero gain. Restore never recomputes a logarithm.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct SecurityGain(u64);

impl SecurityGain {
    pub(crate) const ZERO: Self = Self(ZERO_BITS);

    /// One accepted kill: `g = min(rate * multiplier, 1)` for a validated rate.
    pub(crate) fn for_kill(rate: f64, multiplier: FrozenMultiplier) -> Self {
        debug_assert!(rate.is_finite() && (0.0..=1.0).contains(&rate));
        let gain = rate * multiplier.value();
        if !(gain < 1.0) { return Self(FULL); }
        if gain == 0.0 { return Self::ZERO; }
        Self::from_finite_gain(gain)
    }

    /// Original settlement factor `(1-rate)^count` for a validated rate.
    pub(crate) fn for_legacy(rate: f64, count: u64) -> Self {
        debug_assert!(rate.is_finite() && (0.0..=1.0).contains(&rate));
        if count == 0 || rate <= 0.0 { return Self::ZERO; }
        if rate >= 1.0 { return Self(FULL); }
        let log = (count as f64) * -(-rate).ln_1p();
        if !log.is_finite() { return Self(FULL); }
        Self(log.to_bits())
    }

    /// Identity-preserving log addition. Full absorbs; overflow becomes Full.
    pub(crate) fn merge(self, other: Self) -> Self {
        if self.0 == FULL || other.0 == FULL { return Self(FULL); }
        if self.0 == ZERO_BITS { return other; }
        if other.0 == ZERO_BITS { return self; }
        let sum = f64::from_bits(self.0) + f64::from_bits(other.0);
        if !sum.is_finite() { Self(FULL) } else { Self(sum.to_bits()) }
    }

    /// Toward-limit fraction: 1 for Full, otherwise `-expm1(-H)`.
    pub(crate) fn fraction(self) -> f64 {
        if self.0 == FULL { 1.0 } else { -(-f64::from_bits(self.0)).exp_m1() }
    }

    /// NULL is Full. A finite nonnegative value is its exact log; anything else refuses.
    pub(crate) fn from_storage(value: Option<f64>) -> Option<Self> {
        let Some(value) = value else { return Some(Self(FULL)); };
        if value.is_finite() && value >= 0.0 { Some(Self(if value == 0.0 { ZERO_BITS } else { value.to_bits() })) } else { None }
    }

    /// NULL for Full, otherwise the exact finite log, with zero stored as positive zero.
    pub(crate) fn storage(self) -> Option<f64> {
        if self.0 == FULL { None } else { Some(f64::from_bits(self.0)) }
    }

    fn from_finite_gain(gain: f64) -> Self {
        let log = -(-gain).ln_1p();
        if !log.is_finite() { Self(FULL) } else { Self(log.to_bits()) }
    }
}


#[cfg(all(test, feature = "tests-runtime"))]
mod tests;
