//! Actor-owned combat state: fitted defenses, live condition, contributions and
//! lifecycle tokens. Damage and repair are synchronous actor operations; database,
//! client and market work stay in their existing asynchronous callers. Cycles
//! borrow the resident fit through the single mutable input gate in `inputs`.
//! The adaptive armor hardener's transient cycle profile and accepted-hit
//! tally live in `reactive_armor`, bound to one fitted module, its activation
//! generation and the record's membership; it is never persisted.
mod facts;
mod fitting;
pub(crate) mod inputs;
pub(crate) mod loaded;
mod reactive_armor;
pub(crate) mod spool;
mod transition;
use spool::SpoolReadout;
use super::System;
use crate::services::combat::damage::DamageSource;
use crate::services::combat::groups_health::{recharge, Condition, ShipStats};
use crate::services::fit::{Fit, Key};
use crate::client::filetime_now;
use infinity_core::consts::{dogma::ATTR_DISALLOW_ASSISTANCE, FILETIME_SECOND};
use std::future::Future;
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CombatPhase {
    Live,
    Transition { token: u64 },
    Dying { token: u64 },
}
#[derive(Clone, Copy)]
pub(crate) enum RepairSource {
    Local,
    Remote(crate::space::combat_facts::CombatSource),
}


#[derive(Clone, Debug)]
pub(crate) struct ContributionEntry {
    pub attacker_id: i64,
    pub source_ship_id: Option<i64>,
    pub source_ship_type: Option<i32>,
    pub weapon_type_id: Option<i32>,
    pub damage: f64,
    pub last_damage_at: i64,
    pub illegal: bool,
    pub entity: crate::lua::events::combat::CombatEntity,
    pub exact_damage: crate::lua::events::combat::amount::ExactBinarySum,
    pub context: crate::lua::events::resources::FactContext,
}

#[derive(Clone, Debug)]
pub(crate) struct ActorConditionSnapshot {
    pub item_id: i64,
    pub generation: u64,
    pub revision: u64,
    pub saved_revision: u64,
    pub condition: Condition,
    pub contributions: Vec<ContributionEntry>,
    /// Dirty loaded rows captured under the same gate as this snapshot.
    pub loaded: Vec<crate::dao::items::charges::LoadedWrite>,
}

impl ActorConditionSnapshot {
    /// Validate the snapshot and destination fit before publishing a native transfer.
    pub(crate) fn valid_with_fit(&self, fit: &Fit, key: Key) -> bool {
        let stats = ShipStats::from_fit(fit, key);
        valid_condition(&self.condition)
            && valid_condition(&Condition { stats, now: [0.0; 3], shield_at: 0 })
    }

    /// Rebase a live snapshot onto a destination fit before native ball publication.
    pub(crate) fn fractions_with_fit(&self, fit: &Fit, key: Key) -> [f64; 3] {
        let stats = ShipStats::from_fit(fit, key);
        let at = filetime_now();
        let dt = at.saturating_sub(self.condition.shield_at).max(0) as f64 / FILETIME_SECOND as f64;
        let levels = [
            recharge(self.condition.now[0], self.condition.stats.max[0],
                self.condition.stats.shield_tau_ms, dt).min(stats.max[0]),
            stats.max[1] - (self.condition.stats.max[1] - self.condition.now[1]).max(0.0),
            stats.max[2] - (self.condition.stats.max[2] - self.condition.now[2]).max(0.0),
        ];
        [0, 1, 2].map(|i| if stats.max[i] > 0.0 {
            (levels[i] / stats.max[i]).clamp(0.0, 1.0)
        } else { 0.0 })
    }
}

#[derive(Clone, Debug)]
pub(crate) struct FirstLethalSnapshot {
    pub token: u64,
    pub item_id: i64,
    pub condition: Condition,
    pub contributions: Vec<ContributionEntry>,
    pub undo_contributions: Vec<ContributionEntry>,
    pub source: DamageSource,
    pub position: [f64; 3],
    pub identity: Option<i64>,
    pub owner_id: i64,
    pub corp_id: i64,
    pub is_drone: bool,
    pub is_npc: bool,
    pub expected_online_session: Option<infinity_core::session::OnlineSession>,
    pub kind: Option<i32>,
    pub lethal_at: i64,
    pub generation: u64,
    pub before_revision: u64,
    pub victim: crate::lua::events::combat::CombatEntity,
    pub pending_damage: Option<crate::space::combat_facts::PendingDamage>,
    pub victim_context: crate::lua::events::resources::FactContext,
    pub conflict: crate::lua::events::combat::ConflictKind,
}

