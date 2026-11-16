//! Fitted inputs of the live actor combat record: one mutable preparation gate,
//! real character-domain deadlines, stat rebases, projection replacement,
//! module online/active/overload flags, the fit revision and charge
//! incarnations. `with_combat_fit` is the only way a cycle borrows the resident
//! fit: the same private prepare verifies the ball, current membership and
//! live phase, captures one FILETIME, lets due booster and expert-system
//! deadlines expire and rebases the actor outputs once. Its
//! `with_combat_fit_active` sibling lends the same prepared fit with only the
//! listed inactive module flags raised for the borrow and restored before it
//! returns, without a revision, propagation or rebase of that temporary
//! state. Nothing here loads,
//! clones or falls back to a database fit, and no fit borrow escapes a call.
//! A resident ship or drone's publications run through
//! `prepare_fit_publication`: the receipt begins before expiry and defers the
//! capacitor rebase into the single emitter; a drone receipt publishes only
//! its own `Key::Drone` rows to the real owner.
//! Output rebases follow the existing condition, capacitor, destiny kinematics,
//! NPC targeting and drone spec paths; raw helpers never recurse through the
//! gate. A nearest-deadline actor timer shares the same prepare so expiry
//! applies even when no cycle ran, while membership and deadline identity
//! retire timers a renewal invalidated. `publish_drone_owner_inputs` is the
//! cold owner-handoff counterpart: the real owner's complete ship and
//! character domain lands on a still-live resident drone at one captured
//! FILETIME, without granting control and without a second clock read or
//! database load.

use super::{rebase_condition_at, CombatPhase, ShipStats, System};
use crate::client::{filetime_now, unix_ms_from_filetime_ceil};
use crate::services::combat::groups_health::rebase_capacitor;
use crate::services::effects::{projection::Projection, propulsion};
use crate::services::fit::{attributes::CommittedFitting, character::{CharacterInputs, OwnerInputs},
    Fit, Key};
use infinity_core::consts::{dogma::ATTR_CAPACITOR_CAPACITY, dogma::ATTR_MAX_TARGET_RANGE,
    dogma::ATTR_RECHARGE_RATE, dogma::ATTR_SCAN_RESOLUTION, OWNER_UNOWNED};

/// Identity of the resident fit one read observed, for callers that bind a
/// captured cycle to a membership. It is not a cross-cycle lock: an unrelated
/// later revision never cancels values this cycle already captured.
#[derive(Clone, Copy)]
pub(crate) struct FitStamp {
    pub membership: u64,
}

/// Membership, module, incarnation and type of a resident charge captured
/// before its last round committed; used to remove exactly that node.
#[derive(Clone, Copy)]
pub(crate) struct ChargeToken {
    pub ship: i64,
    pub membership: u64,
    pub module: i64,
    pub incarnation: u64,
    pub type_id: i32,
}

