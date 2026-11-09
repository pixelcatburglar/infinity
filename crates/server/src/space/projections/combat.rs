//! Combat-fact lifetime attached to the physical projected effect.
//! Dogma projection removal remains authoritative; ledger boundaries may end a
//! projection when a continuation cannot be reserved. No separate timer is used.
use super::*;
use crate::space::combat_facts::{CombatSource, Refusal, scramble::ScrambleHandle};

impl System {
    pub(crate) fn combat_character_scope(&self, character: i64) -> Option<crate::space::combat_facts::boundaries::Boundary> {
        self.combat_facts.as_ref()?.export_character_scope(character)
    }

    pub(crate) fn import_combat_character_scope(&mut self, character: i64, receipt: crate::space::combat_facts::boundaries::Boundary)
        -> Result<crate::space::combat_facts::boundaries::Boundary, Refusal> {
        self.nativefact_clock = self.nativefact_clock.max(receipt.at);
        self.ensure_combat_facts();
        self.close_combat_character_boundary(character)?;
        self.combat_facts.as_mut().expect("combat ledger initialized").import_character_scope(character, receipt)
    }
}

impl System {
    pub(super) fn close_projection_scramble(&mut self, handle: Option<ScrambleHandle>) {
        let Some(handle) = handle else { return };
        let at = self.combat_fact_time();
        if let Some(facts) = self.combat_facts.as_mut() {
            if let Err(error) = facts.close_scramble(handle, at) {
                tracing::error!(?error, "closing projection scramble failed");
            }
        }
    }

    fn remove_closed_scrambles(&mut self, handles: Vec<ScrambleHandle>) {
        if handles.is_empty() { return; }
        let modules: Vec<_> = self.projections.values().flatten()
            .filter_map(|p| p.scramble.filter(|handle| handles.contains(handle)).map(|_| p.source_module)).collect();
        for module in modules {
            if let Some((target, mut projection)) = self.take_projection(module) {
                projection.scramble = None;
                self.projection_notice(target, &projection, false);
                self.refresh_projection_target(target);
            }
        }
    }

    pub(crate) fn observe_combat_utc(&mut self) -> Result<(), Refusal> {
        let at = self.combat_fact_time();
        let Some(facts) = self.combat_facts.as_mut() else { return Ok(()) };
        let handles = facts.observe_utc(at)?;
        self.remove_closed_scrambles(handles);
        Ok(())
    }

    pub(crate) fn checkpoint_combat_scrambles(&mut self) -> Result<(), Refusal> {
        if self.combat_facts.is_none() { return Ok(()); }
        self.observe_combat_utc()?;
        let at = self.combat_observed_time();
        let handles = self.combat_facts.as_mut().unwrap().checkpoint_scrambles(at)?;
        self.remove_closed_scrambles(handles);
        self.prune_combat_character_scopes();
        Ok(())
    }

    pub(crate) fn close_combat_character_boundary(&mut self, character: i64) -> Result<(), Refusal> {
        if self.combat_facts.is_none() { return Ok(()); }
        let at = self.combat_fact_time();
        let (_, handles) = self.combat_facts.as_mut().unwrap().close_character_boundary(character, at)?;
        self.remove_closed_scrambles(handles);
        self.prune_combat_character_scopes();
        Ok(())
    }
    pub(crate) fn rebind_combat_character(&mut self, character: i64, corp: Option<i64>, alliance: Option<i64>) {
        let at = self.combat_observed_time();
        let handles: Vec<_> = self.projections.values().flatten().filter_map(|p| p.scramble).collect();
        for handle in handles {
            let Some((mut source, mut target)) = self.combat_facts.as_ref().and_then(|facts| facts.scramble_identity(handle)) else { continue };
            if source.actual_actor_character != Some(character) && target.character_id != Some(character) { continue; }
            if source.entity.character_id == Some(character) {
                source.entity.corporation_id = corp;
                source.entity.alliance_id = alliance;
            }
            if source.actual_actor_character == Some(character) {
                source.context.corporation_id = corp;
                source.context.alliance_id = alliance;
            }
            if target.character_id == Some(character) {
                target.corporation_id = corp;
                target.alliance_id = alliance;
            }
            self.combat_facts.as_mut().expect("active scramble ledger")
                .rebind_scramble(handle, source, target, at).expect("combat affiliation continuation invariant");
        }
    }


    pub(crate) fn accept_projection_scramble(&mut self, target: i64, projection: &Projection, source: CombatSource) -> Result<ScrambleHandle, Refusal> {
        self.observe_combat_utc()?;
        let entity = self.combat_entity(target).ok_or(Refusal::Invalid)?;
        let previous = self.projection_targets.get(&projection.source_module).and_then(|old_target| {
            self.projections.get(old_target)?.iter().find(|old| old.source_module == projection.source_module)
                .and_then(|old| old.scramble.map(|handle| (*old_target, old.effect_id, old.source_ship, old.kind, handle)))
        });
        let previous = if let Some((old_target, effect, ship, kind, handle)) = previous {
            if old_target == target && effect == projection.effect_id && ship == projection.source_ship && kind == projection.kind
                && self.combat_facts.as_ref().is_some_and(|facts| facts.scramble_matches(handle, source, entity)) {
                Some(handle)
            } else {
                self.remove_projection(projection.source_module);
                None
            }
        } else { None };
        if previous.is_none() { self.observe_combat_utc()?; }
        let at = self.combat_observed_time();
        let result = self.combat_facts.as_mut().ok_or(Refusal::Invalid)?.accept_scramble(source, entity, previous, at);
        if matches!(result, Err(Refusal::Full)) && previous.is_some() {
            self.remove_projection(projection.source_module);
        }

        result
    }
}

#[cfg(test)] mod tests;