#[derive(Clone, Debug)]
pub(crate) struct ActorDamageOutcome {
    pub dealt: f64,
    pub old: [f64; 3],
    pub new: [f64; 3],
    pub source: DamageSource,
    pub lethal: bool,
    pub first_lethal: Option<Arc<FirstLethalSnapshot>>,
}
pub(crate) struct DamagePlan {
    target: i64, victim: crate::lua::events::combat::CombatEntity,
    source: DamageSource, condition: Condition, old: [f64; 3], layers: [f64; 3],
    /// Accepted post-resistance typed armor damage for the reactive profile's
    /// open window; zero for flat and force-loss plans.
    armor: [f64; 4],
    dealt: f64, lethal: bool, force_loss: bool, at: i64, generation: u64, revision: u64,
    position: [f64; 3], identity: Option<i64>, kind: Option<i32>, owner_id: i64,
    corp_id: i64, is_drone: bool, is_npc: bool,
    exact_damage: Option<crate::lua::events::combat::amount::ExactBinarySum>,
}
pub(crate) struct DamageBatchEntry {
    plan: Option<DamagePlan>,
    pub outcome: Option<ActorDamageOutcome>,
}



#[derive(Clone, Debug)]
pub(crate) struct DirtyConditionCapture {
    pub item_id: i64,
    pub generation: u64,
    pub revision: u64,
    pub shield: f64,
    pub armor_damage: f64,
    pub hull_damage: f64,
    pub sampled_at: i64,
}

#[derive(Clone)]
pub(crate) enum PendingCombat {
    Dying { token: u64, generation: u64, snapshot: Arc<FirstLethalSnapshot>, in_flight: bool,
        npc_context: Option<Arc<crate::services::combat::groups_health::death::NpcDeathContext>>,
        player_context: Option<Arc<crate::services::ship::loss::facts::PlayerLossContext>> },
    Transition {
        token: u64,
        generation: u64,
        callback: Arc<dyn Fn() -> crate::space::npc::NpcFuture + Send + Sync>,
        in_flight: bool,
    },
}

pub(crate) struct ActorCombatRecord {
    pub(crate) condition: Condition,
    pub(crate) fit: Fit,
    pub(crate) fit_key: Key,
    pub(crate) fit_revision: u64,
    pub(crate) membership_generation: u64,
    /// Runtime-only replay activation, independent of membership generation.
    pub(crate) body_source_generation: Option<u64>,
    pub(crate) source_sequence: u64,
    pub(crate) revision: u64,
    pub(crate) saved_revision: u64,
    /// Armed character-deadline timer: (deadline FILETIME, token).
    pub(crate) character_timer: Option<(i64, u64)>,
    pub(crate) contributions: HashMap<i64, ContributionEntry>,
    pub(crate) phase: CombatPhase,
    pub(crate) pending: Option<PendingCombat>,
    /// Resident loaded charge slots keyed by module item id.
    pub(crate) loaded: HashMap<i64, loaded::LoadedSlot>,
    /// Presentation-only precursor-turret ramp readouts: notification and
    /// `ship_info` prime values only — damage never reads them, they are never
    /// persisted, and each entry dies with its activation generation.
    pub(crate) spool_readouts: Vec<SpoolReadout>,
    /// The running adaptive armor hardener's transient cycle state; never
    /// persisted, never snapshotted, dies with this record. See
    /// `reactive_armor.rs`.
    pub(crate) reactive_armor: Option<reactive_armor::ReactiveArmorState>,
}

pub(crate) fn valid_raw(raw: [f64; 4]) -> bool {
    raw.iter().all(|amount| amount.is_finite() && *amount >= 0.0)
}

pub(crate) fn valid_condition(condition: &Condition) -> bool {
    condition.shield_at >= 0
        && condition.stats.max.iter().all(|value| value.is_finite() && *value >= 0.0)
        && condition.stats.resonance.iter().flatten().all(|value| value.is_finite() && *value >= 0.0)
        && condition.stats.shield_tau_ms.is_finite() && condition.stats.shield_tau_ms >= 0.0
        && condition.now.iter().all(|value| value.is_finite())
        && condition.now[0] >= 0.0
        && condition.now.iter().zip(condition.stats.max).all(|(now, max)| *now <= max)
}

fn stats_equal(a: &ShipStats, b: &ShipStats) -> bool {
    a.max == b.max && a.resonance == b.resonance && a.shield_tau_ms == b.shield_tau_ms
}

/// Preserve absolute armor and hull damage while rebasing the shield anchor.
pub(crate) fn rebase_condition(condition: &mut Condition, stats: ShipStats) -> bool {
    rebase_condition_at(condition, stats, filetime_now())
}

fn rebase_condition_at(condition: &mut Condition, stats: ShipStats, at: i64) -> bool {
    if !valid_condition(&Condition { stats: stats.clone(), now: [0.0; 3], shield_at: 0 })
        || stats_equal(&condition.stats, &stats) { return false; }
    let dt = at.saturating_sub(condition.shield_at).max(0) as f64 / FILETIME_SECOND as f64;
    let shield = recharge(condition.now[0], condition.stats.max[0], condition.stats.shield_tau_ms, dt);
    let armor_damage = condition.stats.max[1] - condition.now[1];
    let hull_damage = condition.stats.max[2] - condition.now[2];
    condition.now = [shield.min(stats.max[0]), stats.max[1] - armor_damage, stats.max[2] - hull_damage];
    condition.stats = stats;
    condition.shield_at = at;
    true
}

