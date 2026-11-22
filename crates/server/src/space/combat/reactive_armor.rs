//! The adaptive armor hardener's actor-owned cycle mechanic. One running
//! activation holds a transient `ReactiveArmorState` on its ship's combat
//! record: the adapted operand profile, the open window's typed armor damage
//! tally and the activation generation it belongs to. A paid admitted cycle
//! start installs or reopens the state; the synchronous cycle end closes the
//! window and adapts the profile before any deferred work, so hits in the
//! heat/family gap are never sampled into the finished cycle. Accepted
//! nonlethal hits observed after `commit_damage_plan` tally into the open
//! window only; refused, replayed, flat and provisional-lethal damage never
//! reaches the tally. Stop, offline, refit, type and membership changes drop
//! the state through generation-fenced retirement or the shared reconcile.
//! The profile itself lives as the resident fit's operand overlay
//! (`services/fit/reactive_armor.rs`); nothing here is persisted, snapshotted
//! or loaded from the database, and the record's death destroys the state.
//! The shipped client effect only declares the four PreMul operand rows
//! (`eve/common/script/dogma/pythonEffects/adaptiveHardener.py`); the fixed
//! donor redistribution is this server's cycle algorithm. Fitting policy
//! already limits one hardener per ship, so the record carries a single
//! `Option`.
use super::inputs::FitDeltaSources;
use super::{ActorCombatRecord, CombatPhase, System};
use crate::services::fit::{reactive_armor::ARMOR_RESONANCE, Key};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

/// One activation's running state: the fitted module it binds, its identity
/// Arc (same handle the lifecycle's `stop` flag shares), the current adapted
/// operand profile, the open window's typed damage tally and whether a paid
/// cycle window is open.
pub(crate) struct ReactiveArmorState {
    module: i64,
    module_type: i32,
    generation: Arc<AtomicBool>,
    current: [f64; 4],
    damage: [f64; 4],
    window_open: bool,
}

impl ReactiveArmorState {
    /// Tally one accepted hit's post-resistance typed armor damage while a
    /// paid cycle window is open; a closed window drops the observation.
    pub(crate) fn observe(&mut self, armor: [f64; 4]) {
        if !self.window_open { return; }
        for (tally, amount) in self.damage.iter_mut().zip(armor) { *tally += amount; }
    }
}

/// Channel preference when damage amounts tie (em, explosive, kinetic,
/// thermal); the stable ascending sort reads donors and receivers off it.
const TIE_ORDER: [usize; 4] = [0, 3, 2, 1];

/// Public reactive-armor donor rule: every undamaged channel donates, and a
/// damaged window still donates from at least this many least-damaged
/// channels. Not an array index.
const MIN_DONORS: usize = 2;

/// One closed window's fixed redistribution: the `donors` least-damaged
/// channels, every undamaged channel and at least `MIN_DONORS`, each give
/// `min(1 - current, shift)` and take that amount themselves; the remaining
/// receivers lose an equal share of the total given. An all-zero window keeps
/// the current profile. Fixed arithmetic: no proportional weighting, clamps
/// or rounding.
fn adapt(current: [f64; 4], damage: [f64; 4], shift: f64) -> [f64; 4] {
    if damage.iter().all(|amount| *amount <= 0.0) { return current; }
    let donors = damage.iter().filter(|amount| **amount <= 0.0).count().max(MIN_DONORS);
    let mut order = TIE_ORDER;
    order.sort_by(|&a, &b| damage[a].partial_cmp(&damage[b]).unwrap_or(std::cmp::Ordering::Equal));
    let mut next = current;
    let mut given = 0.0;
    for &donor in &order[..donors] {
        let step = (1.0 - next[donor]).min(shift);
        next[donor] += step;
        given += step;
    }
    let share = given / (order.len() - donors) as f64;
    for &receiver in &order[donors..] { next[receiver] -= share; }
    next
}

