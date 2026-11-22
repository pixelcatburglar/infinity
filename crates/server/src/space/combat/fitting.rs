//! Committed fitting and active-flag attribute publication, plus guarded live ship repair in the owning actor.
//! Dogma values use Fit's evaluator; durable writes and transaction dispatch are external.
//! A committed capacity mode change republishes the pilot's brain after the patch.
//! Capacitor and heat remain in memory; condition uses the existing checkpoint path.
//! Patches carry per-node changes for ship, module, charge and drone keys, and a
//! cold charge insertion receives a fresh incarnation for final-consumption
//! matching. `publish_drone_fitting` admits only override-only deltas confined
//! to the drone's own node: the owner, never the operating GM or delegate, is
//! authenticated and notified. Every other live fit-attribute change (a
//! committed patch, a module flag or overload write, a character, projection
//! or owner-package replacement, a runtime modifier source change, a
//! propagated ship input, a consumed character deadline) publishes through
//! one receipt per call: `begin_fit_delta` snapshots before the shared
//! expiry gate and
//! `finish_fit_delta` compares the final fit and emits through
//! `publish_fit_delta`, which dedups (owner, item, attribute). A ship
//! publication carries only Ship and Module rows; a resident drone's own
//! receipt publishes only its own `Key::Drone` rows to the real owner, never
//! the copied owner ship domain. A committed stance-switch receipt emits the
//! owner's `OnStanceActive` and the scene `shipStance` slim update once; a
//! redelivered `new_item` already in the resident fit emits neither.
use super::inputs::{FitDeltaBefore, FitDeltaSources};
use super::{CombatPhase, System};
use infinity_core::consts::dogma::{ATTR_ARMOR_DAMAGE, ATTR_ARMOR_HP, ATTR_CAPACITOR_CAPACITY,
    ATTR_CHARGE, ATTR_HULL_DAMAGE, ATTR_RECHARGE_RATE, ATTR_SHIELD_CAPACITY,
    ATTR_SHIELD_CHARGE, ATTR_SHIELD_RECHARGE_RATE, ATTR_STRUCTURE_HP};
use infinity_core::consts::{OWNER_UNOWNED, TYPE_CHARACTER};
use crate::services::combat::groups_health::{fill_capacitor, rebase_capacitor};
use crate::services::effects::{heat::{self, FittingHeatCleanup}, notify::attribute_change,
    projection::Projection};
use crate::services::fit::{attributes::CommittedFitting, character::CharacterInputs, Key};
use crate::services::fit::reactive_armor::ARMOR_RESONANCE;
use crate::sde::Sde;
use marshal::Value;
use infinity_core::session::OnlineSession;
use std::collections::{BTreeSet, HashSet};
use std::sync::Arc;
use tokio::sync::Mutex;

/// Before/after scalars a delta publication pairs against the changed rows of
/// the receiving key; `charge` is the rebased capacitor pair when capacity or
/// its recharge rate moved.
pub(in crate::space) struct FittingDelta {
    pub old_condition: (f64, f64, f64),
    pub new_condition: (f64, f64, f64),
    pub old_cap: f64,
    pub new_cap: f64,
    pub old_shield_max: f64,
    pub new_shield_max: f64,
    pub old_shield_tau: f64,
    pub new_shield_tau: f64,
    pub charge: Option<(f64, f64)>,
}

/// Seed every `modified` attribute of the effects one type carries, the same
/// structural enumeration `fitting_candidates` applies to source nodes.
fn effect_seed_attrs(sde: &Sde, type_id: i32, seeds: &mut BTreeSet<i32>) {
    for entry in sde.types.effects(type_id) {
        if let Some(effect) = sde.dogma.effect(entry.effect_id) {
            seeds.extend(effect.modifiers().iter().map(|modifier| modifier.modified));
        }
    }
}