/// Whether incoming remote assistance is refused for `ship`: an active
/// Entosis link on a capital-fitted hull, or any active fitted module with a
/// positive effective `disallowAssistance` value (SDE dgmAttributeTypes 854,
/// carried by warp disruption field generators). The second condition reads
/// the resident combat record only; a ship without one answers by the
/// Entosis condition alone.
pub(crate) fn assistance_blocked(system: &System, ship: i64) -> bool {
    super::sovereignty::links::assistance_blocked(system, ship)
        || system.actor_combat.get(&ship).is_some_and(|record| record.fit.fitted_modules().any(|(module, _)|
            record.fit.module_state(module).is_some_and(|(_, active)| active)
                && record.fit.value(Key::Module(module), ATTR_DISALLOW_ASSISTANCE) > 0.0))
}

impl ActorCombatRecord {
    fn prune_contributions(&mut self, since: i64) {
        self.contributions.retain(|_, entry| entry.last_damage_at >= since);
    }

    fn contribution_vec(&mut self, at: i64) -> Vec<ContributionEntry> {
        let window = crate::config::game().crimewatch.killmail_attacker_window_secs
            .saturating_mul(FILETIME_SECOND);
        self.prune_contributions(at.saturating_sub(window));
        self.contributions.values().cloned().collect()
    }
}

/// Which resonance table a damage application uses: the target's fitted
/// resonances, or none (Breacher pod boarding damage, client
/// dotWeapons/common/dotRegistry.py ApplyFlatDamage).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Absorption { Fitted, Flat }

/// Layer totals plus the armor layer's actual post-resistance damage, in raw
/// order (em, thermal, kinetic, explosive). Only the armor layer is typed;
/// shield and hull stay in `layers`.
pub(crate) struct AbsorbedDamage {
    pub(crate) layers: [f64; 3],
    pub(crate) armor: [f64; 4],
}

fn absorb_layers(condition: &mut Condition, raw: [f64; 4], resonance: [[f64; 4]; 3]) -> AbsorbedDamage {
    let mut left = raw;
    let mut dealt = [0.0; 3];
    let mut armor = [0.0; 4];
    for layer in 0..3 {
        let product: [f64; 4] = std::array::from_fn(|i| left[i] * resonance[layer][i]);
        let effective: f64 = product.iter().sum();
        if effective <= 0.0 { break; }
        let available = condition.now[layer].max(0.0);
        let taken = effective.min(available);
        if layer == 1 && effective > 0.0 {
            let scale = taken / effective;
            armor = std::array::from_fn(|i| product[i] * scale);
        }
        if effective <= available {
            condition.now[layer] = available - effective;
            dealt[layer] = effective;
            break;
        }
        dealt[layer] = available;
        let overflow = effective - available;
        if condition.now[layer] > 0.0 { condition.now[layer] = 0.0; }
        left = left.map(|value| value * (overflow / effective));
    }
    AbsorbedDamage { layers: dealt, armor }
}

pub(crate) fn absorb_preserving_overdamage(condition: &mut Condition, raw: [f64; 4]) -> AbsorbedDamage {
    absorb_layers(condition, raw, condition.stats.resonance)
}

/// Resistance-free spill through shield, armor and hull, preserving
/// overdamage into the next layer (Breacher pod boarding damage).
pub(crate) fn absorb_flat_preserving_overdamage(condition: &mut Condition, amount: f64) -> [f64; 3] {
    absorb_layers(condition, [amount, 0.0, 0.0, 0.0], [[1.0; 4]; 3]).layers
}

impl System {
    pub(crate) fn combat_world(&self) -> Option<Arc<crate::space::World>> {
        self.world.upgrade()
    }

    pub(crate) fn try_defer(&self, job: impl Future<Output = ()> + Send + 'static) -> bool {
        self.deferred_jobs.as_ref().is_some_and(|tx| tx.send(Box::pin(job)).is_ok())
    }

    pub(super) fn wake_pending_combat(&mut self) {
        let since = self.combat_fact_time().saturating_sub(
            crate::config::game().crimewatch.killmail_attacker_window_secs.saturating_mul(FILETIME_SECOND));
        for record in self.actor_combat.values_mut() {
            record.prune_contributions(since);
        }
        for (ship, pending) in self.claim_pending_combat() {
            match pending {
                PendingCombat::Transition { token, generation, callback, .. } => {
                    if !self.try_defer(callback()) {
                        self.release_pending_combat(ship, token, generation);
                    }
                }
                PendingCombat::Dying { token, generation, snapshot, .. } => {
                    let Some(db) = self.snapshot_context.as_ref().map(|context| context.db.clone()) else {
                        self.release_pending_combat(ship, token, generation);
                        continue;
                    };
                    let Some(world) = self.world.upgrade() else {
                        self.release_pending_combat(ship, token, generation);
                        continue;
                    };
                    let system = self.id;
                    if !self.try_defer(crate::services::combat::damage::reconcile_pending_loss(db, world, system, ship, snapshot)) {
                        self.release_pending_combat(ship, token, generation);
                    }
                }
            }
        }
    }

