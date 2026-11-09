//! Actor-owned NPC decisions and spawn groups, mirroring entity targeting and
//! destiny Orbit/Stop events. Combat attributes are loaded by services from SDE;
//! Weapon, repair and projected effects use a sequential worker outside the actor.
//! Projected cycles have independent actor timers and capture sampled positions;
//! process-unique target generations prevent queued jobs from applying to an
//! obsolete target or NPC incarnation. Think intervals and AI target-loss range use
//! combat configuration; projected scan resolution and dogma target range refresh
//! separately under actor revisions. ECM immediately removes locks and prevents
//! targeting and offensive actions until the jam expires.
//! Brains and groups are transient; persistent NPC simulation is not supported.
use super::*;
use std::collections::HashSet;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};

static NEXT_GROUP: AtomicU64 = AtomicU64::new(1);

/// Process-local NPC effect identities occupy the negative, non-item ID namespace.
static NEXT_NPC_MODULE: AtomicI64 = AtomicI64::new(-1);

use super::targeting::next_generation as next_npc_target_generation;

/// Server NPC targeting policy: sentinel owner permitting targets among all pilots.
pub const HOSTILE_TO_ALL: i64 = -1;
pub type NpcFuture = Pin<Box<dyn Future<Output = ()> + Send>>;
/// Admit on the actor after brains are restored; futures retain only Weak<World>.
pub type NpcEffects = Arc<dyn Fn(&mut System, NpcAction) -> Option<NpcFuture> + Send + Sync>;

pub enum NpcAction {
    Shot { me: Ball, victim: Ball, generation: u64 },
    Effect { me: Ball, victim: Ball, effect_id: i32, source_module: i64, duration_ms: f64, generation: u64 },
    Repair,
}

pub struct NpcConfig {
    pub fly_range: f64,
    pub aggro_range: f64,
    pub rate_of_fire_ms: f64,
    pub scan_resolution: f64,
    /// Shield amount, armor amount, cycle in milliseconds.
    pub repairs: (f64, f64, f64),
}

pub struct NpcBrain {
    pub owner: Option<i64>,
    /// Ship, pilot, lock completion in Unix milliseconds.
    pub target: Option<(i64, i64, i64)>,
    pub announced: bool,
    pub next_shot_ms: i64,
    pub next_repair_ms: i64,
    pub next_think_ms: i64,
    pub config: NpcConfig,
    /// Dogma targeting limit, independent of the AI's acquisition policy.
    pub(crate) max_target_range: Option<f64>,
    effects: NpcEffects,
    modules: Vec<NpcModule>,
    target_generation: u64,
    aggressors: HashSet<i64>,
}

struct NpcModule {
    effect_id: i32,
    source_module: i64,
    duration_ms: f64,
}
impl NpcBrain {
    pub fn new(owner: Option<i64>, config: NpcConfig, effects: NpcEffects) -> Self {
        Self { owner, config, effects, target: None, announced: false,
            next_shot_ms: 0, next_repair_ms: 0, next_think_ms: 0,
            max_target_range: None, aggressors: HashSet::new(), modules: Vec::new(), target_generation: next_npc_target_generation() }
    }
    /// Register an SDE effect instance with a process-unique negative module ID.
    pub(crate) fn add_effect(&mut self, effect_id: i32, duration_ms: f64) {
        if !duration_ms.is_finite() || (duration_ms < 1.0 && duration_ms != -1.0) { return; }
        let source_module = NEXT_NPC_MODULE.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_sub(1))
            .expect("NPC effect identity space exhausted");
        self.modules.push(NpcModule { effect_id, source_module, duration_ms });
    }
}