/// Seed the target attributes a character-input package's sources can modify:
/// trained skill types, brain-item types, the skill grants of installed
/// expert systems, and the character type itself when attribute rows are
/// present. Which modifiers actually run stays the evaluator's call; the
/// conservative set only needs to cover every plausible target.
fn character_seed_attrs(sde: &Sde, inputs: &CharacterInputs, seeds: &mut BTreeSet<i32>) {
    for type_id in inputs.trained.iter().map(|skill| skill.type_id)
        .chain(inputs.brain.iter().map(|brain| brain.item.type_id))
        .chain(inputs.installations.iter()
            .filter_map(|installation| sde.expert_systems.get(installation.type_id))
            .flat_map(|definition| definition.skills.iter().map(|&(skill, _)| skill)))
        .chain(inputs.attributes.is_some().then_some(TYPE_CHARACTER))
    {
        effect_seed_attrs(sde, type_id, seeds);
    }
}

/// Seed the target attributes of explicit incoming projections: each source
/// type and charge type's own effects plus the declared projected effect.
fn projection_seed_attrs(sde: &Sde, projections: &[Projection], seeds: &mut BTreeSet<i32>) {
    for projection in projections {
        effect_seed_attrs(sde, projection.source_type, seeds);
        if let Some(charge) = projection.charge_type { effect_seed_attrs(sde, charge, seeds); }
        if let Some(effect) = sde.dogma.effect(projection.effect_id) {
            seeds.extend(effect.modifiers().iter().map(|modifier| modifier.modified));
        }
    }
}

/// Seed the target attributes of resolved runtime source changes: every
/// `modified` attribute of each named dbuff collection.
fn runtime_seed_attrs(sde: &Sde, collections: impl Iterator<Item = i32>, seeds: &mut BTreeSet<i32>) {
    for id in collections {
        if let Some(collection) = sde.dbuffs.get(id) {
            seeds.extend(collection.modifiers.iter().map(|m| m.modifier.modified));
        }
    }
}

impl System {
    fn space_heal_target(&self, expected: &OnlineSession, generation: u64) -> bool {
        let Some(ship) = expected.ship_id else { return false };
        let owner = expected.character_id;
        expected.station_id.is_none() && expected.system_id == Some(self.id)
            && self.registry.online_session(owner).as_ref() == Some(expected)
            && self.persistent_generation(ship) == Some(generation)
            && self.pilot_ball(owner).is_some_and(|ball| ball.id == ship
                && ball.char_id == Some(owner) && ball.owner_id == owner)
            && self.actor_combat.get(&ship).is_some_and(|record|
                record.membership_generation == generation && record.phase == CombatPhase::Live
                && super::valid_condition(&record.condition) && record.revision.checked_add(2).is_some())
    }

    /// Capture the live target and the identity of its existing actor.
    pub(crate) fn capture_space_heal(&self, expected: &OnlineSession) -> Option<(u64, Arc<Mutex<()>>)> {
        let generation = self.persistent_generation(expected.ship_id?)?;
        if !self.space_heal_target(expected, generation) { return None; }
        Some((generation, Arc::clone(&self.snapshot_context.as_ref()?.gate)))
    }

