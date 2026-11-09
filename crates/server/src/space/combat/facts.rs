//! Native admission of immutable combat source identities and replay attempts.
//! No client protocol, database, or persistent identity store is involved.
use super::System;
use crate::lua::events::combat::CombatEntity;
use crate::lua::events::resources::{FactContext, FactOrigin};
use crate::space::combat_facts::{CombatFacts, CombatSource, Refusal};

fn incarnation(epoch: u64, generation: u64) -> [u8; 16] {
    let mut value = [0; 16];
    value[..8].copy_from_slice(&epoch.to_be_bytes());
    value[8..].copy_from_slice(&generation.to_be_bytes());
    value
}

impl System {
    pub(crate) fn combat_fact_time(&mut self) -> i64 {
        self.nativefact_clock = self.nativefact_clock.max(crate::client::filetime_now());
        self.nativefact_clock
    }
    pub(crate) fn combat_observed_time(&self) -> i64 { self.nativefact_clock }


    pub(crate) fn prepare_body_combat_source(&mut self, source_id: i64, module_id: Option<i64>,
        effect_id: Option<i64>, weapon_type_id: Option<i32>, origin: FactOrigin,
        guard: impl FnOnce(&System) -> bool) -> Result<CombatSource, Refusal> {
        if !self.is_npc(source_id) && self.drone(source_id).is_none() { return Err(Refusal::Invalid); }
        let record = self.actor_combat.get(&source_id).ok_or(Refusal::Invalid)?;
        let generation = record.membership_generation;
        let sequence = record.source_sequence.checked_add(1).ok_or(Refusal::Overflow)?;
        let source = self.prepare_combat_source(source_id, module_id, effect_id, weapon_type_id,
            origin, generation, sequence, 1, guard)?;
        self.actor_combat.get_mut(&source_id).expect("native source retained in actor turn").source_sequence = sequence;
        Ok(source)
    }

    pub(crate) fn ensure_combat_facts(&mut self) {
        let config = &crate::config::game().combat;
        let system_id = self.id;
        self.combat_facts.get_or_insert_with(|| CombatFacts::new(crate::handshake::random_u64(), system_id,
            config.progress_key_capacity, config.progress_pending_batches));
    }

    pub(crate) fn combat_entity(&self, item: i64) -> Option<CombatEntity> {
        let ball = self.ball(item)?;
        if ball.id <= 0 || ball.type_id <= 0 { return None; }
        let generation = self.combat_membership_generation(item)?;
        Some(CombatEntity { item_id: ball.id, type_id: ball.type_id, incarnation: incarnation(self.combat_facts.as_ref()?.runtime_epoch(), generation),
            character_id: ball.char_id.filter(|id| *id > 0),
            corporation_id: (ball.corp_id > 0).then_some(ball.corp_id),
            alliance_id: (ball.alliance_id > 0).then_some(ball.alliance_id), faction_id: None })
    }

    pub(crate) fn prepare_combat_source(&mut self, source_id: i64, module_id: Option<i64>,
        effect_id: Option<i64>, weapon_type_id: Option<i32>, origin: FactOrigin,
        generation: u64, sequence: u64, first_sequence: u64,
        guard: impl FnOnce(&System) -> bool) -> Result<CombatSource, Refusal> {
        if !guard(self) || module_id.is_some_and(|id| id <= 0) || effect_id.is_some_and(|id| id <= 0)
            || weapon_type_id.is_some_and(|id| id <= 0) { return Err(Refusal::Invalid); }
        if self.combat_phase(source_id) != Some(super::CombatPhase::Live) { return Err(Refusal::Invalid); }
        self.observe_combat_utc()?;
        self.ensure_combat_facts();
        let entity = self.combat_entity(source_id).ok_or(Refusal::Invalid)?;
        let controller = if let Some(drone) = self.drone(source_id) {
            drone.controller_owner.and_then(|owner| self.pilot_ball(owner))
                .filter(|ball| Some(ball.id) == drone.controller && ball.char_id == drone.controller_owner)
        } else {
            self.ball(source_id).and_then(|ball| ball.char_id)
                .and_then(|pilot| self.pilot_ball(pilot)).filter(|ball| ball.id == source_id)
        };
        if (self.drone(source_id).is_some() || entity.character_id.is_some()) && controller.is_none() {
            return Err(Refusal::Invalid);
        }
        let actor = controller.and_then(|ball| ball.char_id).filter(|id| *id > 0);
        let context = FactContext { corporation_id: controller.and_then(|b| (b.corp_id > 0).then_some(b.corp_id)),
            alliance_id: controller.and_then(|b| (b.alliance_id > 0).then_some(b.alliance_id)), faction_id: None,
            ship_id: controller.map(|b| b.id), ship_type_id: controller.map(|b| b.type_id),
            system_id: (self.id > 0).then_some(self.id), station_id: None, origin };
        let controller_incarnation = match controller {
            Some(ball) => Some(self.combat_entity(ball.id).ok_or(Refusal::Invalid)?.incarnation),
            None => None,
        };
        let source_is_npc = self.is_npc(source_id) && self.drone(source_id).is_none();
        let source = CombatSource { actual_actor_character: actor, entity, context, source_is_npc, controller_incarnation, module_id, effect_id,
            weapon_type_id, activation: incarnation(self.combat_facts.as_ref().expect("ledger initialized").runtime_epoch(), generation), sequence };
        let facts = self.combat_facts.as_mut().expect("ledger initialized");
        facts.register_source(source.activation, generation, first_sequence)?;
        facts.reserve_attempt(&source)?;
        Ok(source)
    }