    pub(crate) fn install_condition(&mut self, ship: i64, condition: Condition, fit: Fit) {
        self.install_condition_with_key(ship, condition, fit, Key::Ship);
    }

    pub(crate) fn install_condition_with_key(&mut self, ship: i64, condition: Condition, mut fit: Fit, fit_key: Key) {
        let generation = self.persistent_generation(ship)
            .or_else(|| self.actor_combat.get(&ship).map(|record| record.membership_generation))
            .unwrap_or_else(crate::space::targeting::next_generation);
        if !valid_condition(&condition) { return; }
        if self.actor_combat.get(&ship).is_some_and(|record| record.has_loaded_obligations()) { return; }
        self.nativefact_clock = self.nativefact_clock.max(condition.shield_at);
        if self.actor_combat.get(&ship).is_some_and(|record| record.membership_generation != generation) {
            self.retire_body_combat_source(ship);
        }
        let (body_source_generation, source_sequence) = self.actor_combat.get(&ship)
            .filter(|record| record.membership_generation == generation)
            .map_or((None, 0), |record| (record.body_source_generation, record.source_sequence));
        let loaded = loaded::seed_map(fit.loaded_seeds());
        for source in self.runtime_sources_anchored(ship, self.ball(ship).and_then(|ball| ball.char_id)) {
            fit.replace_runtime_modifier_source(source);
        }
        let record = ActorCombatRecord {
            condition, fit, fit_key, fit_revision: 0, membership_generation: generation, body_source_generation, source_sequence,
            revision: 1, saved_revision: 1, character_timer: None, contributions: HashMap::new(),
            phase: CombatPhase::Live, pending: None, loaded, spool_readouts: Vec::new(),
            reactive_armor: None,
        };
        self.actor_combat.insert(ship, record);
        self.arm_character_expiry(ship);
    }

    /// Mark a just-installed cold seed whose fitted rebase has not been checkpointed.
    pub(crate) fn mark_condition_dirty(&mut self, ship: i64, generation: u64) -> bool {
        let Some(record) = self.actor_combat.get_mut(&ship) else { return false };
        if record.membership_generation != generation || !matches!(record.phase, CombatPhase::Live) { return false; }
        let Some(next) = record.revision.checked_add(1) else { return false };
        record.revision = next;
        true
    }

    pub(crate) fn remove_combat_state(&mut self, ship: i64) {
        self.retire_body_combat_source(ship);
        if !matches!(self.actor_combat.get(&ship).map(|r| r.phase),
            Some(CombatPhase::Dying { .. } | CombatPhase::Transition { .. }))
            && !self.actor_combat.get(&ship).is_some_and(|r| r.has_loaded_obligations()) {
            self.actor_combat.remove(&ship);
        }
    }

    pub(crate) fn ship_condition(&self, ship: i64) -> Option<(f64, f64, f64)> {
        let record = self.actor_combat.get(&ship)?;
        let now = filetime_now();
        let dt = now.saturating_sub(record.condition.shield_at).max(0) as f64 / FILETIME_SECOND as f64;
        let shield = recharge(record.condition.now[0], record.condition.stats.max[0],
            record.condition.stats.shield_tau_ms, dt);
        Some((shield, (record.condition.stats.max[1] - record.condition.now[1]).max(0.0),
            (record.condition.stats.max[2] - record.condition.now[2]).max(0.0)))
    }

    pub(crate) fn combat_health_scalars(&self, ship: i64) -> Option<([f64; 3], [f64; 3])> {
        let record = self.actor_combat.get(&ship)?;
        let at = filetime_now();
        let dt = at.saturating_sub(record.condition.shield_at).max(0) as f64 / FILETIME_SECOND as f64;
        let shield = recharge(record.condition.now[0], record.condition.stats.max[0], record.condition.stats.shield_tau_ms, dt);
        Some((record.condition.stats.max, [shield, record.condition.stats.max[1] - record.condition.now[1],
            record.condition.stats.max[2] - record.condition.now[2]].map(|value| value.max(0.0))))
    }

    pub(crate) fn combat_membership_generation(&self, ship: i64) -> Option<u64> {
        self.actor_combat.get(&ship).map(|record| record.membership_generation)
    }