    /// Publish a committed module repair and restore resources on the captured actor only.
    pub(crate) fn publish_space_heal(&mut self, expected: OnlineSession, generation: u64,
        actor_gate: Arc<Mutex<()>>, patch: CommittedFitting, mut module_changes: Vec<Value>) -> bool {
        if !self.snapshot_context.as_ref().is_some_and(|context| Arc::ptr_eq(&context.gate, &actor_gate))
            || !self.space_heal_target(&expected, generation) { return false; }
        let ship = expected.ship_id.unwrap();
        let owner = expected.character_id;
        if !self.publish_fitting(ship, owner, generation, patch, Vec::new()) { return false; }
        let before = self.ship_condition(ship).unwrap();
        self.set_condition_fraction(ship, 1.0);
        let record = &self.actor_combat[&ship];
        let max = record.condition.stats.max;
        if record.condition.now != max { return false; }
        let capacity = record.fit.value(Key::Ship, ATTR_CAPACITOR_CAPACITY);
        let tau = record.fit.value(Key::Ship, ATTR_RECHARGE_RATE);
        let (old_cap, new_cap) = fill_capacitor(ship, capacity, tau, crate::client::filetime_now());
        let at = self.now_ms();
        heat::reset_ship_heat(self, ship, at);
        for (attr, old, new) in [
            (ATTR_SHIELD_CHARGE, before.0, max[0]),
            (ATTR_ARMOR_DAMAGE, before.1, 0.0),
            (ATTR_HULL_DAMAGE, before.2, 0.0),
            (ATTR_CHARGE, old_cap, new_cap),
        ] {
            module_changes.push(attribute_change(owner, Value::Int(ship), attr, Value::Float(new), Value::Float(old)));
        }
        self.notify_pilot(owner, "OnModuleAttributeChanges", vec![Value::list(module_changes)]);
        self.set_health(ship, max.map(|value| if value > 0.0 { 1.0 } else { 0.0 }));
        true
    }

    /// Clear overload without rebasing until the entire committed patch has landed.
    pub(crate) fn clear_fitting_overload(&mut self, ship: i64, module: i64) {
        if let Some(record) = self.actor_combat.get_mut(&ship) {
            record.fit.set_module_overloaded(module, false);
        }
    }