    pub(crate) fn finish_combat_attempt(&mut self, source: &CombatSource) {
        if let Some(facts) = self.combat_facts.as_mut() { facts.finish_attempt(source); }
    }

    pub(crate) fn retire_combat_source(&mut self, generation: u64) -> Result<(), Refusal> {
        let facts = self.combat_facts.as_mut().ok_or(Refusal::Invalid)?;
        let activation = incarnation(facts.runtime_epoch(), generation);
        facts.retire_source(activation, generation)
    }

    pub(crate) fn npc_death_context(&self, item: i64, token: u64, generation: u64)
        -> Option<std::sync::Arc<crate::services::combat::groups_health::death::NpcDeathContext>> {
        let record = self.actor_combat.get(&item)?;
        if record.membership_generation != generation || record.phase != (super::CombatPhase::Dying { token }) { return None; }
        match record.pending.as_ref()? {
            super::PendingCombat::Dying { token: stored, generation: current, npc_context, .. }
                if *stored == token && *current == generation => npc_context.clone(),
            _ => None,
        }
    }

    pub(crate) fn install_npc_death_context(&mut self, item: i64, token: u64, generation: u64,
        context: std::sync::Arc<crate::services::combat::groups_health::death::NpcDeathContext>) -> bool {
        let Some(record) = self.actor_combat.get_mut(&item) else { return false };
        if record.membership_generation != generation || record.phase != (super::CombatPhase::Dying { token }) { return false; }
        match record.pending.as_mut() {
            Some(super::PendingCombat::Dying { token: stored, generation: current, npc_context, .. })
                if *stored == token && *current == generation && npc_context.is_none() => {
                    *npc_context = Some(context);
                    true
                }
            _ => false,
        }
    }

    pub(crate) fn player_loss_context(&self, item: i64, token: u64, generation: u64)
        -> Option<std::sync::Arc<crate::services::ship::loss::facts::PlayerLossContext>> {
        let record = self.actor_combat.get(&item)?;
        if record.membership_generation != generation || record.phase != (super::CombatPhase::Dying { token }) { return None; }
        match record.pending.as_ref()? {
            super::PendingCombat::Dying { token: stored, generation: current, player_context, .. }
                if *stored == token && *current == generation => player_context.clone(),
            _ => None,
        }
    }

    pub(crate) fn install_player_loss_context(&mut self, item: i64, token: u64, generation: u64,
        context: std::sync::Arc<crate::services::ship::loss::facts::PlayerLossContext>) -> bool {
        let Some(record) = self.actor_combat.get_mut(&item) else { return false };
        if record.membership_generation != generation || record.phase != (super::CombatPhase::Dying { token }) { return false; }
        match record.pending.as_mut() {
            Some(super::PendingCombat::Dying { token: stored, generation: current, player_context, .. })
                if *stored == token && *current == generation && player_context.is_none() => {
                    *player_context = Some(context);
                    true
                }
            _ => false,
        }
    }
}