    pub(crate) fn combat_condition_snapshot(&mut self, ship: i64) -> Option<ActorConditionSnapshot> {
        let at = self.combat_fact_time();
        let record = self.actor_combat.get_mut(&ship)?;
        Some(ActorConditionSnapshot {
            item_id: ship, generation: record.membership_generation, revision: record.revision,
            saved_revision: record.saved_revision,
            condition: record.condition.clone(),
            contributions: record.contribution_vec(at),
            loaded: record.loaded.values().filter(|slot| slot.dirty)
                .map(|slot| slot.write(ship)).collect(),
        })
    }
    pub(crate) fn is_damage_immune(&self, target: i64) -> bool { self.sovereignty.hub == Some(target) }

    pub(crate) fn apply_damage(&mut self, target: i64, raw: [f64; 4], source: DamageSource, force_loss: bool)
        -> Result<Option<ActorDamageOutcome>, crate::space::combat_facts::Refusal>
    {
        if force_loss && !self.is_damage_immune(target) { self.ensure_combat_facts(); }
        let facts = source.facts;
        let result = self.apply_damage_inner(target, raw, source, force_loss, Absorption::Fitted);
        if let Some(facts) = facts { self.finish_combat_attempt(&facts); }
        result
    }

    /// `apply_damage` sibling for resistance-free flat damage (Breacher pod
    /// boarding ticks); attempt admission and outcome handling are identical.
    pub(crate) fn apply_flat_damage(&mut self, target: i64, amount: f64, source: DamageSource)
        -> Result<Option<ActorDamageOutcome>, crate::space::combat_facts::Refusal>
    {
        let facts = source.facts;
        let result = self.apply_damage_inner(target, [amount, 0.0, 0.0, 0.0], source, false, Absorption::Flat);
        if let Some(facts) = facts { self.finish_combat_attempt(&facts); }
        result
    }

    fn apply_damage_inner(&mut self, target: i64, raw: [f64; 4], source: DamageSource, force_loss: bool, absorption: Absorption)
        -> Result<Option<ActorDamageOutcome>, crate::space::combat_facts::Refusal> {
        let at = { self.observe_combat_utc()?; self.combat_observed_time() };
        let Some(plan) = self.plan_damage(target, raw, source, force_loss, at, absorption) else { return Ok(None) };
        let pending_damage = if !force_loss && plan.dealt > 0.0 {
            let facts = self.combat_facts.as_mut().ok_or(crate::space::combat_facts::Refusal::Invalid)?;
            let mut reservation = facts.reserve_damage(plan.source.facts.ok_or(crate::space::combat_facts::Refusal::Invalid)?, &plan.victim, at)?;
            let result = facts.commit_damage(&mut reservation, &plan.victim, plan.layers, plan.lethal);
            facts.release(reservation);
            result?
        } else { None };
        Ok(self.commit_damage_plan(plan, pending_damage))
    }

    pub(crate) fn plan_area_damage(&self, target: i64, raw: [f64; 4], source: DamageSource, at: i64)
        -> Option<crate::space::combat_facts::reservation::ReservedTarget<DamageBatchEntry>> {
        let plan = self.plan_damage(target, raw, source, false, at, Absorption::Fitted)?;
        Some(crate::space::combat_facts::reservation::ReservedTarget::new(
            plan.victim, plan.layers, DamageBatchEntry { plan: Some(plan), outcome: None }))
    }

    pub(crate) fn apply_area_damage(&mut self, source: crate::space::combat_facts::CombatSource,
        targets: &mut [crate::space::combat_facts::reservation::ReservedTarget<DamageBatchEntry>],
        at: i64, guard: impl FnOnce(&System) -> bool) -> Result<bool, crate::space::combat_facts::Refusal> {
        if targets.is_empty() {
            self.finish_combat_attempt(&source);
            return Ok(false);
        }
        self.admit_area_source(source, targets, at, guard)
    }

    /// Shared source admission of one area pulse: observed clock and guard must
    /// still hold and every carried ship plan must match its live record. Ship
    /// unions claim through `reserve_damage_union`; a pulse carrying no ship
    /// plan performs the same subject and replay claim through a zero-damage
    /// reservation on the source entity, released at once. A refused, replayed
    /// or full pulse finishes the attempt and mutates nothing.
    pub(crate) fn admit_area_source(&mut self, source: crate::space::combat_facts::CombatSource,
        targets: &mut [crate::space::combat_facts::reservation::ReservedTarget<DamageBatchEntry>],
        at: i64, guard: impl FnOnce(&System) -> bool) -> Result<bool, crate::space::combat_facts::Refusal> {
        if at != self.nativefact_clock || !guard(self) || targets.iter().any(|target| target.payload.plan.as_ref().is_none_or(|plan|
            plan.at != at || plan.source.facts != Some(source) || plan.victim != target.target
                || plan.layers.map(f64::to_bits) != target.amounts.map(f64::to_bits) || self.actor_combat.get(&plan.target)
                .is_none_or(|record| record.revision != plan.revision || record.membership_generation != plan.generation
                    || !matches!(record.phase, CombatPhase::Live)))) {
            self.finish_combat_attempt(&source);
            return Ok(false);
        }
        let count = targets.len();
        let Some(mut facts) = self.combat_facts.take() else { self.finish_combat_attempt(&source); return Err(crate::space::combat_facts::Refusal::Invalid) };
        let result = if count == 0 {
            match facts.reserve_damage(source, &source.entity, at) {
                Ok(reservation) => { facts.release(reservation); Ok(true) }
                Err(error) => Err(error),
            }
        } else { match facts.reserve_damage_union(source, targets, at) {
          Ok(mut reservation) => {
            for index in 0..count {
                let plan = reservation.payload_mut(index).and_then(|entry| entry.plan.take()).expect("validated plan");
                let pending = facts.commit_damage(&mut reservation, &plan.victim, plan.layers, plan.lethal)
                    .expect("atomic union preflight");
                let outcome = self.commit_damage_plan(plan, pending).expect("unchanged actor plan");
                reservation.payload_mut(index).expect("reserved target").outcome = Some(outcome);
            }
            facts.release(reservation);
            Ok(true)
          }
          Err(error) => Err(error),
        } };
        facts.finish_attempt(&source);
        self.combat_facts = Some(facts);
        result
    }

