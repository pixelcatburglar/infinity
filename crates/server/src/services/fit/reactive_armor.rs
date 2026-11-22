//! The adaptive armor hardener's transient effective-operand overlay.
//! Its four type-carried armor damage-resonance attributes are the operand the
//! SDE PreMul modifier rows read when computing the hull's armor resonances:
//! a running cycle adapts those operand values, so the already-evaluated
//! result lives here as a fit-scoped override short-circuiting operand reads
//! before the memo. The overlay is never persisted in node overrides and never
//! re-enters evaluation, so a fitted operand is applied exactly once. It is
//! runtime state only: cold loads have none, and module removal, offline,
//! inactive and type replacement clear it. The shipped client effect declares
//! those four PreMul rows
//! (`eve/common/script/dogma/pythonEffects/adaptiveHardener.py`); cycle timing
//! and damage sampling live with the space combat record.

use super::{Fit, Key};
use infinity_core::consts::dogma::{
    ATTR_ARMOR_EM_DAMAGE_RESONANCE, ATTR_ARMOR_EXPLOSIVE_DAMAGE_RESONANCE,
    ATTR_ARMOR_KINETIC_DAMAGE_RESONANCE, ATTR_ARMOR_THERMAL_DAMAGE_RESONANCE,
};

/// The four armor damage-resonance operand attributes the hardener type
/// carries, in raw damage order (em, thermal, kinetic, explosive). One shared
/// table serves operand overlay, cycle capture, publication and prime reads.
pub(crate) const ARMOR_RESONANCE: [i32; 4] = [
    ATTR_ARMOR_EM_DAMAGE_RESONANCE,
    ATTR_ARMOR_THERMAL_DAMAGE_RESONANCE,
    ATTR_ARMOR_KINETIC_DAMAGE_RESONANCE,
    ATTR_ARMOR_EXPLOSIVE_DAMAGE_RESONANCE,
];

/// One fitted hardener's current effective operand profile.
pub(crate) struct ReactiveArmorOverride {
    pub(crate) module: i64,
    pub(crate) values: [f64; 4],
}

impl Fit {
    /// Install or replace the operand profile of `module`; false when the
    /// values are non-finite or already installed unchanged.
    pub(crate) fn set_reactive_armor(&mut self, module: i64, values: [f64; 4]) -> bool {
        if !values.iter().all(|value| value.is_finite()) { return false; }
        if self.reactive_armor.as_ref().is_some_and(|overlay| overlay.module == module && overlay.values == values) {
            return false;
        }
        self.reactive_armor = Some(ReactiveArmorOverride { module, values });
        crate::poison::lock_or_exit(self.memo.get_mut()).clear();
        true
    }

    /// Drop the operand profile bound to `module`; false when none is bound.
    pub(crate) fn clear_reactive_armor(&mut self, module: i64) -> bool {
        if !self.has_reactive_armor(module) { return false; }
        self.reactive_armor = None;
        crate::poison::lock_or_exit(self.memo.get_mut()).clear();
        true
    }

    /// Whether `module` owns the installed operand profile.
    pub(crate) fn has_reactive_armor(&self, module: i64) -> bool {
        self.reactive_armor.as_ref().is_some_and(|overlay| overlay.module == module)
    }

    /// The overlay operand for one of the bound module's armor resonance
    /// attributes, or None for any other key or attribute.
    pub(crate) fn reactive_armor_operand(&self, key: Key, attr: i32) -> Option<f64> {
        let overlay = self.reactive_armor.as_ref()?;
        if key != Key::Module(overlay.module) { return None; }
        ARMOR_RESONANCE.iter().position(|&resonance| resonance == attr).map(|index| overlay.values[index])
    }
}
