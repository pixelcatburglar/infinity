//! Actor-owned, transient incoming dogma projections and target refreshes.
//! Client dogma consumes OnEwarStart/End for projected effect kinds; SDE effect
//! rows supply notification classification and modifiers. Persistent free hull
//! refreshes include their fitted items and overrides. Projections do not survive system shutdown.
//! Receiver projections update the resident combat fit in place; defenses, motion,
//! signature, NPC targeting and drone specs rebase synchronously inside the actor.
//! A resident ship or drone's refreshed attribute rows publish through the
//! shared fit receipt and the single fit-delta emitter.
use super::*;
use crate::services::{effects::{notify, projection::Projection, propulsion, registry, EffectKind}, fit::Fit};
use infinity_core::consts::{dogma::ATTR_MAX_TARGET_RANGE, dogma::ATTR_SCAN_RESOLUTION, dogma::ATTR_WARP_SCRAMBLE_STATUS};
use crate::services::fit::Key;
use super::combat::inputs::FitDeltaSources;
#[path = "projections/combat.rs"] mod combat_facts_bridge;
impl World {
    /// Capture live persistent hull memberships for cold fitting transactions.
    pub(crate) async fn online_fences(&self, owners: &[i64]) -> Vec<(i64, i64, u64)> {
        let handles: Vec<_> = crate::poison::lock_or_exit(self.systems.lock()).iter()
            .filter(|(_, handle)| !handle.is_closed()).map(|(&id, handle)| (id, handle.clone())).collect();
        let owners: Arc<[i64]> = Arc::from(owners);
        let mut fences = Vec::new();
        for (system, handle) in handles {
            let owners = owners.clone();
            if let Some(rows) = handle.with(move |s| {
                s.persistent_balls().filter(|ball| owners.contains(&ball.owner_id))
                    .filter_map(|ball| s.combat_membership_generation(ball.id).map(|generation| (system, ball.id, generation)))
                    .collect::<Vec<_>>()
            }).await {
                fences.extend(rows);
            }
        }
        fences
    }
}



impl System {
    /// Snapshot the current incoming sources for one target.
    pub(crate) fn projections_on(&self, target: i64) -> Vec<Projection> {
        let blocked = crate::space::sovereignty::links::assistance_blocked(self, target);
        self.projections.get(&target).into_iter().flatten().filter(|projection| {
            !blocked || projection.source_ship == target || !self.projection_sde.as_ref()
                .and_then(|sde| sde.dogma.effect(projection.effect_id))
                .is_some_and(|effect| effect.is_assistance)
        }).cloned().collect()
    }

    /// Generation of the target's current incoming projection snapshot.
    pub(crate) fn projection_revision(&self, target: i64) -> u64 {
        self.projection_revisions.get(&target).copied().unwrap_or(0)
    }

    /// Replace the snapshot for one source module, moving it between targets if needed.
    ///
    /// A renewal of the same source, effect and operands only extends the
    /// stored deadline: the effective inputs are unchanged, so the resident
    /// fit and outputs are not rebuilt.
    pub(crate) fn upsert_projection(&mut self, target: i64, projection: Projection) {
        if !self.contains(target) || !self.contains(projection.source_ship) { return; }
        let previous = self.take_projection(projection.source_module);
        let changed_identity = previous.as_ref().is_none_or(|(old_target, old)|
            *old_target != target || old.effect_id != projection.effect_id || old.source_ship != projection.source_ship || old.kind != projection.kind);
        let renew_only = previous.as_ref().is_some_and(|(old_target, old)|
            !changed_identity && *old_target == target && old.source_type == projection.source_type
            && old.values == projection.values && old.charge_values == projection.charge_values
            && old.charge_type == projection.charge_type && old.scramble == projection.scramble);
        if let Some((old_target, old)) = previous {
            if old.scramble != projection.scramble {
                self.close_projection_scramble(old.scramble);
            }
            if changed_identity {
                self.projection_notice(old_target, &old, false);
                if old_target != target { self.refresh_projection_target(old_target); }
            }
        }
        if changed_identity { self.projection_notice(target, &projection, true); }
        self.projection_targets.insert(projection.source_module, target);
        let entries = self.projections.entry(target).or_default();
        let index = entries.binary_search_by_key(&projection.source_module, |p| p.source_module).unwrap_or_else(|i| i);
        entries.insert(index, projection);
        if !renew_only {
            self.refresh_projection_target(target);
        } else if let Some((expired, _, receipt)) = self.prepare_fit_publication(target, None) {
            // Renewal itself is not dirty, but a real character deadline may
            // have elapsed while its timer was unrun; only that rewrites the
            // resident fit and its outputs.
            if expired { self.rebase_fitted_outputs(target); }
            if let Some(before) = receipt { self.finish_fit_delta(before, None); }
        }
    }

    fn take_projection(&mut self, source_module: i64) -> Option<(i64, Projection)> {
        let target = self.projection_targets.remove(&source_module)?;
        let entries = self.projections.get_mut(&target).expect("projection target index");
        let index = entries.binary_search_by_key(&source_module, |p| p.source_module).expect("projection module index");
        let projection = entries.remove(index);
        if entries.is_empty() { self.projections.remove(&target); }
        Some((target, projection))
    }

    /// Remove a module's projection, if present.
    pub(crate) fn remove_projection(&mut self, source_module: i64) {
        if let Some((target, projection)) = self.take_projection(source_module) {
            self.close_projection_scramble(projection.scramble);
            self.projection_notice(target, &projection, false);
            self.refresh_projection_target(target);
        }
    }