    pub(crate) fn plan_damage(&self, target: i64, raw: [f64; 4], source: DamageSource,
        force_loss: bool, at: i64, absorption: Absorption) -> Option<DamagePlan> {
        if at != self.nativefact_clock || !valid_raw(raw) { return None; }
        if self.is_damage_immune(target) { return None; }
        let position = self.sample_position(target)?;
        if !position.iter().all(|value| value.is_finite()) { return None; }
        let ball = self.ball(target)?;
        let identity = ball.char_id;
        let kind = Some(ball.type_id);
        let owner_id = ball.owner_id;
        let corp_id = ball.corp_id;
        let is_drone = self.drone(target).is_some();
        let is_npc = self.is_npc(target) && !is_drone;
        let victim = self.combat_entity(target)?;
        if (!force_loss && source.facts.is_none()) || (force_loss && source.force_origin.is_none()) { return None; }
        if source.facts.is_some_and(|facts| facts.actual_actor_character.is_some() && facts.controller_entity().is_none()) { return None; }
        let record = self.actor_combat.get(&target)?;
        let generation = record.membership_generation;
        if !matches!(record.phase, CombatPhase::Live) || !valid_condition(&record.condition) { return None; }
        let revision = record.revision;
        revision.checked_add(1)?;
        let mut condition = record.condition.clone();
        let dt = at.saturating_sub(condition.shield_at).max(0) as f64 / FILETIME_SECOND as f64;
        condition.now[0] = recharge(condition.now[0], condition.stats.max[0],
            condition.stats.shield_tau_ms, dt);
        condition.shield_at = at;
        let old = condition.now;
        let (layers, armor) = if force_loss {
            condition.now[2] = 0.0;
            ([0.0; 3], [0.0; 4])
        } else {
            match absorption {
                Absorption::Fitted => {
                    let absorbed = absorb_preserving_overdamage(&mut condition, raw);
                    (absorbed.layers, absorbed.armor)
                }
                Absorption::Flat => (absorb_flat_preserving_overdamage(&mut condition, raw[0]), [0.0; 4]),
            }
        };
        let dealt = layers.iter().sum::<f64>();
        if !dealt.is_finite() || !valid_condition(&condition) { return None; }
        let lethal = force_loss || (dealt > 0.0 && condition.now[2] <= 0.0);
        let mut exact_damage = source.facts.and_then(|facts| facts.actual_actor_character).map(|attacker| {
            let window = crate::config::game().crimewatch.killmail_attacker_window_secs.saturating_mul(FILETIME_SECOND);
            record.contributions.get(&attacker).filter(|entry| entry.last_damage_at >= at.saturating_sub(window))
                .map(|entry| entry.exact_damage.clone()).unwrap_or_default()
        });
        if let Some(sum) = exact_damage.as_mut() {
            for amount in layers { if amount > 0.0 && !sum.try_add(amount) { return None; } }
        }
        Some(DamagePlan { target, victim, source, condition, old, layers, armor, dealt, lethal, force_loss, at,
            generation, revision, position, identity, kind, owner_id, corp_id, is_drone, is_npc, exact_damage })
    }