    /// Begin one publication before any expiry or mutation touches the
    /// resident fit. Admission re-checks the live record's receiving key: a
    /// ship's `Key::Ship` or a resident drone's own `Key::Drone(item)`. The
    /// snapshot captures the pre-expiry condition and scalars of that key;
    /// the old wire values exist only while the real owner can receive rows:
    /// the online pilot of this ship, or the drone's online owner (never a
    /// delegated controller). Their original segment is the surviving
    /// generic candidates kept whole so a removed modifier's recovery value
    /// is never lost; `Fitting` sources extend them with the patch
    /// destinations, remapped onto the receiving key for a drone. The
    /// incoming-only tail pairs the declared sources' seed attributes with
    /// the receiving keys - the ship and its modules, or the drone's own key
    /// alone - and adds the retained character rows when a deadline is due
    /// at `at`. `finish_fit_delta` completes it.
    pub(in crate::space) fn begin_fit_delta(&self, item: i64, at: i64, sources: FitDeltaSources<'_>) -> Option<FitDeltaBefore> {
        let record = self.actor_combat.get(&item)?;
        if !matches!(record.phase, CombatPhase::Live) { return None; }
        let fit_key = record.fit_key;
        let (drone_generation, owner) = match fit_key {
            Key::Ship => (None, self.ball(item).and_then(|ball| ball.char_id)
                .filter(|&owner| self.registry.is_online(owner)
                    && self.pilot_ball(owner).is_some_and(|ball| ball.id == item))),
            Key::Drone(id) if id == item => {
                let drone = self.drone(item);
                (drone.as_ref().map(|drone| drone.generation), drone.map(|drone| drone.owner)
                    .filter(|&owner| owner != OWNER_UNOWNED
                        && self.ball(item).is_some_and(|ball| ball.owner_id == owner)
                        && self.registry.is_online(owner)))
            }
            _ => return None,
        };
        let membership = record.membership_generation;
        let old_condition = self.ship_condition(item)?;
        let old_cap = record.fit.value(fit_key, ATTR_CAPACITOR_CAPACITY);
        let old_tau = record.fit.value(fit_key, ATTR_RECHARGE_RATE);
        let old_shield_max = record.fit.value(fit_key, ATTR_SHIELD_CAPACITY);
        let old_shield_tau = record.fit.value(fit_key, ATTR_SHIELD_RECHARGE_RATE);
        let mut before = Vec::new();
        let mut original = 0;
        if owner.is_some() {
            let ship_receiver = matches!(fit_key, Key::Ship);
            // An adaptive armor publication pairs exactly its eight module/
            // ship armor rows; a due character deadline needs the ordinary
            // full enumeration for every unrelated moved attribute.
            let bounded_armor = match sources {
                FitDeltaSources::ReactiveArmor(module)
                    if !record.fit.next_character_expiry().is_some_and(|deadline| deadline <= at) => Some(module),
                _ => None,
            };
            let mut candidates = if let Some(module) = bounded_armor {
                ARMOR_RESONANCE.into_iter()
                    .flat_map(|attr| [(Key::Module(module), attr), (Key::Ship, attr)])
                    .collect()
            } else {
                let mut candidates = record.fit.fitting_candidates(&record.fit);
                if let FitDeltaSources::Fitting(patch) = sources {
                    // A copied Ship/Module destination is never a drone's
                    // receiving key; the patch's attribute destinations remap
                    // onto the own drone key instead.
                    candidates.extend(patch.candidates().iter()
                        .map(|&(key, attr)| (if ship_receiver { key } else { fit_key }, attr)));
                }
                candidates
            };
            // Receiving authority: ship rows stay Ship/Module, a resident
            // drone publishes only its own key.
            candidates.retain(|&(key, _)| match fit_key {
                Key::Ship => matches!(key, Key::Ship | Key::Module(_)),
                _ => key == fit_key,
            });
            candidates.sort_unstable();
            candidates.dedup();
            before = record.fit.fitting_values(&candidates);
            original = before.len();
            // The eight armor rows are the whole bounded receipt. A due
            // deadline already took the full enumeration above, so only that
            // path still pairs incoming seed attributes with every receiver.
            if bounded_armor.is_none() {
                let mut seeds = BTreeSet::new();
                let sde = record.fit.sde();
                if record.fit.next_character_expiry().is_some_and(|deadline| deadline <= at) {
                    character_seed_attrs(sde, &record.fit.character_inputs(), &mut seeds);
                }
                match sources {
                    FitDeltaSources::Character(inputs) => character_seed_attrs(sde, inputs, &mut seeds),
                    FitDeltaSources::Projections(projections) => projection_seed_attrs(sde, projections, &mut seeds),
                    FitDeltaSources::Runtime(changes) => runtime_seed_attrs(sde, changes.iter().map(|c| c.collection_id), &mut seeds),
                    FitDeltaSources::Owner(inputs) => {
                        let (character, nodes, runtime) = inputs.notification_sources();
                        character_seed_attrs(sde, character, &mut seeds);
                        for (type_id, charge_type, effect_id) in nodes {
                            effect_seed_attrs(sde, type_id, &mut seeds);
                            if let Some(charge) = charge_type { effect_seed_attrs(sde, charge, &mut seeds); }
                            if let Some(effect) = effect_id.and_then(|id| sde.dogma.effect(id)) {
                                seeds.extend(effect.modifiers().iter().map(|modifier| modifier.modified));
                            }
                        }
                        runtime_seed_attrs(sde, runtime.iter().map(|s| s.collection_id), &mut seeds);
                    }
                    _ => {}
                }
                let mut receivers = vec![fit_key];
                if ship_receiver {
                    receivers.extend(record.fit.fitted_modules().map(|(item, _)| Key::Module(item)));
                }
                let mut extra: Vec<(Key, i32)> = receivers.into_iter()
                    .flat_map(|key| seeds.iter().map(move |&attr| (key, attr)))
                    .filter(|pair| candidates.binary_search(pair).is_err())
                    .collect();
                extra.sort_unstable();
                before.extend(record.fit.fitting_values(&extra));
            }
        }
        Some(FitDeltaBefore { item, fit_key, owner, membership, drone_generation,
            old_condition, old_cap, old_tau, old_shield_max, old_shield_tau, before, original })
    }

