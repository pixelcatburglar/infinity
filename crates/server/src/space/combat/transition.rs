//! Combat lifecycle transitions: freezing a live record for boarding, ship
//! swap or loss settlement, committing or rolling the transition back, and
//! the pending-combat mailbox the system's deferred workers claim. Moved out
//! of `combat.rs` to keep each file on one concern; behaviour is unchanged.
use super::*;

impl System {
    pub(crate) fn begin_combat_transition(&mut self, ship: i64) -> Option<(u64, ActorConditionSnapshot)> {
        let snapshot = self.combat_condition_snapshot(ship)?;
        let record = self.actor_combat.get_mut(&ship)?;
        if !matches!(record.phase, CombatPhase::Live) { return None; }
        let token = crate::space::targeting::next_generation();
        record.phase = CombatPhase::Transition { token };
        Some((token, snapshot))
    }

    fn republish_thawed_condition(&mut self, ship: i64) {
        self.refresh_fitted_stats(ship);
        let Some((max, now)) = self.combat_health_scalars(ship) else { return };
        let health = [0, 1, 2].map(|i| if max[i] > 0.0 {
            if i == 0 { (now[i] / max[i]).clamp(0.0, 1.0) }
            else { (1.0 - now[i] / max[i]).clamp(0.0, 1.0) }
        } else { 0.0 });
        if self.ball(ship).is_some_and(|ball| ball.health != health) {
            self.set_health(ship, health);
        }
    }

    pub(crate) fn cancel_combat_transition(&mut self, ship: i64, token: u64, generation: u64) -> bool {
        let Some(record) = self.actor_combat.get_mut(&ship) else { return false };
        if record.membership_generation != generation || record.phase != (CombatPhase::Transition { token }) { return false; }
        if matches!(record.pending, Some(PendingCombat::Transition { token: pending_token, .. }) if pending_token == token) {
            record.pending = None;
        }
        record.phase = CombatPhase::Live;
        self.republish_thawed_condition(ship);
        true
    }

    pub(crate) fn commit_combat_transition(&mut self, ship: i64, token: u64) -> bool {
        let Some(record) = self.actor_combat.get(&ship) else { return false };
        if record.phase != (CombatPhase::Transition { token }) { return false; }
        self.retire_body_combat_source(ship);
        self.actor_combat.remove(&ship);
        true
    }

    pub(crate) fn transfer_combat_state(&mut self, ship: i64, destination_generation: u64,
        snapshot: ActorConditionSnapshot, fit: Fit, fit_key: Key) -> bool {
        if self.persistent_generation(ship) != Some(destination_generation)
            || snapshot.item_id != ship || !valid_condition(&snapshot.condition)
            || self.actor_combat.get(&ship).is_some_and(|record| record.has_loaded_obligations()) { return false; }
        self.retire_body_combat_source(ship);
        let condition = snapshot.condition;
        self.nativefact_clock = self.nativefact_clock.max(condition.shield_at);
        let loaded = loaded::seed_map(fit.loaded_seeds());
        self.actor_combat.insert(ship, ActorCombatRecord {
            condition, fit, fit_key, fit_revision: 0, membership_generation: destination_generation,
            body_source_generation: None, source_sequence: 0,
            revision: snapshot.revision, saved_revision: snapshot.saved_revision,
            character_timer: None,
            contributions: snapshot.contributions.into_iter().map(|e| (e.attacker_id, e)).collect(),
            phase: CombatPhase::Live, pending: None, loaded, spool_readouts: Vec::new(),
            reactive_armor: None,
        });
        self.arm_character_expiry(ship);
        self.refresh_fitted_stats(ship);
        true
    }


    pub(crate) fn mark_pending_transition(
        &mut self,
        ship: i64,
        token: u64,
        generation: u64,
        callback: Arc<dyn Fn() -> crate::space::npc::NpcFuture + Send + Sync>,
    ) -> bool {
        let Some(record) = self.actor_combat.get_mut(&ship) else { return false };
        if record.membership_generation != generation || record.phase != (CombatPhase::Transition { token }) { return false; }
        record.pending = Some(PendingCombat::Transition { token, generation, callback, in_flight: true });
        true
    }

    pub(crate) fn release_pending_combat(&mut self, ship: i64, token: u64, generation: u64) -> bool {
        let Some(record) = self.actor_combat.get_mut(&ship) else { return false };
        let Some(pending) = record.pending.as_mut() else { return false };
        let (stored_token, stored_generation, in_flight) = match pending {
            PendingCombat::Dying { token, generation, in_flight, .. }
            | PendingCombat::Transition { token, generation, in_flight, .. } => (token, generation, in_flight),
        };
        if *stored_token != token || *stored_generation != generation { return false; }
        *in_flight = false;
        true
    }

    pub(crate) fn claim_pending_combat(&mut self) -> Vec<(i64, PendingCombat)> {
        self.actor_combat.iter_mut().filter_map(|(&ship, record)| {
            let pending = record.pending.as_mut()?;
            let in_flight = match pending {
                PendingCombat::Dying { in_flight, .. } | PendingCombat::Transition { in_flight, .. } => in_flight,
            };
            if *in_flight { return None; }
            *in_flight = true;
            Some((ship, pending.clone()))
        }).collect()
    }

    pub(crate) fn restore_known_rollback(&mut self, ship: i64, token: u64, generation: u64) -> bool {
        if self.ball(ship).is_none() || self.persistent_generation(ship).is_some_and(|current| current != generation) { return false; }
        let Some(record) = self.actor_combat.get_mut(&ship) else { return false };
        if record.membership_generation != generation || record.phase != (CombatPhase::Dying { token }) { return false; }
        let Some(next_revision) = record.revision.checked_add(1) else { return false };
        let Some(PendingCombat::Dying { snapshot, .. }) = record.pending.take() else { return false };
        record.condition = snapshot.condition.clone();
        record.contributions = snapshot.undo_contributions.clone().into_iter().map(|e| (e.attacker_id, e)).collect();
        record.revision = next_revision;
        record.saved_revision = record.saved_revision.min(record.revision.saturating_sub(1));
        record.phase = CombatPhase::Live;
        self.republish_thawed_condition(ship);
        true
    }

    pub(crate) fn finalize_dying(&mut self, ship: i64, token: u64, generation: u64) -> bool {
        let Some(record) = self.actor_combat.get(&ship) else { return false };
        if record.membership_generation != generation || record.phase != (CombatPhase::Dying { token }) { return false; }
        self.retire_body_combat_source(ship);
        self.actor_combat.remove(&ship);
        true
    }
}