    fn commit_damage_plan(&mut self, plan: DamagePlan,
        pending_damage: Option<crate::space::combat_facts::PendingDamage>) -> Option<ActorDamageOutcome> {
        let DamagePlan { target, victim, source, condition, old, layers: _, armor, dealt, lethal, force_loss, at,
            generation, revision, position, identity, kind, owner_id, corp_id, is_drone, is_npc, exact_damage } = plan;
        if self.actor_combat.get(&target).is_none_or(|record| record.revision != revision
            || record.membership_generation != generation || !matches!(record.phase, CombatPhase::Live)) { return None; }
        let record = self.actor_combat.get_mut(&target)?;
        let before_revision = record.revision;
        let pre_contributions = lethal.then(|| record.contribution_vec(at));
        if dealt > 0.0 {
            if let Some(attacker) = source.facts.and_then(|facts| facts.actual_actor_character) {
                let window = crate::config::game().crimewatch.killmail_attacker_window_secs.saturating_mul(FILETIME_SECOND);
                if record.contributions.get(&attacker).is_some_and(|entry| entry.last_damage_at < at.saturating_sub(window)) {
                    record.contributions.remove(&attacker);
                }
                let entry = record.contributions.entry(attacker).or_insert(ContributionEntry {
                    attacker_id: attacker, source_ship_id: source.source_ship_id,
                    source_ship_type: source.source_ship_type, weapon_type_id: source.weapon_type_id,
                    damage: 0.0, last_damage_at: at, illegal: false,
                    entity: source.facts?.entity, exact_damage: Default::default(),
                    context: source.facts?.context,
                });
                entry.damage += dealt;
                entry.last_damage_at = at;
                entry.source_ship_id = source.source_ship_id;
                entry.source_ship_type = source.source_ship_type;
                entry.weapon_type_id = source.weapon_type_id;
                entry.illegal |= source.illegal;
                entry.entity = source.facts?.entity;
                entry.context = source.facts?.context;
                entry.exact_damage = exact_damage?;
            }
            record.revision = revision + 1;
        } else if force_loss {
            record.revision = revision + 1;
        }
        record.condition = condition;
        let first_lethal = if lethal {
            let expected_online_session = identity.and_then(|character| self.registry.online_session(character))
                .filter(|session| session.ship_id == Some(target) && session.system_id == Some(self.id) && session.station_id.is_none());
            let token = crate::space::targeting::next_generation();
            let origin = source.facts.map(|facts| facts.context.origin).or(source.force_origin)?;
            let victim_context = crate::lua::events::resources::FactContext {
                corporation_id: victim.corporation_id, alliance_id: victim.alliance_id, faction_id: victim.faction_id,
                ship_id: Some(victim.item_id), ship_type_id: Some(victim.type_id),
                system_id: (self.id > 0).then_some(self.id), station_id: None, origin,
            };
            let conflict = match source.facts {
                Some(facts) if facts.source_is_npc => crate::lua::events::combat::ConflictKind::Npc,
                Some(facts) if identity.is_some() && facts.actual_actor_character.is_some() && facts.actual_actor_character != identity =>
                    crate::lua::events::combat::ConflictKind::PlayerVersusPlayer,
                _ => crate::lua::events::combat::ConflictKind::Unknown,
            };
            let snapshot = Arc::new(FirstLethalSnapshot {
                token, item_id: target,
                condition: Condition { stats: record.condition.stats.clone(), now: old, shield_at: at },
                contributions: record.contribution_vec(at),
                undo_contributions: pre_contributions.unwrap_or_default(),
                source: source.clone(), position, identity, owner_id, corp_id, is_drone, is_npc,
                expected_online_session, kind, lethal_at: at, generation,
                before_revision,
                victim, pending_damage,
                victim_context, conflict,
            });
            record.phase = CombatPhase::Dying { token };
            record.pending = Some(PendingCombat::Dying { token, generation, snapshot: Arc::clone(&snapshot), in_flight: true, npc_context: None, player_context: None });
            Some(snapshot)
        } else { None };
        // Accepted nonlethal typed armor damage tallies into the running
        // hardener's open cycle window; lethal, force-loss and refused plans
        // leave the tally untouched.
        if !lethal && !force_loss {
            if let Some(state) = record.reactive_armor.as_mut() { state.observe(armor); }
        }
        Some(ActorDamageOutcome { dealt, old, new: record.condition.now,
            source, lethal, first_lethal })
    }

    pub(crate) fn repair_condition(&mut self, target: i64, layer: usize, amount: f64, source: RepairSource) -> Result<bool, crate::space::combat_facts::Refusal> {
        let result = self.repair_condition_inner(target, layer, amount, source);
        if let RepairSource::Remote(facts) = source { self.finish_combat_attempt(&facts); }
        result
    }