    /// Complete one publication after the caller's mutation and output
    /// rebase: re-verify the resident identity (live phase, fit key,
    /// membership and a captured drone generation), keep only the
    /// incoming-only seeds that are still legal final candidates, run the
    /// single old-to-final capacitor rebase the gate deferred, then emit the
    /// comparison through `publish_fit_delta` when the real owner still
    /// passes the same fences and can receive it.
    pub(in crate::space) fn finish_fit_delta(&mut self, before: FitDeltaBefore, transition: Option<(i64, bool)>) {
        let Some(record) = self.actor_combat.get(&before.item) else { return };
        if !matches!(record.phase, CombatPhase::Live) || record.fit_key != before.fit_key
            || record.membership_generation != before.membership { return; }
        if before.drone_generation.is_some_and(|generation|
            !self.drone(before.item).is_some_and(|drone| drone.generation == generation)) { return; }
        let mut values = before.before;
        if values.len() > before.original {
            let final_candidates: HashSet<(Key, i32)> =
                record.fit.fitting_candidates(&record.fit).into_iter().collect();
            let tail = values.split_off(before.original);
            values.extend(tail.into_iter()
                .filter(|(key, attr, _)| final_candidates.contains(&(*key, *attr))));
        }
        let fit_key = before.fit_key;
        let changes = record.fit.fitting_changes(&values);
        let new_cap = record.fit.value(fit_key, ATTR_CAPACITOR_CAPACITY);
        let new_tau = record.fit.value(fit_key, ATTR_RECHARGE_RATE);
        let new_shield_max = record.fit.value(fit_key, ATTR_SHIELD_CAPACITY);
        let new_shield_tau = record.fit.value(fit_key, ATTR_SHIELD_RECHARGE_RATE);
        let charge = (before.old_cap != new_cap || before.old_tau != new_tau)
            .then(|| rebase_capacitor(before.item, before.old_cap, before.old_tau, new_cap, new_tau));
        let (Some(owner), Some(new_condition)) = (before.owner, self.ship_condition(before.item)) else { return };
        let admitted = match before.fit_key {
            Key::Ship => self.pilot_ball(owner).is_some_and(|ball|
                ball.id == before.item && ball.char_id == Some(owner) && ball.owner_id == owner),
            Key::Drone(id) if id == before.item => before.drone_generation.is_some_and(|generation|
                self.drone_publication_current(before.item, owner, generation, before.membership)),
            _ => false,
        };
        if !admitted || !self.registry.is_online(owner) { return; }
        self.publish_fit_delta(owner, before.item, fit_key, changes, FittingDelta {
            old_condition: before.old_condition, new_condition,
            old_cap: before.old_cap, new_cap,
            old_shield_max: before.old_shield_max, new_shield_max,
            old_shield_tau: before.old_shield_tau, new_shield_tau,
            charge,
        }, transition);
    }