pub(super) enum EffectJob {
    Npc { effects: NpcEffects, action: NpcAction },
    Drone { effects: drone::DroneEffects, action: drone::DroneAction },
}
pub(super) fn start_worker(system: &mut System) {
    let (tx, mut rx) = mpsc::unbounded_channel::<NpcFuture>();
    let (deferred_tx, mut deferred_rx) = mpsc::unbounded_channel::<NpcFuture>();
    system.effect_jobs = Some(tx);
    system.deferred_jobs = Some(deferred_tx);
    let worker = tokio::spawn(async move {
        while let Some(job) = rx.recv().await { job.await; }
    });
    tokio::spawn(async move {
        if let Err(error) = worker.await { tracing::warn!(%error, "space NPC worker failed"); }
    });
    tokio::spawn(async move {
        let mut tasks = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                job = deferred_rx.recv() => match job {
                    Some(job) => { tasks.spawn(job); }
                    None => break,
                },
                result = tasks.join_next(), if !tasks.is_empty() => {
                    if let Some(Err(error)) = result { tracing::warn!(%error, "space deferred job failed"); }
                }
            }
        }
        while let Some(result) = tasks.join_next().await {
            if let Err(error) = result { tracing::warn!(%error, "space deferred job failed"); }
        }
    });
}

pub(super) struct SpawnGroup {
    members: HashSet<i64>,
    on_cleared: Box<dyn FnOnce(&mut System) + Send>,
}

