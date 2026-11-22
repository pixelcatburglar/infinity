//! Damageable projectile profiles: transient structure-only conditions built
//! from the loaded SDE type row and the shared ship absorber. Missiles and
//! bombs are not ships: they own no combat record, produce no combat facts,
//! killmail, loot, bounty or durable loss, and never enter `plan_damage` or
//! `complete_damage_outcome`. The capability only tracks hit points; terminal
//! ownership, ball removal and callback completion stay in `System`.
use crate::services::combat::groups_health::{Condition, ShipStats};
use crate::services::fit::base_attribute;
use infinity_core::consts::dogma::{ATTR_STRUCTURE_EM_DAMAGE_RESONANCE, ATTR_STRUCTURE_THERMAL_DAMAGE_RESONANCE,
    ATTR_STRUCTURE_KINETIC_DAMAGE_RESONANCE, ATTR_STRUCTURE_EXPLOSIVE_DAMAGE_RESONANCE, ATTR_STRUCTURE_HP};

/// SDE dgmAttributeTypes structure resonances in raw damage order
/// (em, thermal, kinetic, explosive): the structure row of the ship
/// resonance table, applied against the only layer a projectile carries.
const ATTR_STRUCTURE_RESONANCE: [i32; 4] = [
    ATTR_STRUCTURE_EM_DAMAGE_RESONANCE,
    ATTR_STRUCTURE_THERMAL_DAMAGE_RESONANCE,
    ATTR_STRUCTURE_KINETIC_DAMAGE_RESONANCE,
    ATTR_STRUCTURE_EXPLOSIVE_DAMAGE_RESONANCE,
];

/// One live projectile's remaining structure and typed resonance. Only the
/// structure layer carries capacity; shield and armor stay at zero so the
/// shared absorber passes every point through to structure.
pub(crate) struct DamageableProjectile {
    condition: Condition,
}

/// The result of one accepted typed damage application.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ProjectileDamageOutcome {
    /// Hit points removed from the projectile's layers.
    pub(crate) dealt: f64,
    /// Structure left after this application.
    pub(crate) remaining_structure: f64,
    /// Structure reached zero: the caller owns the shared terminal.
    pub(crate) destroyed: bool,
}

impl DamageableProjectile {
    /// Build the profile of a launched charge from the already-loaded SDE:
    /// structure capacity from dogma attribute `hp` and the four structure
    /// resonances, read through the shared type-row/default order. A type
    /// without a positive finite structure capacity is refused.
    pub(crate) fn from_sde(sde: &crate::sde::Sde, type_id: i32) -> Option<Self> {
        let data = sde.types.get(type_id);
        let base = |attr: i32| base_attribute(data, sde.dogma.attribute(attr), attr);
        Self::from_stats(ShipStats {
            max: [0.0, 0.0, base(ATTR_STRUCTURE_HP)],
            resonance: [[1.0; 4], [1.0; 4], ATTR_STRUCTURE_RESONANCE.map(base)],
            shield_tau_ms: 0.0,
        })
    }

    /// Build a profile from explicit stats. A projectile must carry a positive
    /// finite structure capacity; every other value is checked by the shared
    /// ship condition validator.
    pub(crate) fn from_stats(stats: ShipStats) -> Option<Self> {
        if !stats.max[2].is_finite() || stats.max[2] <= 0.0 { return None; }
        let condition = Condition { now: stats.max, shield_at: crate::client::filetime_now(), stats };
        super::combat::valid_condition(&condition).then_some(Self { condition })
    }

    /// Apply one typed raw vector through the shared layer absorber.
    /// `None` means the vector was refused (non-finite or negative) and the
    /// condition is unchanged; `Some` reports the applied result, including
    /// a zero-damage no-op. The capability never removes itself or mutates
    /// the scene: `destroyed` only tells the owner to run the terminal.
    pub(crate) fn apply(&mut self, raw: [f64; 4]) -> Option<ProjectileDamageOutcome> {
        if !super::combat::valid_raw(raw) { return None; }
        let absorbed = super::combat::absorb_preserving_overdamage(&mut self.condition, raw);
        Some(ProjectileDamageOutcome {
            dealt: absorbed.layers.iter().sum(),
            remaining_structure: self.condition.now[2],
            destroyed: self.condition.now[2] <= 0.0,
        })
    }
}
