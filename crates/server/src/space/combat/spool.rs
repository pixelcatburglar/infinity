//! Presentation-only precursor-turret ramp readouts on the actor's combat
//! record: the HUD-visible `damageMultiplierBonusCurrent` (SDE 5804) and
//! `damageMultiplierBonusMaxTimestamp` (SDE 5818, client "blue time" FILETIME)
//! values behind `OnModuleAttributeChanges` and the cold `ship_info` prime.
//! Readouts serve display and notification dedup only — damage never reads
//! them — and each entry lives inside its ship + membership + activation
//! generation fence: it is never persisted and dies with its activation.
use crate::space::System;
use crate::services::effects::notify::attribute_change;
use infinity_core::consts::dogma::{ATTR_DAMAGE_MULTIPLIER_BONUS_CURRENT, ATTR_DAMAGE_MULTIPLIER_BONUS_MAX_TIMESTAMP};
use marshal::Value;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// One module member's displayed ramp: the bonus its last accepted shot
/// applied and the FILETIME the ramp reaches its cap, bound to the activation
/// generation and the source membership captured at first bind.
pub(crate) struct SpoolReadout {
    pub(crate) module: i64,
    pub(crate) master: i64,
    pub(crate) source_membership: u64,
    pub(crate) activation: Arc<AtomicBool>,
    pub(crate) bonus: f64,
    pub(crate) max_at: i64,
}

impl System {
    /// Emit both attribute rows for one module to its pilot.
    fn spool_notify(&self, pilot: i64, module: i64, bonus: f64, old_bonus: f64, max_at: i64, old_max: i64) {
        let rows = Value::list([
            attribute_change(pilot, Value::Int(module), ATTR_DAMAGE_MULTIPLIER_BONUS_CURRENT,
                Value::Float(bonus), Value::Float(old_bonus)),
            attribute_change(pilot, Value::Int(module), ATTR_DAMAGE_MULTIPLIER_BONUS_MAX_TIMESTAMP,
                Value::Float(max_at as f64), Value::Float(old_max as f64)),
        ]);
        self.notify_pilot(pilot, "OnModuleAttributeChanges", vec![rows]);
    }

    /// Register the readout for a member's newly bound activation, fenced by
    /// the ship's current combat membership. Only the bind allocates; a repeat
    /// for the same activation generation never grows the list. The baseline
    /// rows prime the HUD immediately so the ramp countdown shows before the
    /// first shot lands.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn spool_readout_begin(&mut self, ship: i64, module: i64, master: i64,
        membership: u64, activation: &Arc<AtomicBool>, max_at: i64, pilot: i64)
    {
        let Some(record) = self.actor_combat.get_mut(&ship) else { return };
        if record.membership_generation != membership { return; }
        if record.spool_readouts.iter().any(|readout| readout.module == module && readout.master == master && Arc::ptr_eq(&readout.activation, activation)) { return; }
        record.spool_readouts.push(SpoolReadout {
            module, master, source_membership: membership,
            activation: activation.clone(), bonus: 0.0, max_at,
        });
        self.spool_notify(pilot, module, 0.0, 0.0, max_at, 0);
    }

    /// Update one member's readout inside its generation fence and emit the
    /// two attribute rows only when the displayed values actually changed —
    /// a ramp already at its cap sends nothing further.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn spool_readout_publish(&mut self, ship: i64, module: i64,
        activation: &Arc<AtomicBool>, bonus: f64, max_at: i64, pilot: i64)
    {
        let Some(index) = self.actor_combat.get(&ship).and_then(|record|
            record.spool_readouts.iter().position(|readout|
                readout.module == module && Arc::ptr_eq(&readout.activation, activation)
                    && readout.source_membership == record.membership_generation))
        else { return };
        let (old_bonus, old_max) = {
            let record = self.actor_combat.get_mut(&ship).expect("readout index checked");
            let readout = &mut record.spool_readouts[index];
            let old = (readout.bonus, readout.max_at);
            readout.bonus = bonus;
            readout.max_at = max_at;
            old
        };
        if old_bonus == bonus && old_max == max_at { return; }
        self.spool_notify(pilot, module, bonus, old_bonus, max_at, old_max);
    }

    /// Drop this activation's readouts and report zeroed attributes for every
    /// removed module. Only entries bound to this exact generation are
    /// removed: a successor activation's rows are never touched.
    pub(crate) fn spool_readout_clear(&mut self, ship: i64, activation: &Arc<AtomicBool>) {
        let removed: Vec<(i64, f64, i64)> = {
            let Some(record) = self.actor_combat.get_mut(&ship) else { return };
            let mut removed = Vec::new();
            record.spool_readouts.retain(|readout| {
                if Arc::ptr_eq(&readout.activation, activation) {
                    removed.push((readout.module, readout.bonus, readout.max_at));
                    false
                } else { true }
            });
            removed
        };
        if removed.is_empty() { return; }
        let Some(pilot) = self.ball(ship).and_then(|ball| ball.char_id) else { return };
        for (module, bonus, max_at) in removed {
            self.spool_notify(pilot, module, 0.0, bonus, 0, max_at);
        }
    }

    /// The live readouts for a cold `ship_info` prime: (module, bonus, max_at)
    /// for every entry whose generation flag still stands — a stopped
    /// activation's leftovers are skipped even before their clear lands.
    pub(crate) fn spool_readouts(&self, ship: i64) -> Vec<(i64, f64, i64)> {
        self.actor_combat.get(&ship).map(|record| record.spool_readouts.iter()
            .filter(|readout| !readout.activation.load(Ordering::Relaxed))
            .map(|readout| (readout.module, readout.bonus, readout.max_at))
            .collect()).unwrap_or_default()
    }
}

#[cfg(all(test, feature = "tests-space"))] mod tests;
