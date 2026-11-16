//! In-process sleep store for proven-quiescent NPC cohorts. A cleanly stopped
//! actor moves its surviving, explicitly marked belt or combat-site group into
//! the owning `World`; the same system's next cold load installs it before the
//! first scene publication. Payloads are moved wholesale, never cloned,
//! serialized or carried across a restart, and never rebuilt from spawn tables.

use super::*;
use crate::sde::Sde;
use infinity_core::consts::GROUP_ASTEROID_BELT;

/// Which content authority produced a sleepable spawn group. Only origins
/// with a real producer may mark a group; unmarked or unknown groups stay
/// strict unload blockers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NpcSpawnOrigin {
    /// Belt rats spawned by `services::exploration::belt_rats` for this belt.
    Belt(i64),
    /// A combat site's rats, spawned once by
    /// `services::exploration::sites::spawn_site_npcs` for this site.
    Exploration(i64),
}

/// A stopped actor's surviving NPC cohort held by the owning `World` under
/// the system ID until its next cold load. Carries the real objects: balls,
/// brains, combat records, groups and the clean fact ledger, and holds no
/// actor, worker endpoint, observer, timer or strong `World` reference.
pub(in crate::space) struct SleepingNpcs {
    pub(in crate::space) survivors: Vec<Ball>,
    pub(in crate::space) brains: HashMap<i64, NpcBrain>,
    pub(in crate::space) actor_combat: HashMap<i64, combat::ActorCombatRecord>,
    pub(in crate::space) groups: HashMap<u64, npc::SpawnGroup>,
    pub(in crate::space) member_groups: HashMap<i64, HashSet<u64>>,
    pub(in crate::space) combat_facts: Option<combat_facts::CombatFacts>,
    pub(in crate::space) nativefact_clock: i64,
}

/// Whether the marked origin still resolves inside this system's own content
/// authority: a belt must be a real asteroid-belt row of the system SDE map, a
/// site a live combat site holding an in-space roster.
fn origin_in_system(sde: &Sde, system: i64, origin: NpcSpawnOrigin) -> bool {
    match origin {
        NpcSpawnOrigin::Belt(belt) => sde.map.celestial(belt).is_some_and(|row|
            row.solar_system_id == Some(system) && row.group_id == Some(i64::from(GROUP_ASTEROID_BELT))),
        NpcSpawnOrigin::Exploration(site) =>
            crate::services::exploration::sites::npc_sleep_origin_matches(system, site),
    }
}

/// One combat record as admitted for sleep: live phase, no pending or armed
/// character timer, and a structurally valid condition.
fn record_sleepable(record: &combat::ActorCombatRecord) -> bool {
    record.phase == combat::CombatPhase::Live && record.pending.is_none()
        && record.character_timer.is_none() && combat::valid_condition(&record.condition)
}