impl System {
    /// Attach only to a live ball. First think is staggered over the configured interval.
    pub fn add_npc(&mut self, id: i64, mut brain: NpcBrain) -> bool {
        if !self.contains(id) { return false; }
        if self.actor_combat.get(&id).is_some_and(|record| matches!(record.phase, super::combat::CombatPhase::Dying { .. })) {
            self.actor_combat.remove(&id);
        }
        if !self.actor_combat.contains_key(&id) {
            let Some(sde) = self.projection_sde.clone() else { return false };
            let fit = crate::services::fit::Fit::for_space_type(sde, self.ball(id).map(|ball| ball.type_id).unwrap_or_default(), Vec::new());
            let stats = crate::services::combat::groups_health::ShipStats::from_fit(&fit, crate::services::fit::Key::Ship);
            let condition = crate::services::combat::groups_health::Condition {
                now: stats.max, stats, shield_at: crate::client::filetime_now(),
            };
            self.install_condition(id, condition, fit);
            if !self.actor_combat.contains_key(&id) { return false; }
        }
        brain.next_think_ms = self.ms.saturating_add(id.rem_euclid(crate::config::game().combat.npc_think_ms));
        let first = self.brains.insert(id, brain).is_none();
        if first {
            super::ship_cosmetics::announce(self, id);
        }
        true
    }
    /// Whether the system has an NPC brain for this ball.
    pub(crate) fn is_npc(&self, id: i64) -> bool {
        self.brains.contains_key(&id)
    }
    /// Support is restricted to other NPC brains in a shared spawn group, or
    /// to the same ball owner when the source has no spawn group.
    pub(crate) fn npc_assist_eligible(&self, source: i64, target: i64) -> bool {
        if source == target || !self.is_npc(source) || !self.is_npc(target) { return false; }
        let Some((me, other)) = self.ball(source).zip(self.ball(target)) else { return false };
        if me.char_id.is_some() || other.char_id.is_some() { return false; }
        match self.member_groups.get(&source).filter(|groups| !groups.is_empty()) {
            Some(groups) => self.member_groups.get(&target).is_some_and(|other_groups|
                groups.iter().any(|group| other_groups.contains(group))),
            None => me.owner_id == other.owner_id,
        }
    }
    /// Select the lowest score among eligible NPCs, breaking ties by item ID.
    /// The callback receives sampled balls and returns None for out-of-range or
    /// otherwise unsuitable candidates. It must not await, access DB or call World.
    /// Remote repairs can reuse this selector with their own score and range rules.
    pub(crate) fn assist_target(&self, source: i64, mut score: impl FnMut(&Ball, &Ball) -> Option<f64>) -> Option<Ball> {
        let me = self.sampled_ball(source)?;
        self.brains.keys().filter_map(|&id| {
            if !self.npc_assist_eligible(source, id) { return None; }
            let target = self.sampled_ball(id)?;
            let value = score(&me, &target).filter(|value| value.is_finite())?;
            Some((value, target))
        }).min_by(|(a, x), (b, y)| a.total_cmp(b).then_with(|| x.id.cmp(&y.id))).map(|(_, ball)| ball)
    }
    pub fn add_aggressor(&mut self, id: i64, pilot: i64) {
        if let Some(brain) = self.brains.get_mut(&id) { brain.aggressors.insert(pilot); }
    }
    /// The callback runs once INSIDE this actor: no await, DB or World
    /// calls. It may spawn owned asynchronous work. Missing members count as gone.
    pub fn spawn_group(&mut self, members: impl IntoIterator<Item = i64>, on_cleared: impl FnOnce(&mut System) + Send + 'static) -> u64 {
        let id = NEXT_GROUP.fetch_add(1, Ordering::Relaxed);
        let members: HashSet<_> = members.into_iter().filter(|id| self.contains(*id)).collect();
        if members.is_empty() {
            on_cleared(self);
        } else {
            for member in &members { self.member_groups.entry(*member).or_default().insert(id); }
            self.groups.insert(id, SpawnGroup { members, on_cleared: Box::new(on_cleared) });
        }
        id
    }
    /// Cleared or unknown groups read as zero; handles are process-unique across reloads.
    pub fn group_remaining(&self, id: u64) -> usize {
        self.groups.get(&id).map_or(0, |g| g.members.len())
    }
    pub(super) fn npc_removed(&mut self, id: i64) {
        self.brains.remove(&id);
        if let Some(groups) = self.member_groups.remove(&id) {
            for group in groups {
                let Some(g) = self.groups.get_mut(&group) else { continue };
                g.members.remove(&id);
                if g.members.is_empty() {
                    let g = self.groups.remove(&group).unwrap();
                    (g.on_cleared)(self);
                }
            }
        }
    }
    fn npc_effect(&mut self, brain: &NpcBrain, action: NpcAction) {
        if self.effect_jobs.is_some() {
            self.effect_staging.push(EffectJob::Npc { effects: brain.effects.clone(), action });
        }
    }
    pub(super) fn admit_effect_jobs(&mut self) {
        let mut staging = std::mem::take(&mut self.effect_staging);
        for job in staging.drain(..) {
            let future = match job {
                EffectJob::Npc { effects, action } => effects(self, action),
                EffectJob::Drone { effects, action } => effects(self, action),
            };
            if let (Some(tx), Some(future)) = (&self.effect_jobs, future) { let _ = tx.send(future); }
        }
        self.effect_staging = staging;
    }
    pub(super) fn think_npcs(&mut self) {
        // Reuse the scratch vector: no per-tick allocation after reaching capacity.
        let mut due = std::mem::take(&mut self.due_brains);
        due.extend(self.brains.iter().filter(|(_, b)| self.ms >= b.next_think_ms).map(|(&id, _)| id));
        for &id in &due {
            let Some(mut brain) = self.brains.remove(&id) else { continue };
            brain.next_think_ms = self.ms.saturating_add(crate::config::game().combat.npc_think_ms);
            self.think_npc(id, &mut brain);
            if self.contains(id) { self.brains.insert(id, brain); }
        }
        due.clear();
        self.due_brains = due;
    }
    fn lose_npc_target(&mut self, id: i64, brain: &mut NpcBrain) {
        self.remove_projections_from(id);
        if let Some((_, pilot, _)) = brain.target.take() {
            brain.target_generation = next_npc_target_generation();
            self.notify_pilot(pilot, "OnTarget", vec![Value::str("otherlost"), Value::Int(id), Value::None]);
            self.command(id, |b, stamp, _| {
                b.stop();
                vec![event(stamp, "Stop", vec![Value::Int(b.id)])]
            });
        }
        brain.announced = false;
    }
    pub(crate) fn drop_npc_targets(&mut self, id: i64) {
        self.drone_jammed(id);
        let Some(mut brain) = self.brains.remove(&id) else { return };
        self.lose_npc_target(id, &mut brain);
        self.brains.insert(id, brain);
    }
    pub(super) fn refresh_npc_targeting(&mut self, id: i64, scan_resolution: f64, max_target_range: Option<f64>) {
        let Some(mut brain) = self.brains.remove(&id) else { return };
        brain.config.scan_resolution = scan_resolution;
        brain.max_target_range = max_target_range;
        let out_of_range = brain.target.is_some_and(|(ship, _, _)| {
            self.ball(id).zip(self.ball(ship)).is_some_and(|(me, victim)|
                max_target_range.is_some_and(|range| len(sub(victim.position_at(self.ms), me.position_at(self.ms))) > range))
        });
        if out_of_range { self.lose_npc_target(id, &mut brain); }
        self.brains.insert(id, brain);
    }
    fn think_npc(&mut self, id: i64, brain: &mut NpcBrain) {
        let Some(me) = self.ball(id) else { return };
        let pos = me.position_at(self.ms);
        let current = brain.target.and_then(|(ship, _, _)| self.ball(ship))
            .filter(|b| targeting::target_visible(b) && b.signature_radius.is_some()
                && len(sub(b.position_at(self.ms), pos)) < brain.config.aggro_range * crate::config::game().combat.npc_target_loss_range_factor
                && brain.max_target_range.is_none_or(|range| len(sub(b.position_at(self.ms), pos)) <= range));
        let victim_id = current.map(|b| b.id);
        let (shield, armor, cycle) = brain.config.repairs;
        if (shield > 0.0 || armor > 0.0) && self.ms >= brain.next_repair_ms {
            brain.next_repair_ms = self.ms + cycle as i64;
            self.npc_effect(brain, NpcAction::Repair);
        }
        if crate::services::combat::jamming::is_jammed(id, self.now_ms()) {
            if brain.target.is_some() { self.lose_npc_target(id, brain); }
            return;
        }
        if victim_id.is_none() {
            self.lose_npc_target(id, brain);
            let nearest = self.pilots().filter_map(|b| b.char_id.map(|owner| (b, owner)))
                .chain(self.drones().filter_map(|d| self.ball(d.id).map(|b| (b, d.owner))))
                .filter(|(b, pilot)| (brain.owner == Some(HOSTILE_TO_ALL) || brain.owner == Some(*pilot) || brain.aggressors.contains(pilot))
                    && targeting::target_visible(b) && b.signature_radius.is_some())
                .map(|(b, pilot)| (b, pilot, len(sub(b.position_at(self.ms), pos))))
                .filter(|(_, _, distance)| *distance <= brain.config.aggro_range && brain.max_target_range.is_none_or(|range| *distance <= range))
                .min_by(|(_, _, a), (_, _, b)| a.total_cmp(b))
                .map(|(b, pilot, _)| (b.id, pilot, b.signature_radius.unwrap()));
            if let Some((ship, pilot, signature)) = nearest {
                let orbit = brain.config.fly_range;
                self.command(id, move |b, stamp, others| {
                    let Some(target) = others.ball(ship) else { return vec![] };
                    if !b.orbit(target, orbit as f32) { return vec![]; }
                    b.set_speed_fraction(1.0);
                    vec![event(stamp, "Orbit", vec![Value::Int(id), Value::Int(ship), Value::Float(orbit)]),
                        event(stamp, "SetSpeedFraction", vec![Value::Int(id), Value::Float(1.0)])]
                });
                let locked = self.ms.saturating_add(lock_time_ms(brain.config.scan_resolution, signature) as i64);
                brain.target_generation = next_npc_target_generation();
                brain.target = Some((ship, pilot, locked));
                brain.announced = false;
                brain.next_shot_ms = locked;
                let generation = brain.target_generation;
                self.schedule_at(locked, move |s| s.complete_npc_lock(id, ship, pilot, locked, generation));
            }
            return;
        }
        let (ship, _, _) = brain.target.unwrap();
        if brain.announced && self.ms >= brain.next_shot_ms {
            self.npc_effect(brain, NpcAction::Shot { me: self.sampled_ball(id).unwrap(), victim: self.sampled_ball(ship).unwrap(), generation: brain.target_generation });
            brain.next_shot_ms = self.ms + brain.config.rate_of_fire_ms as i64;
        }
    }
    /// Validate a queued NPC projection or effect in the actor immediately before it applies.
    pub(crate) fn npc_projection_valid(&self, id: i64, target: i64, generation: u64) -> bool {
        let Some(brain) = self.brains.get(&id) else { return false };
        !crate::services::combat::jamming::is_jammed(id, self.now_ms())
            && brain.target_generation == generation && brain.announced
            && brain.target.is_some_and(|(ship, _, _)| ship == target)
            && self.ball(id).zip(self.ball(target)).is_some_and(|(me, victim)|
                targeting::target_visible(victim) && victim.signature_radius.is_some()
                    && len(sub(victim.position_at(self.ms), me.position_at(self.ms)))
                        < brain.config.aggro_range * crate::config::game().combat.npc_target_loss_range_factor
                    && brain.max_target_range.is_none_or(|range| len(sub(victim.position_at(self.ms), me.position_at(self.ms))) <= range))
    }
    fn npc_effect_cycle(&mut self, id: i64, target: i64, generation: u64, index: usize) {
        if !self.npc_projection_valid(id, target, generation) { return; }
        let brain = &self.brains[&id];
        let Some(module) = brain.modules.get(index) else { return };
        let duration_ms = module.duration_ms;
        let action = NpcAction::Effect {
            me: self.sampled_ball(id).unwrap(), victim: self.sampled_ball(target).unwrap(),
            effect_id: module.effect_id, source_module: module.source_module, duration_ms, generation,
        };
        if self.effect_jobs.is_some() {
            self.effect_staging.push(EffectJob::Npc { effects: brain.effects.clone(), action });
        }
        if duration_ms > 0.0 {
            self.schedule_at(self.ms.saturating_add(duration_ms as i64), move |s|
                s.npc_effect_cycle(id, target, generation, index));
        }
    }
    fn complete_npc_lock(&mut self, id: i64, ship: i64, pilot: i64, deadline: i64, generation: u64) {
        let Some(mut brain) = self.brains.remove(&id) else { return };
        let current = brain.target_generation == generation
            && brain.target == Some((ship, pilot, deadline)) && !brain.announced;
        let valid = current && !crate::services::combat::jamming::is_jammed(id, self.now_ms())
            && self.ball(id).zip(self.ball(ship)).is_some_and(|(me, victim)|
                (victim.char_id == Some(pilot) || self.drone(ship).is_some_and(|d| d.owner == pilot))
                && targeting::target_visible(victim) && victim.signature_radius.is_some()
                && len(sub(victim.position_at(self.ms), me.position_at(self.ms))) < brain.config.aggro_range * crate::config::game().combat.npc_target_loss_range_factor
                && brain.max_target_range.is_none_or(|range| len(sub(victim.position_at(self.ms), me.position_at(self.ms))) <= range));
        if valid {
            self.notify_pilot(pilot, "OnTarget", vec![Value::str("otheradd"), Value::Int(id)]);
            brain.announced = true;
            if let Some(char_id) = self.ball(ship).and_then(|ball| ball.char_id) {
                crate::progress::publish_live(self.progress_registry(), &crate::lua::events::GameEvent::Targeted {
                    char_id, by_ball_id: id, system_id: self.id,
                });
            }
            self.npc_effect(&brain, NpcAction::Shot { me: self.sampled_ball(id).unwrap(), victim: self.sampled_ball(ship).unwrap(), generation: brain.target_generation });
            brain.next_shot_ms = self.ms + brain.config.rate_of_fire_ms as i64;
        } else if current {
            self.lose_npc_target(id, &mut brain);
        }
        self.brains.insert(id, brain);
        if valid {
            let generation = self.brains[&id].target_generation;
            let count = self.brains[&id].modules.len();
            for index in 0..count { self.npc_effect_cycle(id, ship, generation, index); }
        }
    }
}

#[cfg(test)] mod tests;
