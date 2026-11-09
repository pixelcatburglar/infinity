//! Tracks aggressors and assigns idle drones to automatic engagement.
//! Client preferences are in eve/client/script/ui/inflight/drones/droneSettings.py.
//! Uses system actor state, drone settings and crimewatch; no tables directly.
//! The client does not specify the server's target selection algorithm.

use super::*;
use infinity_core::consts::OWNER_UNOWNED;

pub(in crate::space) struct Aggressors {
    ship: i64,
    sources: Vec<i64>,
}

impl System {
    pub(super) fn record_drone_aggressor(&mut self, attacker: i64, target: i64) {
        let Some(owner) = self.ball(target).and_then(|b| b.char_id) else { return };
        if self.pilot_ball(owner).is_none_or(|b| b.id != target) || attacker == target
            || self.ball(attacker).is_none_or(|b| b.health[2] <= 0.0)
            || self.combat_owner(attacker) == Some(owner)
            || (!self.is_npc(attacker) && self.combat_owner(attacker).is_none_or(|c| c == OWNER_UNOWNED)) {
            return;
        }
        let roster = self.drone_aggressors.entry(owner).or_insert_with(|| Aggressors { ship: target, sources: Vec::new() });
        if roster.ship != target { roster.ship = target; roster.sources.clear(); }
        if !roster.sources.contains(&attacker) { roster.sources.push(attacker); }
    }

    pub(super) fn clear_drone_aggressors(&mut self, ship: i64) {
        self.drone_aggressors.retain(|_, roster| roster.ship != ship);
    }
    pub(super) fn remove_drone_aggressor(&mut self, source: i64) {
        self.drone_aggressors.retain(|_, roster| {
            roster.sources.retain(|&id| id != source);
            !roster.sources.is_empty()
        });
    }


    pub(super) fn think_automatic_drones(&mut self) {
        if self.drone_aggressors.is_empty() { return; }
        let mut rosters = std::mem::take(&mut self.drone_aggressors);
        let mut drones = std::mem::take(&mut self.automatic_drones);
        let mut targets = std::mem::take(&mut self.automatic_targets);
        let now = crate::client::filetime_now();
        rosters.retain(|&owner, roster| {
            if self.pilot_ball(owner).is_none_or(|b| b.id != roster.ship || b.health[2] <= 0.0) { return false; }
            roster.sources.retain(|&id| self.ball(id).is_some_and(|b| b.health[2] > 0.0)
                && self.combat_owner(id) != Some(owner)
                && (self.is_npc(id) || self.combat_owner(id).is_some_and(|c| c != OWNER_UNOWNED)));
            if roster.sources.is_empty() { return false; }
            let settings = crate::services::drones::settings::current(owner);
            if !settings.aggressive { return true; }
            let Some(ship) = self.ball(roster.ship) else { return false };
            if ship.cloak != 0 || ship.mode == MODE_WARP || crate::services::combat::jamming::is_jammed(roster.ship, self.ms) { return true; }
            drones.clear();
            drones.extend(self.drones().filter(|d| d.owner == owner && d.controller == Some(roster.ship)
                && d.controller_owner == Some(owner) && d.order == DroneOrder::Idle && d.pending.is_none()).map(|d| d.id));
            drones.sort_unstable();
            roster.sources.sort_unstable();
            let mut index = 0;
            for &id in &drones {
                let Some(d) = self.drone(id) else { continue };
                let Some((ship, me)) = self.ball(roster.ship).zip(self.ball(id)) else { continue };
                if me.health[2] <= 0.0 || me.cloak != 0 || me.mode == MODE_WARP
                    || crate::services::combat::jamming::is_jammed(id, self.ms) || !d.spec.control_range.is_finite() { continue; }
                targets.clear();
                targets.extend(roster.sources.iter().copied().filter(|&source| {
                    let Some(victim) = self.ball(source) else { return false };
                    targeting::target_visible(victim)
                        && dist(ship.position_at(self.ms), victim.position_at(self.ms)) <= d.spec.control_range
                        && (d.spec.max_target_range == 0.0 || dist(me.position_at(self.ms), victim.position_at(self.ms)) <= d.spec.max_target_range)
                        && (self.is_npc(source) || self.combat_owner(source).is_some_and(|aggressor|
                            crate::services::crimewatch::retaliation_allowed(owner, aggressor, now)))
                }));
                if !targets.is_empty() {
                    let target = targets[if settings.focus_fire { 0 } else { index % targets.len() }];
                    if self.drone_order(id, owner, roster.ship, DroneOrder::Engage(target)) { index += 1; }
                }
            }
            true
        });
        drones.clear();
        targets.clear();
        self.drone_aggressors = rosters;
        self.automatic_drones = drones;
        self.automatic_targets = targets;
    }
}

#[cfg(test)] mod tests;