    /// Remove outgoing projections of a source against a lost target.
    pub(crate) fn remove_projections_between(&mut self, source_ship: i64, target: i64) {
        let modules: Vec<_> = self.projections.get(&target).into_iter().flatten()
            .filter_map(|p| (p.source_ship == source_ship).then_some(p.source_module)).collect();
        for id in modules { self.remove_projection(id); }
    }

    /// Remove all outgoing projections of a departing source.
    pub(crate) fn remove_projections_from(&mut self, source_ship: i64) {
        let modules: Vec<_> = self.projections.values().flatten()
            .filter_map(|p| (p.source_ship == source_ship).then_some(p.source_module)).collect();
        for id in modules { self.remove_projection(id); }
    }

    pub(super) fn remove_projections_on_departure(&mut self, target: i64) {
        let modules: Vec<_> = self.projections.get(&target).into_iter().flatten().map(|p| p.source_module).collect();
        for id in modules { self.remove_projection(id); }
        let revision = self.projection_revisions.entry(target).or_default();
        *revision = revision.checked_add(1).expect("projection revision exhausted");
    }

    pub(super) fn expire_projections(&mut self) {
        let modules: Vec<_> = self.projections.values().flatten()
            .filter_map(|p| (p.until_ms <= self.ms).then_some(p.source_module)).collect();
        for id in modules { self.remove_projection(id); }
    }

    fn projection_notice(&self, target: i64, projection: &Projection, start: bool) {
        if !matches!(projection.kind, EffectKind::Projected { .. }) { return; }
        let Some(character) = self.ball(target).and_then(|ball| ball.char_id) else { return };
        let Some(effect) = self.projection_sde.as_ref().and_then(|sde| registry::resolve(sde, projection.effect_id)) else { return };
        if let Some(slot) = notify::ewar_slot(&effect) {
            let (event, args) = notify::ewar(projection.source_ship, projection.source_module, target, slot, start);
            self.notify_pilot(character, event, args);
        }
    }

    pub(crate) fn refresh_projection_target(&mut self, target: i64) {
        let revision = self.projection_revisions.entry(target).or_default();
        *revision = revision.checked_add(1).expect("projection revision exhausted");
        let revision = *revision;
        let generation = self.combat_membership_generation(target);
        let Some(ball) = self.ball(target) else { return };
        let type_id = ball.type_id;
        let projections = self.projections_on(target);
        if self.actor_combat.contains_key(&target) {
            // The shared expiry gate runs first, then the projection write;
            // both merge into one output rebase and the scramble check reads
            // the effective post-expiry fit.
            let prepared = self.prepare_fit_publication(target, Some(FitDeltaSources::Projections(&projections)));
            let replaced = prepared.is_some()
                && self.set_fitted_projections(target, projections.clone(), Some(revision));
            if matches!(prepared, Some((true, _, _))) || replaced {
                self.rebase_fitted_outputs(target);
            }
            if let Some((_, _, Some(before))) = prepared { self.finish_fit_delta(before, None); }
            if replaced && self.actor_combat.get(&target).is_some_and(|record|
                record.fit.value(record.fit_key, ATTR_WARP_SCRAMBLE_STATUS) > 0.0)
            {
                if let Some(generation) = generation {
                    self.command(target, move |ball, stamp, system| {
                        if system.combat_membership_generation(target) == Some(generation)
                            && ball.mode == MODE_WARP && ball.effect_stamp < 0
                        {
                            ball.stop();
                            vec![event(stamp, "Stop", vec![Value::Int(target)])]
                        } else {
                            Vec::new()
                        }
                    });
                }
            }
        } else {
            // No resident fit: the type-only snapshot keeps NPC and free-ball
            // outputs current without loading anything.
            let Some(sde) = self.projection_sde.clone() else { return };
            let fit = Fit::for_space_type(sde.clone(), type_id, projections.clone());
            let values = propulsion::kinematics(&fit, Key::Ship);
            let has_target_range = sde.types.attribute(type_id, ATTR_MAX_TARGET_RANGE).is_some()
                || sde.dogma.attribute(ATTR_MAX_TARGET_RANGE).and_then(|a| a.default_value).is_some_and(|value| value > 0.0);
            let targeting = (fit.value(Key::Ship, ATTR_SCAN_RESOLUTION),
                has_target_range.then(|| fit.value(Key::Ship, ATTR_MAX_TARGET_RANGE)));
            if let Some(drone) = self.drone(target) {
                match crate::services::drones::worker::spec(&fit, Key::Drone(target), type_id) {
                    Ok(spec) => { self.drone_refresh(target, drone.generation, spec, values, None); }
                    Err(crate::services::CallError::Internal(error)) => tracing::warn!(%error, target, "projected drone spec refresh failed"),
                    Err(crate::services::CallError::User { msg, .. }) => tracing::warn!(%msg, target, "projected drone spec refresh failed"),
                }
            } else {
                self.queue_projection_kinematics(target, revision, values);
                self.refresh_npc_targeting(target, targeting.0, targeting.1);
            }
        }
        // The owner's ship snapshot also lives in each controlled drone's Ship
        // domain; the copy gates on it before the domain write and finishes once.
        for drone in self.drones().filter(|drone| drone.controller == Some(target)).map(|drone| drone.id).collect::<Vec<_>>() {
            let Some((expired, _, receipt)) = self.prepare_fit_publication(drone, Some(FitDeltaSources::Projections(&projections))) else { continue };
            let changed = self.actor_combat.get_mut(&drone).is_some_and(|record| {
                record.fit.replace_projections_for(Key::Ship, projections.clone(), Some(revision));
                record.fit_revision = record.fit_revision.saturating_add(1);
                true
            });
            if expired || changed { self.rebase_fitted_outputs(drone); }
            if let Some(before) = receipt { self.finish_fit_delta(before, None); }
        }
    }
}

#[cfg(all(test, feature = "tests-space"))] mod tests;