impl ActorCombatRecord {
    /// Drop the mechanic when its module binding (type, online, active) or
    /// its effective operand overlay no longer stands. Runs inside an
    /// already-open actor mutation receipt after active/online/patch writes,
    /// so it emits nothing itself.
    pub(crate) fn reconcile_reactive_armor(&mut self) {
        let keep = self.reactive_armor.as_ref().is_some_and(|state|
            self.fit.has_reactive_armor(state.module)
                && self.fit.module_state(state.module) == Some((true, true))
                && self.fit.source_type(Key::Module(state.module)) == Some(state.module_type));
        if !keep { self.reactive_armor = None; }
    }
}

impl System {
    /// Shared retirement teardown: one bounded receipt captures the old wire
    /// values before the operand overlay and the module's state clear, the
    /// condition refresh and the receipt finish reuse the ordinary delta
    /// path. Callers fence generation and membership beforehand; the state
    /// still clears when the publication gate itself cannot open.
    fn reactive_armor_teardown(&mut self, ship: i64, module: i64) {
        let prepared = self.prepare_fit_publication(ship, Some(FitDeltaSources::ReactiveArmor(module)));
        if let Some(record) = self.actor_combat.get_mut(&ship) {
            if record.reactive_armor.as_ref().is_some_and(|state| state.module == module) {
                record.reactive_armor = None;
            }
            record.fit.clear_reactive_armor(module);
            record.fit_revision = record.fit_revision.saturating_add(1);
        }
        let Some((expired, _, receipt)) = prepared else { return };
        if expired { self.rebase_fitted_outputs(ship); } else { self.refresh_fitted_stats(ship); }
        if let Some(before) = receipt { self.finish_fit_delta(before, None); }
    }

    /// First-activation setup, not an observation window. Same-generation and
    /// absent state is untouched; another module's state is not ours to clear.
    /// The caller has already checked current owner, type, effect and
    /// membership authority.
    pub(crate) fn prepare_reactive_armor_activation(&mut self, ship: i64, membership: u64,
        module: i64, module_type: i32, generation: &Arc<AtomicBool>) -> bool {
        let Some(record) = self.actor_combat.get(&ship) else { return false };
        if !matches!(record.phase, CombatPhase::Live) || record.membership_generation != membership {
            return false;
        }
        if record.fit.source_type(Key::Module(module)) != Some(module_type) { return false; }
        let stale = record.reactive_armor.as_ref()
            .is_some_and(|state| state.module == module && !Arc::ptr_eq(&state.generation, generation));
        if stale { self.reactive_armor_teardown(ship, module); }
        true
    }

    /// Install the operand profile at a paid admitted start, or reopen the
    /// closed window of the matching repeat. `seed` is `Some` only for an
    /// activation's first admitted cycle and installs the profile once; a
    /// repeat requires the existing generation, type and membership binding
    /// and reopens without resetting the current profile.
    pub(crate) fn begin_reactive_armor(&mut self, ship: i64, membership: u64, module: i64,
        module_type: i32, generation: &Arc<AtomicBool>, seed: Option<[f64; 4]>) -> bool {
        let Some(record) = self.actor_combat.get_mut(&ship) else { return false };
        if !matches!(record.phase, CombatPhase::Live) || record.membership_generation != membership {
            return false;
        }
        if record.fit.module_state(module) != Some((true, true))
            || record.fit.source_type(Key::Module(module)) != Some(module_type) { return false; }
        match record.reactive_armor.as_mut() {
            Some(state) => {
                if state.module != module || state.module_type != module_type
                    || !Arc::ptr_eq(&state.generation, generation) { return false; }
                if seed.is_some() { return false; }
                state.damage = [0.0; 4];
                state.window_open = true;
                let current = state.current;
                record.fit.set_reactive_armor(module, current);
            }
            None => {
                let Some(seed) = seed else { return false; };
                record.reactive_armor = Some(ReactiveArmorState {
                    module, module_type, generation: generation.clone(),
                    current: seed, damage: [0.0; 4], window_open: true,
                });
                record.fit.set_reactive_armor(module, seed);
            }
        }
        true
    }