    /// Apply one committed fitting patch to the currently piloted live incarnation.
    pub(crate) fn publish_fitting(&mut self, ship: i64, owner: i64, generation: u64,
        patch: CommittedFitting, cleanup: Vec<FittingHeatCleanup>) -> bool {
        if self.persistent_generation(ship) != Some(generation)
            || !self.pilot_ball(owner).is_some_and(|ball| ball.id == ship && ball.char_id == Some(owner) && ball.owner_id == owner)
            || !self.actor_combat.get(&ship).is_some_and(|record| record.membership_generation == generation && record.phase == CombatPhase::Live) {
            return false;
        }
        // The receipt snapshots the pre-expiry fit before the shared gate's
        // expiry and the patch touch it; the publication completes once after
        // the output rebase.
        let Some((expired, _, receipt)) =
            self.prepare_fit_publication(ship, Some(FitDeltaSources::Fitting(&patch)))
        else { return false };
        let old_cargo = self.actor_combat[&ship].fit
            .override_state(Key::Ship, infinity_core::consts::dogma::ATTR_CAPACITY);
        for chunk in cleanup { heat::apply_fitting_cleanup(self, chunk); }
        let (motion, brain, stance) = {
            let record = self.actor_combat.get_mut(&ship).unwrap();
            // The mode item lands with the patch; a receipt whose new item is
            // already resident is a duplicate delivery and emits no events.
            let stance = patch.ship_stance()
                .filter(|transition| record.fit.module_state(transition.new_item).is_none());
            record.fit.apply_committed_fitting(&patch);
            record.fit_revision = record.fit_revision.saturating_add(1);
            // The mechanic dies with the module binding the patch removed;
            // unrelated character, projection and overload deltas keep it.
            record.reconcile_reactive_armor();
            let motion = crate::services::effects::propulsion::kinematics(&record.fit, Key::Ship);
            // A raw cargo-mode change republishes the brain even when the
            // numeric wire value is unchanged; the owner, not the operating
            // GM, is told.
            let brain = (record.fit.override_state(Key::Ship, infinity_core::consts::dogma::ATTR_CAPACITY) != old_cargo)
                .then(|| crate::services::dogma::brain::compile(&record.fit, owner, ship));
            (motion, brain, stance)
        };
        // Controlled drones share the owner's ship domain; the same committed
        // delta lands on their resident copies without touching their own
        // drone, projected or condition state, and each publishes only its own
        // rows to the real owner.
        self.propagate_ship_input(ship, FitDeltaSources::Fitting(&patch), |fit| fit.apply_ship_fitting(&patch));
        // A consumed expiry also rebases NPC targeting and drone-spec outputs
        // the fitting path itself does not recompute.
        if expired { self.rebase_fitted_outputs(ship); } else { self.refresh_fitted_stats(ship); }
        let revision = self.projection_revision(ship);
        crate::services::effects::propulsion::queue_fitting_kinematics(self, ship, generation, revision, motion);
        if let Some(before) = receipt { self.finish_fit_delta(before, None); }
        if let Some((character, ship_value, structure)) = brain {
            self.notify_pilot(owner, crate::services::dogma::brain::UPDATED_EVENT,
                crate::services::dogma::brain::update_args(owner, character, ship_value, structure));
        }
        // A committed offline or a module leaving the fit is the forced
        // terminal for any duration-bounded activation record on the ship;
        // the record's own generation fences stale matches.
        for module in crate::services::effects::active_cycle::ship_records(ship) {
            if self.actor_combat[&ship].fit.module_state(module).is_none_or(|(online, _)| !online) {
                crate::services::effects::active_cycle::finish_module(self, ship, module);
            }
        }
        if let Some(stance) = stance {
            self.notify_pilot(owner, "OnStanceActive",
                crate::services::ship_stance::state::stance_active_args(ship, stance.new));
            let slim = crate::services::ship_stance::state::stance_slim_value(
                Some(stance.old), stance.switched_at, stance.new);
            self.command(ship, move |ball, stamp, _| {
                ball.slim.retain(|(key, _)| *key != "shipStance");
                ball.slim.push(("shipStance", slim));
                vec![crate::space::event(stamp, "OnSlimItemChange", vec![Value::Int(ship), ball.slim_item()])]
            });
        }
        true
    }

