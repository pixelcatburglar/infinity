//! Read-only inspection of actor-owned state before unloading a system.
//! This server-side lifecycle check uses the SDE and in-memory ballpark, not a client API.
//! Worker, mailbox, cross-system and combat checkpoint state are checked separately.

use super::*;

impl System {
    pub(crate) fn unload_blocked(&self) -> Option<&'static str> {
        if !self.pilots.is_empty() { return Some("pilots"); }
        if self.observers.keys().any(|&client| self.observer_token(client).is_some()) { return Some("observers"); }
        if self.balls.values().any(|ball| ball.char_id.is_some()) { return Some("character_ball"); }
        if !self.warp_exits.is_empty() || !self.warp_arrivals.is_empty() { return Some("warp"); }
        if !self.timers.is_empty() || !self.asteroid_timers.is_empty() { return Some("timers"); }
        if !self.actions.is_empty() { return Some("actions"); }
        if !self.destiny_flush_acks.is_empty() { return Some("destiny_flush"); }
        if self.delivery_batching { return Some("notifications"); }
        if !self.pending_uncloak.is_empty() { return Some("pending_uncloak"); }
        // Only one exact, marked, quiescent NPC cohort may exempt the four
        // NPC-owned blockers below; any other group, record or ball keeps the
        // strict original refusal.
        let npc_cohort = !self.brains.is_empty() && (match self.npc_sleep_blocked() {
            None => true,
            Some(reason) => return Some(reason),
        });
        if !self.drones.is_empty() || !self.drone_reservations.is_empty() || !self.drone_aggressors.is_empty() { return Some("drones"); }
        if (!self.groups.is_empty() || !self.member_groups.is_empty()) && !npc_cohort { return Some("groups"); }
        if !self.effect_staging.is_empty() { return Some("effect_jobs"); }
        if !self.projections.is_empty() || !self.projection_targets.is_empty() { return Some("projections"); }
        if !self.tractors.is_empty() || !self.tractor_epochs.is_empty() { return Some("tractors"); }
        if !self.module_cloaks.is_empty() || !self.cloak_reactivation.is_empty() || !self.cloak_targeting.is_empty() { return Some("cloak"); }
        if !self.compressors.is_empty() { return Some("compressors"); }
        if crate::services::effects::heat::active(&self.heat) { return Some("heat"); }
        if !self.gm_previews.is_empty() { return Some("gm_previews"); }
        if !self.actor_combat.is_empty() && !npc_cohort { return Some("actor_combat"); }
        let Some(sde) = self.sde() else { return Some("unsupported_scene"); };
        if !sde.map.is_public_system(self.id) { return Some("unsupported_scene"); }
        // Any sealed claim (Claimed, parked or unknown) blocks unload even
        // when its ball already left and only the tombstone index remains.
        if self.npc_wreck_claims_pending() { return Some("npc_wreck_claims"); }
        if self.balls.values().any(|ball| {
            let celestial = sde.map.celestial(ball.id).is_some_and(|row| {
                row.solar_system_id == Some(self.id) && row.type_id == Some(i64::from(ball.type_id))
                    && ball.mode == MODE_RIGID && ball.cloak == 0
            });
            // Only an exact unclaimed transient NPC root exempts here; claimed,
            // unknown or tombstoned roots keep blocking through the claim check.
            !celestial && self.rock_type(ball.id).is_none() && !(npc_cohort && self.brains.contains_key(&ball.id))
                && !self.npc_wreck_discardable(ball.id)
        }) || self.rocks.keys().any(|id| self.rock_type(*id).is_none()) {
            return Some("dynamic_ball");
        }
        sovereignty::links::unload_blocked(self)
    }
    /// Occupancy for the automatic idle window only: every pilot entry and the
    /// whole observers map count, including a pending observer still awaiting
    /// its initial SetState; a closed channel counts until the regular prune.
    /// This is deliberately stricter than the manual `unload_blocked` reason.
    pub(in crate::space) fn unload_occupied(&self) -> bool {
        !self.pilots.is_empty() || !self.observers.is_empty()
    }
}

#[cfg(all(test, feature = "tests-space"))] mod tests;