    /// Synchronous end of one paid window: take the tally and close the
    /// window before any publication. An unchanged profile with no due
    /// character deadline opens no receipt. A real deadline uses the existing
    /// full Current receipt, and a changed profile rides in the same old-to-new
    /// batch (eight armor rows when no deadline is due). The profile is
    /// effective immediately; a closed window cannot be processed twice, and
    /// an ordinary manual stop keeps this paid final cycle legitimate.
    pub(crate) fn finish_reactive_armor_cycle(&mut self, ship: i64, membership: u64, module: i64,
        generation: &Arc<AtomicBool>, shift: f64) -> bool {
        let open = self.actor_combat.get(&ship).is_some_and(|record|
            matches!(record.phase, CombatPhase::Live)
                && record.membership_generation == membership
                && record.fit.module_state(module) == Some((true, true))
                && record.reactive_armor.as_ref().is_some_and(|state|
                    state.module == module && state.window_open
                    && Arc::ptr_eq(&state.generation, generation)
                    && record.fit.source_type(Key::Module(module)) == Some(state.module_type)));
        if !open { return false; }
        let next = self.actor_combat.get_mut(&ship).and_then(|record| {
            let state = record.reactive_armor.as_mut()?;
            if state.module != module || !state.window_open
                || !Arc::ptr_eq(&state.generation, generation) { return None; }
            let next = adapt(state.current, std::mem::take(&mut state.damage), shift);
            state.window_open = false;
            Some(next)
        });
        let Some(next) = next else { return false; };
        let unchanged = self.actor_combat.get(&ship)
            .and_then(|record| record.reactive_armor.as_ref())
            .is_some_and(|state| state.current == next);
        let expiring = self.actor_combat.get(&ship).is_some_and(|record|
            record.fit.next_character_expiry().is_some_and(|deadline| deadline <= crate::client::filetime_now()));
        if unchanged && !expiring { return true; }
        let source = if expiring { None } else { Some(FitDeltaSources::ReactiveArmor(module)) };
        let Some((expired, _, receipt)) = self.prepare_fit_publication(ship, source) else { return true; };
        let changed = !unchanged && self.actor_combat.get_mut(&ship).is_some_and(|record| {
            let Some(state) = record.reactive_armor.as_mut() else { return false; };
            if state.module != module || !Arc::ptr_eq(&state.generation, generation) { return false; }
            state.current = next;
            record.fit.set_reactive_armor(module, next)
        });
        if expired { self.rebase_fitted_outputs(ship); } else if changed { self.refresh_fitted_stats(ship); }
        if let Some(before) = receipt { self.finish_fit_delta(before, None); }
        true
    }

    /// Retire one generation's state and its operand overlay through the
    /// shared teardown. A stale generation's cleanup can never clear a
    /// successor, and repeated teardown is silent.
    pub(crate) fn retire_reactive_armor(&mut self, ship: i64, membership: u64, module: i64,
        generation: &Arc<AtomicBool>) {
        let current = self.actor_combat.get(&ship).is_some_and(|record|
            matches!(record.phase, CombatPhase::Live) && record.membership_generation == membership
                && record.reactive_armor.as_ref().is_some_and(|state|
                    state.module == module && Arc::ptr_eq(&state.generation, generation)));
        if !current { return; }
        self.reactive_armor_teardown(ship, module);
    }

    /// One actor snapshot of the running profile's wire values: the module
    /// id, its four operand values and the ship's four effective armor
    /// resonances through the single mutable fit gate. A stop-requested but
    /// not-yet-retired profile stays visible.
    pub(crate) fn reactive_armor_readout(&mut self, ship: i64) -> Option<(i64, [f64; 4], [f64; 4])> {
        let module = self.actor_combat.get(&ship)
            .filter(|record| matches!(record.phase, CombatPhase::Live))
            .and_then(|record| record.reactive_armor.as_ref())
            .map(|state| state.module)?;
        let readout = self.with_combat_fit(ship, move |fit, _, _| (
            module,
            ARMOR_RESONANCE.map(|attr| fit.value(Key::Module(module), attr)),
            ARMOR_RESONANCE.map(|attr| fit.value(Key::Ship, attr)),
        ))?;
        self.actor_combat.get(&ship)
            .filter(|record| matches!(record.phase, CombatPhase::Live))
            .and_then(|record| record.reactive_armor.as_ref())
            .filter(|state| state.module == module)
            .map(|_| readout)
    }
}

#[cfg(all(test, feature = "tests-space"))] mod tests;