    /// Publish one resident-fit delta to the owner's
    /// `OnModuleAttributeChanges` channel.
    ///
    /// `receiving` is the receipt's own key: a ship's module rows key to
    /// their own item while a resident drone publishes only its own
    /// `Key::Drone(item)` rows; copied owner ship keys and charge receivers
    /// are never published. Capacity rows carry their paired current charge
    /// or damage and recharge-only changes republish the stored charge on
    /// the receiving item, in the cold publisher's order. `transition` names
    /// the active-state module and start flag and adds one DEBUG event per
    /// emitted row; the cold path passes None. One (owner, item, attribute)
    /// row emits at most once per call; pairing duplicates drop silently.
    pub(in crate::space) fn publish_fit_delta(&mut self, owner: i64, item: i64, receiving: Key,
        changes: Vec<(Key, i32, f64, f64)>, scalars: FittingDelta, transition: Option<(i64, bool)>) {
        let mut notices = Vec::new();
        let mut emitted = HashSet::new();
        let mut emit = |item: i64, attr: i32, new: f64, old: f64| {
            if !emitted.insert((owner, item, attr)) { return; }
            notices.push(attribute_change(owner, Value::Int(item), attr, Value::Float(new), Value::Float(old)));
            if let Some((source_module_id, start)) = transition {
                tracing::debug!(source_module_id, affected_item_id = item, attribute_id = attr,
                    old_value = old, new_value = new, start, "active transition attribute delta");
            }
        };
        for (key, attr, old, new) in changes {
            let target = match (receiving, key) {
                (Key::Ship, Key::Module(id)) => id,
                (receiving, key) if key == receiving => item,
                _ => continue,
            };
            emit(target, attr, new, old);
            if key != receiving { continue; }
            let pair = match attr {
                ATTR_SHIELD_CAPACITY => Some((ATTR_SHIELD_CHARGE, scalars.old_condition.0, scalars.new_condition.0)),
                ATTR_ARMOR_HP => Some((ATTR_ARMOR_DAMAGE, scalars.old_condition.1, scalars.new_condition.1)),
                ATTR_STRUCTURE_HP => Some((ATTR_HULL_DAMAGE, scalars.old_condition.2, scalars.new_condition.2)),
                ATTR_CAPACITOR_CAPACITY => scalars.charge.map(|(old, new)| (ATTR_CHARGE, old, new)),
                _ => None,
            };
            if let Some((attr, old, new)) = pair {
                emit(item, attr, new, old);
            }
        }
        if scalars.old_shield_max == scalars.new_shield_max && scalars.old_shield_tau != scalars.new_shield_tau {
            emit(item, ATTR_SHIELD_CHARGE, scalars.new_condition.0, scalars.old_condition.0);
        }
        if scalars.old_cap == scalars.new_cap {
            if let Some((old, new)) = scalars.charge {
                emit(item, ATTR_CHARGE, new, old);
            }
        }
        if !notices.is_empty() {
            self.notify_pilot(owner, "OnModuleAttributeChanges", vec![Value::list(notices)]);
        }
    }

    /// Apply a committed override-only field delta to one owned drone's own node.
    ///
    /// Admission re-verifies the real owner of the ball and the drone record,
    /// an un-abandoned ownership, the original drone generation, persistent
    /// and record membership, the drone fit key and the live phase. A patch
    /// touching any other key, node topology or runtime flag is refused rather
    /// than silently succeeding; the attribute notification always reaches the
    /// real owner. An identical value produces no notice, and a consumed
    /// expiry still lands its outputs inside the same single rebase and
    /// publishes through the same receipt.
    pub(crate) fn publish_drone_fitting(
        &mut self, drone: i64, owner: i64, drone_generation: u64,
        membership: u64, patch: CommittedFitting,
    ) -> bool {
        let admitted = patch.overrides_only_on(Key::Drone(drone))
            && self.drone_publication_current(drone, owner, drone_generation, membership);
        let Some((expired, _, receipt)) = admitted
            .then(|| self.prepare_fit_publication(drone, Some(FitDeltaSources::Fitting(&patch))))
            .flatten() else { return false };
        let changed = self.actor_combat.get_mut(&drone).is_some_and(|record| {
            if !record.fit.apply_committed_fitting(&patch) { return false; }
            record.fit_revision = record.fit_revision.saturating_add(1);
            true
        });
        // A consumed expiry rebases outputs even when the delta was a no-op;
        // the receipt still finishes once to the same owner.
        if expired || changed { self.rebase_fitted_outputs(drone); }
        if let Some(before) = receipt { self.finish_fit_delta(before, None); }
        changed
    }
}

#[cfg(all(test, feature = "tests-space"))] mod tests;
