//! Shared precursor-turret spool ramp for the targetDisintegratorAttack
//! effect (SDE 6995): pure damage-bonus arithmetic and the activation-target
//! binding that player cycle members and NPC brains both run on.
//!
//! Mirrors the client's `eve/client/script/environment/effects/triglavianBeam.py`
//! Repeat semantics — the k-th admitted shot (k counting from zero) applies
//! `raw * (1 + min(k * step, maximum))` — and the graphicInfo contract read in
//! `TriglavianBeam.__init__`. The ramp accumulates a single bonus; a rolled
//! miss or a fully resisted accepted hit advances it, while a refused guard,
//! an unadmitted member or an out-of-optimal shot does not. State holds no
//! `World`, database or `Fit`; parameters are captured per shot from the
//! effective resident fit.
use marshal::Value;

/// The identity one ramp belongs to: the shooter's combat membership, the
/// activation's own generation and the locked target's incarnation. A player
/// member's `activation_generation` is its `NativeCombatGeneration` id; an
/// NPC's is its brain target generation. `source_membership` for an NPC is its
/// combat record's membership generation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SpoolBinding {
    pub source_membership: u64,
    pub activation_generation: u64,
    pub target: i64,
    pub target_membership: u64,
}

/// Fitted ramp values captured from the effective resident fit: the per-shot
/// `damageMultiplierBonusPerCycle` step and `damageMultiplierBonusMax` cap
/// (SDE 2733/2734, plain ratios, not percentages).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct SpoolParameters {
    pub step: f64,
    pub maximum: f64,
}

/// Runtime ramp: the bound identity plus the next admitted shot's bonus.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct SpoolState {
    binding: Option<SpoolBinding>,
    next_bonus: f64,
}

impl SpoolState {
    /// Bind to an identity; binding the same one is a no-op, a different one
    /// resets the ramp so a replaced incarnation never inherits the bonus.
    pub(crate) fn bind(&mut self, binding: SpoolBinding) {
        if self.binding == Some(binding) { return; }
        *self = Self { binding: Some(binding), next_bonus: 0.0 };
    }

    /// The bound identity, if any.
    pub(crate) fn binding(&self) -> Option<SpoolBinding> {
        self.binding
    }

    /// The bonus the next admitted shot applies, clamped to the parameters'
    /// cap so a parameter change never re-fires above the new maximum.
    pub(crate) fn current(&self, parameters: SpoolParameters) -> f64 {
        self.next_bonus.clamp(0.0, parameters.maximum.max(0.0))
    }

    /// Advance the ramp after one accepted shot, saturating at the cap.
    pub(crate) fn advance(&mut self, parameters: SpoolParameters) {
        self.next_bonus = (self.next_bonus + parameters.step).clamp(0.0, parameters.maximum.max(0.0));
    }

    /// Drop the binding and the accumulated bonus.
    pub(crate) fn reset(&mut self) {
        *self = Self::default();
    }
}

impl SpoolParameters {
    /// FILETIME the ramp reaches its maximum from `current`: the activation's
    /// nominal start plus the remaining whole cycles of `duration_ms`. Zero
    /// when the ramp cannot progress (a non-positive cap or no positive step
    /// below the cap); an already-full ramp keeps its arrival time.
    pub(crate) fn maximum_at(self, start_filetime: i64, current: f64, duration_ms: f64) -> i64 {
        if self.maximum <= 0.0 { return 0; }
        if current >= self.maximum { return start_filetime; }
        if self.step <= 0.0 || !duration_ms.is_finite() || duration_ms <= 0.0 { return 0; }
        let cycles = ((self.maximum - current) / self.step).ceil();
        if !cycles.is_finite() || cycles > i64::MAX as f64 { return 0; }
        start_filetime.saturating_add(
            crate::client::filetime_ticks_from_ms(duration_ms).saturating_mul(cycles as i64))
    }
}

/// The `graphicInfo` payload `effects.TriglavianBeam` requires; the client's
/// historical `mulitplierBonusPerCycle` spelling is kept verbatim, with no
/// corrected alias.
pub(crate) fn graphic_info(parameters: SpoolParameters) -> Value {
    Value::dict([
        ("mulitplierBonusPerCycle", Value::Float(parameters.step)),
        ("multiplierBonusMax", Value::Float(parameters.maximum)),
    ])
}

#[cfg(all(test, feature = "tests-effects"))] mod tests;
