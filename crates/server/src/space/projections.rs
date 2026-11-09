//! Actor-owned, transient incoming dogma projections and target refreshes.
//! Client dogma consumes OnEwarStart/End for projected effect kinds; SDE effect
//! rows supply notification classification and modifiers. Persistent free hull
//! refreshes include their fitted items and overrides. Projections do not survive system shutdown.
//! Receiver projections update cached defenses in the actor before asynchronous kinematic refreshes.
//! Incoming remote assistance is suppressed for capital hulls with a paid Entosis link.
//! Effective warp disruption cancels queued or aligning warp through a membership-fenced action.
use super::*;
use crate::services::{effects::{notify, projection::Projection, propulsion, registry, EffectKind}, fit::Fit};
use infinity_core::consts::{dogma::ATTR_MAX_TARGET_RANGE, dogma::ATTR_SCAN_RESOLUTION, dogma::ATTR_WARP_SCRAMBLE_STATUS};
use crate::services::fit::Key;
#[path = "projections/combat.rs"] mod combat_facts_bridge;
impl World {
    /// Capture live persistent hull memberships for cold fitting transactions.
    pub(crate) async fn online_fences(&self, owners: &[i64]) -> Vec<(i64, i64, u64)> {
        let handles: Vec<_> = self.systems.lock().unwrap().iter()
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
    /// Replace a cached fit only for the actor membership and projection revision
    /// that produced it. The actor re-anchors condition state before publishing
    /// new defense scalars; it never restores raw HP from the fit.
    pub(crate) fn replace_fitted_cache(
        &mut self, ship: i64, generation: u64, fit_revision: u64, fit: Fit, key: Key, revision: Option<u64>,
    ) -> bool {
        let Some(record_generation) = self.combat_membership_generation(ship) else { return false };
        if record_generation != generation || !self.contains(ship) { return false; }
        let expected_revision = match key {
            Key::Ship => fit.projection_revision(),
            Key::Drone(id) if id == ship => fit.drone_projection_revision(),
            _ => return false,
        };
        if expected_revision.is_some() && expected_revision != revision { return false; }
        if let Some(revision) = revision {
            let current = self.projection_revision(ship);
            if current != revision { return false; }
        }
        let Some(record) = self.actor_combat.get_mut(&ship) else { return false };
        if record.fit_key != key || record.fit_revision != fit_revision || !matches!(record.phase, combat::CombatPhase::Live) { return false; }
        record.fit = fit;
        record.fit_revision = record.fit_revision.saturating_add(1);
        self.refresh_fitted_stats(ship);
        true
    }
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
    pub(crate) fn upsert_projection(&mut self, target: i64, projection: Projection) {
        if !self.contains(target) || !self.contains(projection.source_ship) { return; }
        let previous = self.take_projection(projection.source_module);
        let changed_identity = previous.as_ref().is_none_or(|(old_target, old)|
            *old_target != target || old.effect_id != projection.effect_id || old.source_ship != projection.source_ship || old.kind != projection.kind);
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
        self.refresh_projection_target(target);
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
        let drone = self.drone(target);
        let incarnation = self.persistent_generation(target);
        let generation = self.combat_membership_generation(target);
        let Some(ball) = self.ball(target) else { return };
        let character = ball.char_id;
        let type_id = ball.type_id;
        let projections = self.projections_on(target);
        if self.replace_fitted_projections(target, projections.clone(), Some(revision))
            && self.actor_combat.get(&target).is_some_and(|record|
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
        let fit_revision = self.actor_combat.get(&target).map(|record| record.fit_revision);
        let Some(sde) = self.projection_sde.clone() else { return };
        let Some(context) = self.snapshot_context.clone() else { return };
        let has_target_range = sde.types.attribute(type_id, ATTR_MAX_TARGET_RANGE).is_some()
            || sde.dogma.attribute(ATTR_MAX_TARGET_RANGE).and_then(|a| a.default_value).is_some_and(|value| value > 0.0);
        if let Some(drone) = drone.as_ref().filter(|d| d.owner != infinity_core::consts::OWNER_UNOWNED) {
            self.defer(refresh_owned_drone(context, sde, drone.clone(), revision, incarnation, generation, fit_revision));
            return;
        }
        self.defer(async move {
            let fit = if let Some(character) = character {
                match Fit::load_projected(&context.db, sde, character, target, projections).await {
                    Ok(fit) => fit,
                    Err(crate::services::CallError::Internal(error)) => { tracing::warn!(%error, target, "projected fit refresh failed"); return; }
                    Err(crate::services::CallError::User { msg, .. }) => { tracing::warn!(%msg, target, "projected fit refresh failed"); return; }
                }
            } else if incarnation.is_some() && drone.is_none()
                && sde.types.get(type_id).and_then(|t| t.category_id) == Some(infinity_core::consts::CATEGORY_SHIP) {
                match Fit::load_unpiloted_projected(&context.db, sde, target, projections).await {
                    Ok(fit) => fit,
                    Err(crate::services::CallError::Internal(error)) => { tracing::warn!(%error, target, "projected fit refresh failed"); return; }
                    Err(crate::services::CallError::User { msg, .. }) => { tracing::warn!(%msg, target, "projected fit refresh failed"); return; }
                }
            } else { Fit::for_space_type(sde, type_id, projections) };
            let values = propulsion::kinematics(&fit, Key::Ship);
            let targeting = (fit.value(Key::Ship, ATTR_SCAN_RESOLUTION),
                has_target_range.then(|| fit.value(Key::Ship, ATTR_MAX_TARGET_RANGE)));
            let drone_spec = if drone.is_some() {
                match crate::services::drones::worker::spec(&fit, Key::Ship, type_id) {
                    Ok(spec) => Some(spec),
                    Err(crate::services::CallError::Internal(error)) => { tracing::warn!(%error, target, "projected drone spec refresh failed"); return; }
                    Err(crate::services::CallError::User { msg, .. }) => { tracing::warn!(%msg, target, "projected drone spec refresh failed"); return; }
                }
            } else { None };
            if let Some(tx) = context.tx.upgrade() {
                let fit_for_cache = fit;
                let _ = tx.send(Box::new(move |system| {
                    if !system.contains(target) || system.projection_revisions.get(&target) != Some(&revision)
                        || system.persistent_generation(target) != incarnation { return; }
                    if let Some(old) = &drone {
                        if !system.drone(target).is_some_and(|d| d.generation == old.generation && d.owner == old.owner) {
                            if system.drone(target).is_some() { system.refresh_projection_target(target); }
                            return;
                        }
                    } else if system.drone(target).is_some() { return; }
                    if let (Some(generation), Some(fit_revision)) = (generation, fit_revision) {
                        if !system.replace_fitted_cache(target, generation, fit_revision, fit_for_cache, Key::Ship, Some(revision)) {
                            if system.combat_membership_generation(target) == Some(generation) && system.combat_phase(target) == Some(combat::CombatPhase::Live)
                                && system.combat_fit_revision(target) != Some(fit_revision) { system.refresh_projection_target(target); }
                            return;
                        }
                    }
                    if let (Some(drone), Some(spec)) = (drone, drone_spec) {
                        system.drone_refresh(target, drone.generation, spec, values, None);
                    } else {
                        system.queue_projection_kinematics(target, revision, values);
                        system.refresh_npc_targeting(target, targeting.0, targeting.1);
                    }
                }));
            }
        });
    }
}

async fn refresh_owned_drone(context: snapshot::Context, sde: Arc<Sde>, drone: drone::DroneRecord, revision: u64, incarnation: Option<u64>, actor_generation: Option<u64>, fit_revision: Option<u64>) {
    let target = drone.id;
    let owner = drone.owner;
    let generation = drone.generation;
    let ship = match crate::dao::characters::selection::ship_id(&context.db, owner).await {
        Ok(Some(ship)) => ship,
        Ok(None) => { tracing::warn!(target, owner, "drone owner has no selected ship"); return; }
        Err(error) => { tracing::warn!(%error, target, "drone owner ship lookup failed"); return; }
    };
    let Some(tx) = context.tx.upgrade() else { return };
    let handle = actor::SystemHandle { tx, gate: context.gate.clone(), written: context.written.clone() };
    let Some(snapshot) = handle.with(move |system| {
        let current = system.drone(target)?;
        if system.persistent_generation(target) != incarnation || system.projection_revision(target) != revision { return None; }
        if current.owner != owner || current.generation != generation {
            system.refresh_projection_target(target);
            return None;
        }
        Some((system.projections_on(ship), system.projection_revision(ship), system.projections_on(target),
            system.combat_membership_generation(ship), system.combat_fit_revision(ship)))
    }).await.flatten() else { return };
    let (ship_projections, ship_revision, drone_projections, ship_generation, ship_fit_revision) = snapshot;
    let fit = match Fit::load_drone(&context.db, sde, owner, ship, target, ship_projections, drone_projections).await {
        Ok(fit) => fit,
        Err(crate::services::CallError::Internal(error)) => { tracing::warn!(%error, target, "projected drone fit refresh failed"); return; }
        Err(crate::services::CallError::User { msg, .. }) => { tracing::warn!(%msg, target, "projected drone fit refresh failed"); return; }
    };
    let selected = match crate::dao::characters::selection::ship_id(&context.db, owner).await {
        Ok(selected) => selected,
        Err(error) => { tracing::warn!(%error, target, "drone owner ship lookup failed"); return; }
    };
    let values = propulsion::kinematics(&fit, Key::Drone(target));
    let spec = match crate::services::drones::worker::spec(&fit, Key::Drone(target), drone.spec.type_id) {
        Ok(spec) => spec,
        Err(crate::services::CallError::Internal(error)) => { tracing::warn!(%error, target, "projected drone spec refresh failed"); return; }
        Err(crate::services::CallError::User { msg, .. }) => { tracing::warn!(%msg, target, "projected drone spec refresh failed"); return; }
    };
    let _ = handle.send(Box::new(move |system| {
        if system.persistent_generation(target) != incarnation || system.projection_revision(target) != revision { return; }
        if !system.drone(target).is_some_and(|d| d.owner == owner && d.generation == generation) {
            if system.drone(target).is_some() { system.refresh_projection_target(target); }
            return;
        }
        if selected != Some(ship) || system.projection_revision(ship) != ship_revision
            || system.combat_membership_generation(ship) != ship_generation || system.combat_fit_revision(ship) != ship_fit_revision {
            system.refresh_projection_target(target);
            return;
        }
        if let (Some(actor_generation), Some(fit_revision)) = (actor_generation, fit_revision) {
            if !system.replace_fitted_cache(target, actor_generation, fit_revision, fit, Key::Drone(target), Some(revision)) {
                if system.combat_membership_generation(target) == Some(actor_generation) && system.combat_phase(target) == Some(combat::CombatPhase::Live)
                    && system.combat_fit_revision(target) != Some(fit_revision) { system.refresh_projection_target(target); }
                return;
            }
        }
        system.drone_refresh(target, generation, spec, values, Some((ship, ship_revision)));
    }));
}

#[cfg(test)] mod tests;