/// The incoming modifier sources one publication seeds its old-value
/// snapshot from. `Current` covers mutations already expressible through the
/// source nodes the fit holds, such as flag and overload writes; the other
/// variants name an input package the caller is about to apply, including
/// the cold `Owner` handoff to a resident drone. A sources value never
/// outlives the call that created it.
#[derive(Clone, Copy)]
pub(in crate::space) enum FitDeltaSources<'a> {
    Current,
    Character(&'a CharacterInputs),
    Projections(&'a [Projection]),
    Fitting(&'a CommittedFitting),
    Owner(&'a OwnerInputs),
}

/// One publication's pre-expiry snapshot: the receiving item and its
/// resident fit key, the real owner while an online pilot can receive rows
/// (None otherwise), the record membership and a resident drone's captured
/// generation, the old condition and the old capacitor and shield scalars,
/// and the old wire values. Entries before `original` are the surviving
/// generic candidates; the rest are incoming-only seeds. The deferred
/// capacitor rebase lands in `finish_fit_delta`, never in both the gate and
/// the finish.
pub(in crate::space) struct FitDeltaBefore {
    pub item: i64,
    pub fit_key: Key,
    pub owner: Option<i64>,
    pub membership: u64,
    pub drone_generation: Option<u64>,
    pub old_condition: (f64, f64, f64),
    pub old_cap: f64,
    pub old_tau: f64,
    pub old_shield_max: f64,
    pub old_shield_tau: f64,
    pub before: Vec<(Key, i32, f64)>,
    pub original: usize,
}

impl System {
    /// The single mutable preparation behind every resident fit access.
    ///
    /// Every fit read in space goes through this gate: an elapsed real
    /// deadline expires the retained character rows once at the captured
    /// FILETIME and rebases the in-memory capacitor balance. Returns None for
    /// a missing ball, a stale or absent membership, or a record that is no
    /// longer live; otherwise `(expired, at)`: whether an expiry actually
    /// rewrote the fit, and this gate's single captured FILETIME.
    /// `receipt_owned` is call-local: it is set only while this call's
    /// publication receipt holds the receiving key's capacitor rebase, which
    /// then lands once in `finish_fit_delta` instead of here.
    fn prepare_core(&mut self, item: i64, at: i64, receipt_owned: bool) -> Option<(bool, i64)> {
        if self.ball(item).is_none() { return None; }
        if let Some(generation) = self.persistent_generation(item) {
            if self.actor_combat.get(&item).is_none_or(|record| record.membership_generation != generation) {
                return None;
            }
        }
        let record = self.actor_combat.get_mut(&item)?;
        if !matches!(record.phase, CombatPhase::Live) { return None; }
        let due = record.fit.next_character_expiry().is_some_and(|deadline| deadline <= at);
        if !due {
            self.arm_character_expiry(item);
            return Some((false, at));
        }
        let ship_basis = (matches!(record.fit_key, Key::Ship) && !receipt_owned)
            .then(|| (record.fit.value(Key::Ship, ATTR_CAPACITOR_CAPACITY), record.fit.value(Key::Ship, ATTR_RECHARGE_RATE)));
        if !record.fit.expire_character_inputs(at) {
            self.arm_character_expiry(item);
            return Some((false, at));
        }
        record.fit_revision = record.fit_revision.saturating_add(1);
        if let Some((old_capacity, old_tau)) = ship_basis {
            let (capacity, tau) = (record.fit.value(Key::Ship, ATTR_CAPACITOR_CAPACITY), record.fit.value(Key::Ship, ATTR_RECHARGE_RATE));
            if old_capacity != capacity || old_tau != tau {
                rebase_capacitor(item, old_capacity, old_tau, capacity, tau);
            }
        }
        self.arm_character_expiry(item);
        Some((true, at))
    }

    /// The publication counterpart of the shared gate. The receipt begins
    /// before expiry so its snapshot still carries pre-expiry values: always
    /// when `planned` names incoming sources, otherwise only when a real
    /// character deadline is due at this gate's single FILETIME. A present
    /// receipt defers the capacitor rebase into `finish_fit_delta`. Resident
    /// `Key::Ship` and own `Key::Drone(item)` records produce receipts;
    /// every other key still passes the gate and returns None.
    pub(in crate::space) fn prepare_fit_publication(&mut self, item: i64,
        planned: Option<FitDeltaSources>,
    ) -> Option<(bool, i64, Option<FitDeltaBefore>)> {
        let at = filetime_now();
        let begin = self.actor_combat.get(&item).is_some_and(|record|
            matches!(record.phase, CombatPhase::Live)
            && (matches!(record.fit_key, Key::Ship) || record.fit_key == Key::Drone(item))
            && (planned.is_some()
                || record.fit.next_character_expiry().is_some_and(|deadline| deadline <= at)));
        let receipt = begin
            .then(|| self.begin_fit_delta(item, at, planned.unwrap_or(FitDeltaSources::Current)))
            .flatten();
        let (expired, at) = self.prepare_core(item, at, receipt.is_some())?;
        Some((expired, at, receipt))
    }

    /// Schedule the actor timer for the record's nearest real deadline.
    ///
    /// The armed (deadline, token) pair is stored on the record; a renewal or
    /// removal changes it and leaves the queued closure dead. The timer only
    /// re-enters the same prepare, so a deadline that was already consumed
    /// while reads were in flight cannot fire twice.
    pub(in crate::space) fn arm_character_expiry(&mut self, item: i64) {
        let Some(record) = self.actor_combat.get_mut(&item) else { return };
        let membership = record.membership_generation;
        match record.fit.next_character_expiry() {
            Some(deadline) if record.character_timer.map(|timer| timer.0) != Some(deadline) => {
                let token = crate::space::targeting::next_generation();
                record.character_timer = Some((deadline, token));
                self.schedule_at(unix_ms_from_filetime_ceil(deadline), move |s| {
                    if s.actor_combat.get(&item).is_none_or(|record|
                        record.membership_generation != membership || record.character_timer != Some((deadline, token))) { return; }
                    s.actor_combat.get_mut(&item).unwrap().character_timer = None;
                    let prepared = s.prepare_fit_publication(item, None);
                    if matches!(prepared, Some((true, _, _))) { s.rebase_fitted_outputs(item); }
                    if let Some((_, _, Some(before))) = prepared { s.finish_fit_delta(before, None); }
                });
            }
            Some(_) => {}
            None => { record.character_timer = None; }
        }
    }

    /// Recompute every live output of the resident fit once.
    ///
    /// Defense uses the existing condition rebase (absolute armor and hull
    /// damage kept, shield recharge re-anchored); motion, signature and NPC
    /// targeting queue through the existing destiny boundary; an owned drone
    /// re-derives its spec from the same fit instead of reloading one. This
    /// never re-enters `with_combat_fit`.
    pub(in crate::space) fn rebase_fitted_outputs(&mut self, item: i64) {
        self.refresh_fitted_stats(item);
        let drone = self.drone(item);
        let sde = self.projection_sde.clone();
        let Some(record) = self.actor_combat.get(&item) else { return };
        let key = record.fit_key;
        let kinematics = propulsion::kinematics(&record.fit, key);
        if let Some(drone) = drone {
            match crate::services::drones::worker::spec(&record.fit, key, drone.spec.type_id) {
                Ok(spec) => {
                    let owner_projection = (drone.owner != OWNER_UNOWNED)
                        .then_some(drone.controller).flatten()
                        .map(|controller| (controller, self.projection_revision(controller)));
                    self.drone_refresh(item, drone.generation, spec, kinematics, owner_projection);
                }
                Err(crate::services::CallError::Internal(error)) => tracing::warn!(%error, item, "resident drone spec refresh failed"),
                Err(crate::services::CallError::User { msg, .. }) => tracing::warn!(%msg, item, "resident drone spec refresh failed"),
            }
            return;
        }
        let revision = self.projection_revision(item);
        let type_id = self.ball(item).map(|ball| ball.type_id).unwrap_or_default();
        let has_target_range = sde.as_ref().is_some_and(|sde|
            sde.types.attribute(type_id, ATTR_MAX_TARGET_RANGE).is_some()
            || sde.dogma.attribute(ATTR_MAX_TARGET_RANGE).and_then(|a| a.default_value).is_some_and(|value| value > 0.0));
        let targeting = (record.fit.value(key, ATTR_SCAN_RESOLUTION),
            has_target_range.then(|| record.fit.value(key, ATTR_MAX_TARGET_RANGE)));
        self.queue_projection_kinematics(item, revision, kinematics);
        self.refresh_npc_targeting(item, targeting.0, targeting.1);
    }
    /// Apply one changed ship-domain input to every drone still controlled by
    /// `ship`. Each drone passes the shared publication gate with the
    /// caller's declared sources, keeps its own drone, projected and
    /// condition state, is rebased only when the delta or a consumed expiry
    /// changed its resident fit, and finishes its own receipt to the real
    /// owner either way.
    pub(in crate::space) fn propagate_ship_input(&mut self, ship: i64,
        sources: FitDeltaSources<'_>, apply: impl Fn(&mut Fit) -> bool,
    ) {
        for drone in self.drones().filter(|drone| drone.controller == Some(ship)).map(|drone| drone.id).collect::<Vec<_>>() {
            let Some((expired, _, receipt)) = self.prepare_fit_publication(drone, Some(sources)) else { continue };
            let changed = self.actor_combat.get_mut(&drone).is_some_and(|record| {
                if !apply(&mut record.fit) { return false; }
                record.fit_revision = record.fit_revision.saturating_add(1);
                true
            });
            // A consumed expiry rebases outputs even when the delta was a no-op.
            if expired || changed { self.rebase_fitted_outputs(drone); }
            if let Some(before) = receipt { self.finish_fit_delta(before, None); }
        }
    }

    /// Borrow the live fit through the single mutable gate.
    ///
    /// The closure runs inside this actor turn with the prepared fit, its
    /// receiver key and the observed stamp; references cannot escape. Missing,
    /// stale-member and retired records read as None.
    pub(crate) fn with_combat_fit<R: 'static>(
        &mut self, item: i64, read: impl FnOnce(&Fit, Key, FitStamp) -> R,
    ) -> Option<R> {
        let (expired, _, receipt) = self.prepare_fit_publication(item, None)?;
        if expired { self.rebase_fitted_outputs(item); }
        if let Some(before) = receipt { self.finish_fit_delta(before, None); }
        let record = self.actor_combat.get(&item)?;
        let stamp = FitStamp { membership: record.membership_generation };
        Some(read(&record.fit, record.fit_key, stamp))
    }

    /// Borrow the live fit while the listed modules hold a scoped active bit.
    ///
    /// Same single prepare as `with_combat_fit`, including the ordinary expiry
    /// rebase while the fit is still untouched; each listed module found
    /// inactive is set active for the callback only (no fit revision, no
    /// propagation and no output rebase), then the mutable record borrow is
    /// released, the callback runs against a shared fit borrow, and the
    /// record is reacquired to restore exactly those modules on every
    /// callback result, including a refusal or error value it returns. A
    /// module already active is never toggled, so a repeated cycle borrows
    /// the unchanged live fit like `with_combat_fit`.
    pub(crate) fn with_combat_fit_active<R: 'static>(
        &mut self, item: i64, modules: impl Iterator<Item = i64>,
        read: impl FnOnce(&Fit, Key, FitStamp) -> R,
    ) -> Option<R> {
        let (expired, _, receipt) = self.prepare_fit_publication(item, None)?;
        if expired { self.rebase_fitted_outputs(item); }
        // The expiry publication completes before the scoped bits are raised,
        // so a temporary evaluation flag never reaches the wire.
        if let Some(before) = receipt { self.finish_fit_delta(before, None); }
        let changed: Vec<i64> = self.actor_combat.get_mut(&item).map(|record| {
            modules.filter(|module| record.fit.set_module_active(*module, true)).collect()
        })?;
        let result = self.actor_combat.get(&item).map(|record| {
            let stamp = FitStamp { membership: record.membership_generation };
            read(&record.fit, record.fit_key, stamp)
        });
        if let Some(record) = self.actor_combat.get_mut(&item) {
            for module in changed { record.fit.set_module_active(module, false); }
        }
        result
    }

    /// Publish a committed character-domain replacement through the same gate.
    ///
    /// `item` is the owner's piloted ship or one of their owned drones; the
    /// owner, not a delegated controller, owns the attributes. A ship publish
    /// also carries the same inputs into every owned drone's resident fit.
    pub(crate) fn publish_character_inputs(
        &mut self, item: i64, owner: i64, generation: u64,
        inputs: &CharacterInputs,
    ) -> bool {
        let drone = self.drone(item).filter(|drone| drone.owner == owner);
        let admitted = if drone.is_some() {
            self.persistent_generation(item) == Some(generation)
                && self.actor_combat.get(&item).is_some_and(|record|
                    record.membership_generation == generation && matches!(record.phase, CombatPhase::Live))
        } else {
            self.persistent_generation(item) == Some(generation)
                && self.pilot_ball(owner).is_some_and(|ball| ball.id == item && ball.char_id == Some(owner) && ball.owner_id == owner)
                && self.actor_combat.get(&item).is_some_and(|record|
                    record.membership_generation == generation && matches!(record.phase, CombatPhase::Live))
        };
        if !admitted { return false; }
        let Some((_, _, receipt)) =
            self.prepare_fit_publication(item, Some(FitDeltaSources::Character(inputs)))
        else { return false };
        self.apply_character_inputs(item, inputs, receipt.is_some());
        if let Some(before) = receipt { self.finish_fit_delta(before, None); }
        if drone.is_none() {
            for drone in self.drones().filter(|drone| drone.owner == owner).map(|drone| drone.id).collect::<Vec<_>>() {
                // Every owned drone publishes through its own receipt.
                let Some((_, _, receipt)) =
                    self.prepare_fit_publication(drone, Some(FitDeltaSources::Character(inputs)))
                else { continue };
                self.apply_character_inputs(drone, inputs, receipt.is_some());
                if let Some(before) = receipt { self.finish_fit_delta(before, None); }
            }
        }
        true
    }

    pub(super) fn drone_publication_current(
        &self, drone: i64, owner: i64, drone_generation: u64, membership: u64,
    ) -> bool {
        owner != OWNER_UNOWNED
            && self.ball(drone).is_some_and(|ball| ball.owner_id == owner)
            && self.drone(drone).is_some_and(|record| record.owner == owner && record.generation == drone_generation)
            && self.persistent_generation(drone) == Some(membership)
            && self.actor_combat.get(&drone).is_some_and(|record|
                record.membership_generation == membership && record.fit_key == Key::Drone(drone)
                && matches!(record.phase, CombatPhase::Live))
    }

    /// Publish one owner's complete ship and character domain into a resident
    /// drone fit at the cold handoff boundary.
    ///
    /// Every captured tuple re-verifies against the current instance: the
    /// real owner of the ball and drone record, an un-abandoned ownership,
    /// the original drone generation, persistent and record membership, the
    /// drone fit key and the live phase. Only the owner domain moves;
    /// controller, owner rights, order, target and generation are never
    /// granted here and a delegated controller is never the material source.
    /// The receipt begins before the package lands so its snapshot still
    /// carries the replaced domain's old values.
    pub(crate) fn publish_drone_owner_inputs(
        &mut self, drone: i64, owner: i64, drone_generation: u64,
        membership: u64, inputs: &OwnerInputs,
    ) -> bool {
        let admitted = self.drone_publication_current(drone, owner, drone_generation, membership);
        let Some((_, at, receipt)) = admitted
            .then(|| self.prepare_fit_publication(drone, Some(FitDeltaSources::Owner(inputs))))
            .flatten() else { return false };
        let Some(record) = self.actor_combat.get_mut(&drone) else { return false };
        record.fit.apply_owner_inputs(inputs);
        // A deadline already past in the package expires inside this turn at
        // the same captured FILETIME; it can never resurrect into outputs.
        record.fit.expire_character_inputs(at);
        record.fit_revision = record.fit_revision.saturating_add(1);
        self.arm_character_expiry(drone);
        self.rebase_fitted_outputs(drone);
        if let Some(before) = receipt { self.finish_fit_delta(before, None); }
        true
    }

    /// Apply published inputs to one already-gated record and rebase once.
    /// `receipt_owned` marks a ship whose publication receipt holds the
    /// capacitor rebase; every other record rebases here as before.
    fn apply_character_inputs(&mut self, item: i64, inputs: &CharacterInputs, receipt_owned: bool) {
        let Some(record) = self.actor_combat.get_mut(&item) else { return };
        let ship_basis = (matches!(record.fit_key, Key::Ship) && !receipt_owned)
            .then(|| (record.fit.value(Key::Ship, ATTR_CAPACITOR_CAPACITY), record.fit.value(Key::Ship, ATTR_RECHARGE_RATE)));
        record.fit.apply_character_inputs(inputs);
        record.fit_revision = record.fit_revision.saturating_add(1);
        if let Some((old_capacity, old_tau)) = ship_basis {
            let (capacity, tau) = (record.fit.value(Key::Ship, ATTR_CAPACITOR_CAPACITY), record.fit.value(Key::Ship, ATTR_RECHARGE_RATE));
            if old_capacity != capacity || old_tau != tau {
                rebase_capacitor(item, old_capacity, old_tau, capacity, tau);
            }
        }
        self.arm_character_expiry(item);
        self.rebase_fitted_outputs(item);
    }

    /// Token of the charge currently resident on one module of a live fit.
    pub(crate) fn fitted_charge_token(&mut self, ship: i64, module: i64) -> Option<ChargeToken> {
        self.with_combat_fit(ship, move |fit, _, stamp| {
            fit.charge_incarnation(module).map(|(incarnation, type_id)|
                ChargeToken { ship, membership: stamp.membership, module, incarnation, type_id })
        }).flatten()
    }

    /// Remove exactly the resident charge a committed final consumption named,
    /// refusing a same-type reload that reused the module meanwhile.
    pub(crate) fn remove_fitted_charge(&mut self, token: ChargeToken) -> bool {
        let removable = self.ball(token.ship).is_some()
            && self.persistent_generation(token.ship).is_none_or(|generation| generation == token.membership)
            && self.actor_combat.get(&token.ship).is_some_and(|record|
                record.membership_generation == token.membership
                && record.fit.charge_incarnation(token.module) == Some((token.incarnation, token.type_id)));
        let Some((expired, _, receipt)) = self.prepare_fit_publication(token.ship,
            removable.then_some(FitDeltaSources::Current)) else { return false };
        let removed = self.actor_combat.get_mut(&token.ship).is_some_and(|record| {
            if record.membership_generation != token.membership
                || !record.fit.remove_charge(token.module, token.incarnation, token.type_id) { return false; }
            record.fit_revision = record.fit_revision.saturating_add(1);
            true
        });
        if expired || removed { self.rebase_fitted_outputs(token.ship); }
        // The token-approved removal also drops the matching charge node from
        // every controlled drone's ship-domain copy; each drone's own
        // incarnation token stays independent and is never matched here. A
        // refused removal still finishes a consumed expiry receipt.
        if let Some(before) = receipt { self.finish_fit_delta(before, None); }
        if removed {
            self.propagate_ship_input(token.ship, FitDeltaSources::Current, move |fit| fit.remove_charge_node(token.module, token.type_id));
        }
        removed
    }

    /// Pay one charge under a live permit; a last round also removes the fit
    /// charge node its token names in this same actor turn. Refusal leaves
    /// the permit's finite use to the caller's owner.
    pub(crate) fn consume_fitted_loaded(&mut self, permit: super::loaded::LoadedPermit, token: ChargeToken) -> Option<(i32, i64)> {
        let Some(record) = self.actor_combat.get(&token.ship) else { return None };
        if permit.ship != token.ship || permit.module != token.module || permit.type_id != token.type_id
            || self.ball(token.ship).is_none() || !matches!(record.phase, CombatPhase::Live)
            || !matches!(record.fit_key, Key::Ship) || record.membership_generation != token.membership
            || self.persistent_generation(token.ship).is_some_and(|generation| generation != token.membership)
            || record.fit.charge_incarnation(token.module) != Some((token.incarnation, token.type_id)) { return None; }
        let remaining = self.consume_loaded(permit)?;
        // The prevalidated identity still holds this same actor turn, so the
        // token-approved removal of the emptied node cannot fail.
        if remaining.1 == 0 { self.remove_fitted_charge(token); }
        Some(remaining)
    }

    pub(crate) fn refresh_fitted_stats(&mut self, ship: i64) -> bool {
        let at = self.combat_fact_time();
        let Some(record) = self.actor_combat.get_mut(&ship) else { return false };
        if !matches!(record.phase, CombatPhase::Live) { return false; }
        let Some(next) = record.revision.checked_add(1) else { return false; };
        let stats = ShipStats::from_fit(&record.fit, record.fit_key);
        if !rebase_condition_at(&mut record.condition, stats, at) { return false; }
        record.fit_revision = record.fit_revision.saturating_add(1);
        record.revision = next;
        let health = [0, 1, 2].map(|i| if record.condition.stats.max[i] > 0.0 {
            (record.condition.now[i] / record.condition.stats.max[i]).clamp(0.0, 1.0)
        } else { 0.0 });
        if self.ball(ship).is_some_and(|ball| ball.health != health) { self.set_health(ship, health); }
        true
    }

    /// Same replacement without the stats rebase; the synchronous projection
    /// refresh merges it into one output rebase with a pending expiry.
    pub(in crate::space) fn set_fitted_projections(&mut self, ship: i64,
        projections: Vec<crate::services::effects::projection::Projection>, revision: Option<u64>) -> bool {
        let Some(record) = self.actor_combat.get_mut(&ship) else { return false };
        if !matches!(record.phase, CombatPhase::Live) || record.revision == u64::MAX { return false; }
        let key = record.fit_key;
        record.fit.replace_projections_for(key, projections, revision);
        record.fit_revision = record.fit_revision.saturating_add(1);
        true
    }

    pub(crate) fn fitted_module_online(&mut self, ship: i64, module: i64, online: bool) -> bool {
        let Some((expired, _, receipt)) = self.prepare_fit_publication(ship, None) else { return false };
        let Some(record) = self.actor_combat.get_mut(&ship) else { return false };
        if record.revision == u64::MAX || !record.fit.set_module_online(module, online) {
            if expired { self.rebase_fitted_outputs(ship); }
            if let Some(before) = receipt { self.finish_fit_delta(before, None); }
            return false;
        }
        record.fit_revision = record.fit_revision.saturating_add(1);
        self.propagate_ship_input(ship, FitDeltaSources::Current, move |fit| fit.set_module_online(module, online));
        self.rebase_fitted_outputs(ship);
        if let Some(before) = receipt { self.finish_fit_delta(before, None); }
        true
    }

    /// Set one module's active bit and publish the resulting attribute delta
    /// to the piloted ship's owner through the single ship receipt; a consumed
    /// expiry merges into the same batch. A same-state or missing-module write
    /// plans no snapshot and still finishes a due receipt; a missing recipient
    /// skips only the publication.
    pub(crate) fn fitted_module_active(&mut self, ship: i64, module: i64, active: bool) -> bool {
        // The candidate snapshot only exists when the flag actually moves or
        // a real deadline is due.
        let planned = self.actor_combat.get(&ship).is_some_and(|record|
            record.fit.module_state(module).is_some_and(|state| state.1 != active))
            .then_some(FitDeltaSources::Current);
        let Some((expired, _, receipt)) = self.prepare_fit_publication(ship, planned)
        else { return false };
        let Some(record) = self.actor_combat.get(&ship) else { return false };
        if record.revision == u64::MAX
            || !record.fit.module_state(module).is_some_and(|state| state.1 != active) {
            if expired { self.rebase_fitted_outputs(ship); }
            if let Some(before) = receipt { self.finish_fit_delta(before, None); }
            return false;
        }
        {
            let record = self.actor_combat.get_mut(&ship).unwrap();
            record.fit.set_module_active(module, active);
            record.fit_revision = record.fit_revision.saturating_add(1);
        }
        self.propagate_ship_input(ship, FitDeltaSources::Current, move |fit| fit.set_module_active(module, active));
        self.rebase_fitted_outputs(ship);
        if let Some(before) = receipt { self.finish_fit_delta(before, Some((module, active))); }
        true
    }

    /// Set one module's overload bit through the same receipt. The snapshot
    /// is planned only while a live module node can take the write; the setter
    /// admits a same-state write, so an effective no-op simply finds no final
    /// value change and emits nothing.
    pub(crate) fn fitted_module_overloaded(&mut self, ship: i64, module: i64, overloaded: bool) -> bool {
        let planned = self.actor_combat.get(&ship).is_some_and(|record|
            record.fit.module_state(module).is_some())
            .then_some(FitDeltaSources::Current);
        let Some((expired, _, receipt)) = self.prepare_fit_publication(ship, planned)
        else { return false };
        let Some(record) = self.actor_combat.get_mut(&ship) else { return false };
        if record.revision == u64::MAX || !record.fit.set_module_overloaded(module, overloaded) {
            if expired { self.rebase_fitted_outputs(ship); }
            if let Some(before) = receipt { self.finish_fit_delta(before, None); }
            return false;
        }
        record.fit_revision = record.fit_revision.saturating_add(1);
        self.propagate_ship_input(ship, FitDeltaSources::Current, move |fit| fit.set_module_overloaded(module, overloaded));
        self.rebase_fitted_outputs(ship);
        if let Some(before) = receipt { self.finish_fit_delta(before, None); }
        true
    }

    #[cfg(all(test, any(feature = "tests-combat", feature = "tests-effects", feature = "tests-slash", feature = "tests-space")))]
    pub(crate) fn combat_fit_revision(&self, ship: i64) -> Option<u64> {
        self.actor_combat.get(&ship).map(|record| record.fit_revision)
    }
}
