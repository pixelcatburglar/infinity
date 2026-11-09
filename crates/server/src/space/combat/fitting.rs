//! Cold committed fitting publication against the current actor-owned Fit and condition.
//! Dogma values use Fit's evaluator; durable writes and transaction dispatch are external.

use super::{CombatPhase, System};
use infinity_core::consts::dogma::{ATTR_ARMOR_DAMAGE, ATTR_ARMOR_HP, ATTR_CAPACITOR_CAPACITY,
    ATTR_CHARGE, ATTR_HULL_DAMAGE, ATTR_RECHARGE_RATE, ATTR_SHIELD_CAPACITY,
    ATTR_SHIELD_CHARGE, ATTR_SHIELD_RECHARGE_RATE, ATTR_STRUCTURE_HP};
use crate::services::combat::groups_health::rebase_capacitor;
use crate::services::effects::{heat::{self, FittingHeatCleanup}, notify::attribute_change};
use crate::services::fit::{attributes::CommittedFitting, Key};
use marshal::Value;

impl System {
    /// Clear overload without rebasing until the entire committed patch has landed.
    pub(crate) fn clear_fitting_overload(&mut self, ship: i64, module: i64) {
        if let Some(record) = self.actor_combat.get_mut(&ship) {
            record.fit.set_module_overloaded(module, false);
        }
    }

    /// Apply one committed fitting patch to the currently piloted live incarnation.
    pub(crate) fn publish_fitting(&mut self, ship: i64, owner: i64, generation: u64,
        patch: CommittedFitting, cleanup: Vec<FittingHeatCleanup>) -> bool {
        if self.persistent_generation(ship) != Some(generation)
            || !self.pilot_ball(owner).is_some_and(|ball| ball.id == ship && ball.char_id == Some(owner) && ball.owner_id == owner)
            || !self.actor_combat.get(&ship).is_some_and(|record| record.membership_generation == generation && record.phase == CombatPhase::Live) {
            return false;
        }
        let record = &self.actor_combat[&ship];
        let mut candidates = record.fit.fitting_candidates(&record.fit);
        candidates.extend_from_slice(patch.candidates());
        candidates.sort_unstable();
        candidates.dedup();
        let before = record.fit.fitting_values(&candidates);
        let old_cap = record.fit.value(Key::Ship, ATTR_CAPACITOR_CAPACITY);
        let old_tau = record.fit.value(Key::Ship, ATTR_RECHARGE_RATE);
        let old_shield_max = record.fit.value(Key::Ship, ATTR_SHIELD_CAPACITY);
        let old_shield_tau = record.fit.value(Key::Ship, ATTR_SHIELD_RECHARGE_RATE);
        let old_condition = self.ship_condition(ship).unwrap();
        for chunk in cleanup { heat::apply_fitting_cleanup(self, chunk); }
        let record = self.actor_combat.get_mut(&ship).unwrap();
        record.fit.apply_committed_fitting(patch);
        record.fit_revision = record.fit_revision.saturating_add(1);
        let changes = record.fit.fitting_changes(&before);
        let new_cap = record.fit.value(Key::Ship, ATTR_CAPACITOR_CAPACITY);
        let new_tau = record.fit.value(Key::Ship, ATTR_RECHARGE_RATE);
        let new_shield_max = record.fit.value(Key::Ship, ATTR_SHIELD_CAPACITY);
        let new_shield_tau = record.fit.value(Key::Ship, ATTR_SHIELD_RECHARGE_RATE);
        let motion = crate::services::effects::propulsion::kinematics(&record.fit, Key::Ship);
        let cap = (old_cap != new_cap || old_tau != new_tau)
            .then(|| rebase_capacitor(ship, old_cap, old_tau, new_cap, new_tau));
        self.refresh_fitted_stats(ship);
        let revision = self.projection_revision(ship);
        crate::services::effects::propulsion::queue_fitting_kinematics(self, ship, generation, revision, motion);
        let new_condition = self.ship_condition(ship).unwrap();
        let mut notices = Vec::new();
        for (key, attr, old, new) in changes {
            let item = match key { Key::Ship => ship, Key::Module(id) => id, _ => continue };
            notices.push(attribute_change(owner, Value::Int(item), attr, Value::Float(new), Value::Float(old)));
            if key != Key::Ship { continue; }
            let pair = match attr {
                ATTR_SHIELD_CAPACITY => Some((ATTR_SHIELD_CHARGE, old_condition.0, new_condition.0)),
                ATTR_ARMOR_HP => Some((ATTR_ARMOR_DAMAGE, old_condition.1, new_condition.1)),
                ATTR_STRUCTURE_HP => Some((ATTR_HULL_DAMAGE, old_condition.2, new_condition.2)),
                ATTR_CAPACITOR_CAPACITY => cap.map(|(old, new)| (ATTR_CHARGE, old, new)),
                _ => None,
            };
            if let Some((attr, old, new)) = pair {
                notices.push(attribute_change(owner, Value::Int(ship), attr, Value::Float(new), Value::Float(old)));
            }
        }
        if old_shield_max == new_shield_max && old_shield_tau != new_shield_tau {
            notices.push(attribute_change(owner, Value::Int(ship), ATTR_SHIELD_CHARGE,
                Value::Float(new_condition.0), Value::Float(old_condition.0)));
        }
        if old_cap == new_cap {
            if let Some((old, new)) = cap {
                notices.push(attribute_change(owner, Value::Int(ship), ATTR_CHARGE, Value::Float(new), Value::Float(old)));
            }
        }
        if !notices.is_empty() {
            self.notify_pilot(owner, "OnModuleAttributeChanges", vec![Value::list(notices)]);
        }
        true
    }
}

#[cfg(test)] mod tests;