impl System {
    /// The exact safe NPC cohort, or the first reason it is not. `None` means
    /// every brain, group, record and dynamic ball belongs to one marked,
    /// quiescent belt cohort; the ordinary non-NPC blockers in
    /// `unload_blocked` still apply on top. Read-only and allocation-free.
    pub(in crate::space) fn npc_sleep_blocked(&self) -> Option<&'static str> {
        if self.brains.is_empty() && self.groups.is_empty() && self.member_groups.is_empty() {
            return None;
        }
        if let Some(reason) = self.npc_sleep_internals_blocked() { return Some(reason); }
        if self.actor_combat.len() != self.brains.len()
            || self.actor_combat.iter().any(|(&id, record)| !self.brains.contains_key(&id) || !record_sleepable(record)) {
            return Some("actor_combat");
        }
        for &id in self.brains.keys() {
            let Some(ball) = self.ball(id) else { return Some("npc_ball") };
            if ball.char_id.is_some() || !ball.free || ball.mode != MODE_STOP || ball.cloak != 0
                || ball.follow_id != 0 || ball.launcher != 0 || self.snapshot_positions.contains_key(&id) {
                return Some("npc_ball");
            }
            if ball.vel != [0.0; 3] { return Some("npc_velocity"); }
            if ball.state_at(self.ms) != (ball.pos, ball.vel) { return Some("npc_moving"); }
        }
        if !self.observers.is_empty() { return Some("observers"); }
        if self.presentation.has_activity() { return Some("presentation"); }
        if !self.missile_flights.is_empty() { return Some("missiles"); }
        if collision::stationary_contact(&self.balls, collision::enabled()) { return Some("collision"); }
        if self.combat_facts.as_ref().is_some_and(|facts| facts.has_pending_attempts()) {
            return Some("combat_attempts");
        }
        let Some(sde) = self.sde() else { return Some("unsupported_scene") };
        for group in self.groups.values() {
            let Some(origin) = group.origin else { return Some("npc_group_origin") };
            if !origin_in_system(sde, self.id, origin) { return Some("npc_sleep_origin") }
        }
        None
    }

    /// Move the certified cohort out of a stopped actor. `Err` means the
    /// state contradicts the final stop proof; the caller treats it as a
    /// containment failure, never a partial move. No scene exit, combat
    /// retire, ship-forget or group clear runs here.
    pub(in crate::space) fn take_sleeping_npcs(&mut self) -> Result<Option<SleepingNpcs>, &'static str> {
        if self.brains.is_empty() && self.groups.is_empty() && self.member_groups.is_empty() {
            return Ok(None);
        }
        if let Some(reason) = self.npc_sleep_internals_blocked() { return Err(reason); }
        if self.actor_combat.len() != self.brains.len()
            || self.actor_combat.iter().any(|(&id, record)| !self.brains.contains_key(&id) || !record_sleepable(record)) {
            return Err("actor_combat");
        }
        let mut survivors = Vec::with_capacity(self.brains.len());
        for &id in self.brains.keys() {
            let Some(ball) = self.balls.remove(&id) else { return Err("npc_ball") };
            survivors.push(ball);
        }
        Ok(Some(SleepingNpcs {
            survivors,
            brains: std::mem::take(&mut self.brains),
            actor_combat: std::mem::take(&mut self.actor_combat),
            groups: std::mem::take(&mut self.groups),
            member_groups: std::mem::take(&mut self.member_groups),
            combat_facts: self.combat_facts.take(),
            nativefact_clock: self.nativefact_clock,
        }))
    }

    /// Borrow-side validation of a stored payload before it is consumed;
    /// the world map is never modified here and a refusal keeps the entry.
    pub(in crate::space) fn sleeping_npcs_install_blocked(&self, payload: &SleepingNpcs) -> Option<&'static str> {
        if payload.survivors.is_empty()
            || payload.brains.len() != payload.survivors.len()
            || payload.actor_combat.len() != payload.survivors.len() {
            return Some("npc_sleep_payload");
        }
        if payload.survivors.iter().any(|ball| !payload.brains.contains_key(&ball.id)
            || !payload.actor_combat.contains_key(&ball.id)
            || self.balls.contains_key(&ball.id) || self.rocks.contains_key(&ball.id)) {
            return Some("npc_sleep_conflict");
        }
        if payload.groups.keys().any(|id| self.groups.contains_key(id))
            || (self.combat_facts.is_some() && payload.combat_facts.is_some()) {
            return Some("npc_sleep_conflict");
        }
        if payload.actor_combat.values().any(|record| !record_sleepable(record)) { return Some("npc_sleep_payload"); }
        if let Some(reason) = System::sleeping_npcs_groups_blocked(payload) { return Some(reason); }
        let Some(sde) = self.sde() else { return Some("unsupported_scene") };
        if payload.survivors.iter().any(|ball| sde.types.get(ball.type_id).is_none()) { return Some("npc_sleep_type"); }
        for group in payload.groups.values() {
            let Some(origin) = group.origin else { return Some("npc_group_origin") };
            if !origin_in_system(sde, self.id, origin) { return Some("npc_sleep_origin") }
        }
        None
    }

    /// Install a fully validated payload before the first scene publication:
    /// each survivor's ball is inserted for a fresh scene token while the NPC
    /// and combat maps are still empty, then the original brains, records,
    /// groups, member index and clean fact ledger move back wholesale, and
    /// every restored token receives the canonical NPC no_skin projection.
    /// Combat incarnation, source sequence and ledger counters are untouched.
    pub(in crate::space) fn install_sleeping_npcs(&mut self, payload: SleepingNpcs) {
        let SleepingNpcs { survivors, brains, actor_combat, groups, member_groups,
            combat_facts, nativefact_clock } = payload;
        let mut prepared = Vec::with_capacity(survivors.len());
        for ball in survivors {
            let skin = ship_cosmetics::PreparedSkin::npc(&ball);
            let id = ball.id;
            self.insert(ball);
            prepared.push((id, skin));
        }
        self.brains.extend(brains);
        self.actor_combat.extend(actor_combat);
        self.groups.extend(groups);
        self.member_groups.extend(member_groups);
        if self.combat_facts.is_none() { self.combat_facts = combat_facts; }
        self.nativefact_clock = self.nativefact_clock.max(nativefact_clock);
        for (id, skin) in prepared {
            let token = self.capture_ball(id).expect("inserted ball");
            self.install_skin(token, skin);
        }
    }
}
