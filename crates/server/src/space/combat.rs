//! Actor-owned combat state: fitted defenses, live condition, contributions and
//! lifecycle tokens. Damage and repair are synchronous actor operations; database,
//! client and market work stay in their existing asynchronous callers.
mod facts;
mod fitting;
use super::System;
use crate::services::combat::damage::DamageSource;
use crate::services::combat::groups_health::{recharge, Condition, ShipStats};
use crate::services::fit::{Fit, Key};
use crate::client::filetime_now;
use infinity_core::consts::FILETIME_SECOND;
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
    pub(crate) source_sequence: u64,
    pub(crate) revision: u64,
    pub(crate) saved_revision: u64,
    pub(crate) contributions: HashMap<i64, ContributionEntry>,
    pub(crate) phase: CombatPhase,
    pub(crate) pending: Option<PendingCombat>,
}

fn valid_raw(raw: [f64; 4]) -> bool {
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

fn absorb_preserving_overdamage(condition: &mut Condition, raw: [f64; 4]) -> [f64; 3] {
    let mut left = raw;
    let mut dealt = [0.0; 3];
    for layer in 0..3 {
        let effective: f64 = (0..4).map(|i| left[i] * condition.stats.resonance[layer][i]).sum();
        if effective <= 0.0 { break; }
        let available = condition.now[layer].max(0.0);
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
    dealt
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

    pub(crate) fn install_condition_with_key(&mut self, ship: i64, condition: Condition, fit: Fit, fit_key: Key) {
        let generation = self.persistent_generation(ship)
            .or_else(|| self.actor_combat.get(&ship).map(|record| record.membership_generation))
            .unwrap_or_else(crate::space::targeting::next_generation);
        if !valid_condition(&condition) { return; }
        self.nativefact_clock = self.nativefact_clock.max(condition.shield_at);
        let source_sequence = self.actor_combat.get(&ship)
            .filter(|record| record.membership_generation == generation).map_or(0, |record| record.source_sequence);
        let record = ActorCombatRecord {
            condition, fit, fit_key, fit_revision: 0, membership_generation: generation, source_sequence,
            revision: 1, saved_revision: 1, contributions: HashMap::new(),
            phase: CombatPhase::Live, pending: None,
        };
        self.actor_combat.insert(ship, record);
    }

    /// Mark a just-installed cold seed whose fitted rebase has not been checkpointed.
    pub(crate) fn mark_condition_dirty(&mut self, ship: i64, generation: u64) -> bool {
        let Some(record) = self.actor_combat.get_mut(&ship) else { return false };
        if record.membership_generation != generation || !matches!(record.phase, CombatPhase::Live) { return false; }
        let Some(next) = record.revision.checked_add(1) else { return false };
        record.revision = next;
        true
    }

    pub(crate) fn refresh_fitted_stats(&mut self, ship: i64) -> bool {
        let at = self.combat_fact_time();
        let Some(record) = self.actor_combat.get_mut(&ship) else { return false };
        if !matches!(record.phase, CombatPhase::Live) { return false; }
        let Some(next) = record.revision.checked_add(1) else { return false };
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

    /// Returns whether projections were replaced; false for missing, non-live or revision-exhausted records.
    pub(crate) fn replace_fitted_projections(&mut self, ship: i64,
        projections: Vec<crate::services::effects::projection::Projection>, revision: Option<u64>) -> bool {
        let Some(record) = self.actor_combat.get_mut(&ship) else { return false };
        if !matches!(record.phase, CombatPhase::Live) || record.revision == u64::MAX { return false; }
        let key = record.fit_key;
        record.fit.replace_projections_for(key, projections, revision);
        record.fit_revision = record.fit_revision.saturating_add(1);
        self.refresh_fitted_stats(ship);
        true
    }

    pub(crate) fn fitted_module_online(&mut self, ship: i64, module: i64, online: bool) -> bool {
        let Some(record) = self.actor_combat.get_mut(&ship) else { return false };
        if !matches!(record.phase, CombatPhase::Live) || record.revision == u64::MAX || !record.fit.set_module_online(module, online) { return false; }
        record.fit_revision = record.fit_revision.saturating_add(1);
        self.refresh_fitted_stats(ship)
    }

    pub(crate) fn fitted_module_active(&mut self, ship: i64, module: i64, active: bool) -> bool {
        let Some(record) = self.actor_combat.get_mut(&ship) else { return false };
        if !matches!(record.phase, CombatPhase::Live) || record.revision == u64::MAX || !record.fit.set_module_active(module, active) { return false; }
        record.fit_revision = record.fit_revision.saturating_add(1);
        self.refresh_fitted_stats(ship)
    }

    pub(crate) fn fitted_module_overloaded(&mut self, ship: i64, module: i64, overloaded: bool) -> bool {
        let Some(record) = self.actor_combat.get_mut(&ship) else { return false };
        if !matches!(record.phase, CombatPhase::Live) || record.revision == u64::MAX || !record.fit.set_module_overloaded(module, overloaded) { return false; }
        record.fit_revision = record.fit_revision.saturating_add(1);
        self.refresh_fitted_stats(ship)
    }

    pub(crate) fn combat_fit_revision(&self, ship: i64) -> Option<u64> {
        self.actor_combat.get(&ship).map(|record| record.fit_revision)
    }


    pub(crate) fn remove_combat_state(&mut self, ship: i64) {
        if !matches!(self.actor_combat.get(&ship).map(|r| r.phase),
            Some(CombatPhase::Dying { .. } | CombatPhase::Transition { .. })) {
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
        })
    }
    pub(crate) fn is_damage_immune(&self, target: i64) -> bool { self.sovereignty.hub == Some(target) }

    pub(crate) fn apply_damage(&mut self, target: i64, raw: [f64; 4], source: DamageSource, force_loss: bool)
        -> Result<Option<ActorDamageOutcome>, crate::space::combat_facts::Refusal>
    {
        if force_loss && !self.is_damage_immune(target) { self.ensure_combat_facts(); }
        let facts = source.facts;
        let result = self.apply_damage_inner(target, raw, source, force_loss);
        if let Some(facts) = facts { self.finish_combat_attempt(&facts); }
        result
    }

    fn apply_damage_inner(&mut self, target: i64, raw: [f64; 4], source: DamageSource, force_loss: bool)
        -> Result<Option<ActorDamageOutcome>, crate::space::combat_facts::Refusal> {
        let at = { self.observe_combat_utc()?; self.combat_observed_time() };
        let Some(plan) = self.plan_damage(target, raw, source, force_loss, at) else { return Ok(None) };
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
        let plan = self.plan_damage(target, raw, source, false, at)?;
        Some(crate::space::combat_facts::reservation::ReservedTarget::new(
            plan.victim, plan.layers, DamageBatchEntry { plan: Some(plan), outcome: None }))
    }

    pub(crate) fn apply_area_damage(&mut self, source: crate::space::combat_facts::CombatSource,
        targets: &mut [crate::space::combat_facts::reservation::ReservedTarget<DamageBatchEntry>],
        at: i64, guard: impl FnOnce(&System) -> bool) -> Result<bool, crate::space::combat_facts::Refusal> {
        if targets.is_empty() || at != self.nativefact_clock || !guard(self) || targets.iter().any(|target| target.payload.plan.as_ref().is_none_or(|plan|
            plan.at != at || plan.source.facts != Some(source) || plan.victim != target.target
                || plan.layers.map(f64::to_bits) != target.amounts.map(f64::to_bits) || self.actor_combat.get(&plan.target)
                .is_none_or(|record| record.revision != plan.revision || record.membership_generation != plan.generation
                    || !matches!(record.phase, CombatPhase::Live)))) {
            self.finish_combat_attempt(&source);
            return Ok(false);
        }
        let count = targets.len();
        let Some(mut facts) = self.combat_facts.take() else { self.finish_combat_attempt(&source); return Err(crate::space::combat_facts::Refusal::Invalid) };
        let result = match facts.reserve_damage_union(source, targets, at) {
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
        };
        facts.finish_attempt(&source);
        self.combat_facts = Some(facts);
        result
    }

    pub(crate) fn plan_damage(&self, target: i64, raw: [f64; 4], source: DamageSource,
        force_loss: bool, at: i64) -> Option<DamagePlan> {
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
        let layers = if force_loss {
            condition.now[2] = 0.0;
            [0.0; 3]
        } else {
            absorb_preserving_overdamage(&mut condition, raw)
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
        Some(DamagePlan { target, victim, source, condition, old, layers, dealt, lethal, force_loss, at,
            generation, revision, position, identity, kind, owner_id, corp_id, is_drone, is_npc, exact_damage })
    }

    fn commit_damage_plan(&mut self, plan: DamagePlan,
        pending_damage: Option<crate::space::combat_facts::PendingDamage>) -> Option<ActorDamageOutcome> {
        let DamagePlan { target, victim, source, condition, old, layers: _, dealt, lethal, force_loss, at,
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

    pub(crate) fn begin_combat_transition(&mut self, ship: i64) -> Option<(u64, ActorConditionSnapshot)> {
        let snapshot = self.combat_condition_snapshot(ship)?;
        let record = self.actor_combat.get_mut(&ship)?;
        if !matches!(record.phase, CombatPhase::Live) { return None; }
        let token = crate::space::targeting::next_generation();
        record.phase = CombatPhase::Transition { token };
        Some((token, snapshot))
    }

    fn republish_thawed_condition(&mut self, ship: i64) {
        self.refresh_fitted_stats(ship);
        let Some((max, now)) = self.combat_health_scalars(ship) else { return };
        let health = [0, 1, 2].map(|i| if max[i] > 0.0 {
            if i == 0 { (now[i] / max[i]).clamp(0.0, 1.0) }
            else { (1.0 - now[i] / max[i]).clamp(0.0, 1.0) }
        } else { 0.0 });
        if self.ball(ship).is_some_and(|ball| ball.health != health) {
            self.set_health(ship, health);
        }
    }

    pub(crate) fn cancel_combat_transition(&mut self, ship: i64, token: u64, generation: u64) -> bool {
        let Some(record) = self.actor_combat.get_mut(&ship) else { return false };
        if record.membership_generation != generation || record.phase != (CombatPhase::Transition { token }) { return false; }
        if matches!(record.pending, Some(PendingCombat::Transition { token: pending_token, .. }) if pending_token == token) {
            record.pending = None;
        }
        record.phase = CombatPhase::Live;
        self.republish_thawed_condition(ship);
        true
    }

    pub(crate) fn commit_combat_transition(&mut self, ship: i64, token: u64) -> bool {
        let Some(record) = self.actor_combat.get(&ship) else { return false };
        if record.phase != (CombatPhase::Transition { token }) { return false; }
        self.actor_combat.remove(&ship);
        true
    }

    pub(crate) fn transfer_combat_state(&mut self, ship: i64, destination_generation: u64,
        snapshot: ActorConditionSnapshot, fit: Fit, fit_key: Key) -> bool {
        if self.persistent_generation(ship) != Some(destination_generation)
            || snapshot.item_id != ship || !valid_condition(&snapshot.condition) { return false; }
        let condition = snapshot.condition;
        self.nativefact_clock = self.nativefact_clock.max(condition.shield_at);
        self.actor_combat.insert(ship, ActorCombatRecord {
            condition, fit, fit_key, fit_revision: 0, membership_generation: destination_generation,
            source_sequence: 0,
            revision: snapshot.revision, saved_revision: snapshot.saved_revision,
            contributions: snapshot.contributions.into_iter().map(|e| (e.attacker_id, e)).collect(),
            phase: CombatPhase::Live, pending: None,
        });
        self.refresh_fitted_stats(ship);
        true
    }


    pub(crate) fn mark_pending_transition(
        &mut self,
        ship: i64,
        token: u64,
        generation: u64,
        callback: Arc<dyn Fn() -> crate::space::npc::NpcFuture + Send + Sync>,
    ) -> bool {
        let Some(record) = self.actor_combat.get_mut(&ship) else { return false };
        if record.membership_generation != generation || record.phase != (CombatPhase::Transition { token }) { return false; }
        record.pending = Some(PendingCombat::Transition { token, generation, callback, in_flight: true });
        true
    }

    pub(crate) fn release_pending_combat(&mut self, ship: i64, token: u64, generation: u64) -> bool {
        let Some(record) = self.actor_combat.get_mut(&ship) else { return false };
        let Some(pending) = record.pending.as_mut() else { return false };
        let (stored_token, stored_generation, in_flight) = match pending {
            PendingCombat::Dying { token, generation, in_flight, .. }
            | PendingCombat::Transition { token, generation, in_flight, .. } => (token, generation, in_flight),
        };
        if *stored_token != token || *stored_generation != generation { return false; }
        *in_flight = false;
        true
    }

    pub(crate) fn claim_pending_combat(&mut self) -> Vec<(i64, PendingCombat)> {
        self.actor_combat.iter_mut().filter_map(|(&ship, record)| {
            let pending = record.pending.as_mut()?;
            let in_flight = match pending {
                PendingCombat::Dying { in_flight, .. } | PendingCombat::Transition { in_flight, .. } => in_flight,
            };
            if *in_flight { return None; }
            *in_flight = true;
            Some((ship, pending.clone()))
        }).collect()
    }

    pub(crate) fn restore_known_rollback(&mut self, ship: i64, token: u64, generation: u64) -> bool {
        if self.ball(ship).is_none() || self.persistent_generation(ship).is_some_and(|current| current != generation) { return false; }
        let Some(record) = self.actor_combat.get_mut(&ship) else { return false };
        if record.membership_generation != generation || record.phase != (CombatPhase::Dying { token }) { return false; }
        let Some(next_revision) = record.revision.checked_add(1) else { return false };
        let Some(PendingCombat::Dying { snapshot, .. }) = record.pending.take() else { return false };
        record.condition = snapshot.condition.clone();
        record.contributions = snapshot.undo_contributions.clone().into_iter().map(|e| (e.attacker_id, e)).collect();
        record.revision = next_revision;
        record.saved_revision = record.saved_revision.min(record.revision.saturating_sub(1));
        record.phase = CombatPhase::Live;
        self.republish_thawed_condition(ship);
        true
    }

    pub(crate) fn finalize_dying(&mut self, ship: i64, token: u64, generation: u64) -> bool {
        let Some(record) = self.actor_combat.get(&ship) else { return false };
        if record.membership_generation != generation || record.phase != (CombatPhase::Dying { token }) { return false; }
        self.actor_combat.remove(&ship);
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

#[cfg(test)] pub(crate) mod tests;