    fn repair_condition_inner(&mut self, target: i64, layer: usize, amount: f64, source: RepairSource) -> Result<bool, crate::space::combat_facts::Refusal> {
        if layer >= 3 || !amount.is_finite() || amount <= 0.0 { return Ok(false); }
        let at = if matches!(source, RepairSource::Remote(_)) { self.observe_combat_utc()?; self.combat_observed_time() } else { self.combat_fact_time() };
        let Some(record) = self.actor_combat.get(&target) else { return Ok(false) };
        if !matches!(record.phase, CombatPhase::Live) { return Ok(false); }
        let Some(next_revision) = record.revision.checked_add(1) else { return Ok(false) };
        let mut condition = record.condition.clone();
        let dt = at.saturating_sub(condition.shield_at).max(0) as f64 / FILETIME_SECOND as f64;
        condition.now[0] = recharge(condition.now[0], condition.stats.max[0], condition.stats.shield_tau_ms, dt);
        condition.shield_at = at;
        let old = condition.now[layer];
        condition.now[layer] = (old + amount).min(condition.stats.max[layer]);
        let accepted = condition.now[layer] - old;
        if let RepairSource::Remote(source) = source {
            if accepted <= 0.0 {
                self.finish_combat_attempt(&source);
                return Ok(false);
            }
            let Some(victim) = self.combat_entity(target) else { return Ok(false) };
            let facts = self.combat_facts.as_mut().ok_or(crate::space::combat_facts::Refusal::Invalid)?;
            let hp_layer = [crate::lua::events::combat::HpLayer::Shield, crate::lua::events::combat::HpLayer::Armor,
                crate::lua::events::combat::HpLayer::Hull][layer];
            let mut reservation = facts.reserve_remote_repair(source, &victim, hp_layer, at)?;
            let result = facts.commit_remote_repair(&mut reservation, &victim, hp_layer, accepted);
            facts.release(reservation);
            result?;
        }
        let record = self.actor_combat.get_mut(&target).expect("condition retained in actor turn");
        record.condition = condition;
        if accepted <= 0.0 { return Ok(false); }
        record.revision = next_revision;
        Ok(true)
    }

    pub(crate) fn set_condition_fraction(&mut self, target: i64, fraction: f64) -> bool {
        self.set_condition_fraction_mask(target, 0b111, fraction)
    }

    pub(crate) fn set_condition_fraction_mask(&mut self, target: i64, mask: u8, fraction: f64) -> bool {
        if !fraction.is_finite() { return false; }
        let at = self.combat_fact_time();
        let fraction = fraction.clamp(0.0, 1.0);
        let Some(record) = self.actor_combat.get_mut(&target) else { return false };
        if !matches!(record.phase, CombatPhase::Live) { return false; }
        let Some(next_revision) = record.revision.checked_add(1) else { return false };
        let dt = at.saturating_sub(record.condition.shield_at).max(0) as f64 / FILETIME_SECOND as f64;
        record.condition.now[0] = recharge(record.condition.now[0], record.condition.stats.max[0], record.condition.stats.shield_tau_ms, dt);
        record.condition.shield_at = at;
        let old = record.condition.now;
        let max = record.condition.stats.max;
        record.condition.now = crate::services::combat::groups_health::set_damage_layers(
            old, max, max, [0, 1, 2].map(|layer| mask & (1 << layer) != 0), 1.0 - fraction);
        if old == record.condition.now { return false; }
        record.revision = next_revision;
        true
    }

    pub(crate) fn capture_dirty_condition(&self, ship: i64, persistent: bool, at: i64) -> Option<DirtyConditionCapture> {
        let record = self.actor_combat.get(&ship)?;
        if !persistent || at != self.nativefact_clock { return None; }
        let (condition, revision, sampled_at) = match record.pending.as_ref() {
            Some(PendingCombat::Dying { snapshot, .. }) =>
                (&snapshot.condition, snapshot.before_revision, snapshot.lethal_at),
            _ => (&record.condition, record.revision, at),
        };
        if revision == record.saved_revision { return None; }
        let dt = sampled_at.saturating_sub(condition.shield_at).max(0) as f64 / FILETIME_SECOND as f64;
        let shield = recharge(condition.now[0], condition.stats.max[0], condition.stats.shield_tau_ms, dt);
        Some(DirtyConditionCapture { item_id: ship, generation: record.membership_generation, revision,
            shield, armor_damage: condition.stats.max[1] - condition.now[1],
            hull_damage: condition.stats.max[2] - condition.now[2], sampled_at })
    }

    pub(crate) fn acknowledge_condition(&mut self, ship: i64, generation: u64, revision: u64) -> bool {
        let Some(record) = self.actor_combat.get_mut(&ship) else { return false };
        if record.membership_generation != generation || record.revision != revision || !matches!(record.phase, CombatPhase::Live) { return false; }
        record.saved_revision = revision;
        true
    }

    pub(crate) fn combat_phase(&self, ship: i64) -> Option<CombatPhase> {
        self.actor_combat.get(&ship).map(|record| record.phase)
    }

    /// Check the existing session registry without holding its guard during actor mutation.
    pub(crate) fn combat_session_current(&self, character: i64, expected: Option<&infinity_core::session::OnlineSession>) -> bool {
        self.registry.replace_online_session(character, expected, expected.cloned())
    }

    pub(crate) fn combat_session_id(&self, character: i64) -> Option<i64> {
        self.registry.online_session(character).map(|session| session.session_id)
    }

}

#[cfg(all(test, any(feature = "tests-combat", feature = "tests-effects", feature = "tests-services", feature = "tests-ships", feature = "tests-slash", feature = "tests-space")))] pub(crate) mod tests;
