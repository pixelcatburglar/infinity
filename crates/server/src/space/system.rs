//! Actor-owned ballpark membership, visibility, actions, motion and timers,
//! serving destiny updates consumed by `eve/client/script/remote/michelle.py`.
//! Initial snapshots and incremental Destiny events share ordered observer channels.
//! Celestials are supplied from the SDE; dynamic state and timers are in memory.
//! Changed pilot positions are saved to `game.items` by the snapshot worker.
//! One FIFO holds captured AddBalls updates and mode, propulsion, cloak and
//! missile-launch actions. Ball scene membership is partitioned into coarse
//! fixed-edge grid bubbles (`interest`); each observed bubble shares one
//! committed token roster and one boundary delta (`membership`). An action
//! enqueued at logical time C is stamped C and applied at the next boundary
//! before evolution to C+1.
//! `current_time` starts at load-time Unix seconds and advances once per fixed
//! one-second evolution, independently of the wall clock. Timers and NPC
//! decisions run on 100 ms housekeeping ticks; snapshots follow motion.
//! A `queue_destiny_flush` marker signals once every earlier action has been
//! delivered; an idle ballpark completes the ack immediately.
//! Explicit pilot actions cancel jump cloak once; expiry uses the raw uncloak queue.
//! Uncloak batches restore the ball and clear its client shader for every observer.
//! Fleet target tags are cleared on target removal or cloaking and synchronized on observation.
//! After more than five due steps, catchup clamps to one logical step and the
//! next deadline is the next whole wall second, an intentional departure from
//! native cadence. The clamped interpolation anchor is actual execution time;
//! the logical counter is not reset to wall time.
//!
//! Persistent positions and ship conditions checkpoint through the same worker;
//! projections, capacitor, timers and other ephemeral state do not survive restart.
//! In-process membership transfers carry captured condition; ordinary native cleanup
//! follows actor membership generations. Legacy sphere collisions do not emit bumps.
//! Ordinary warp charges the shared in-memory capacitor at activation; partial routes
//! publish their completed state and do not complete the original arrival callback.
//! Live compressor ranges use actor-owned module generations and static type lists.
//! Taking a ball also ends its in-memory ECM lockout before its pilot leaves.
//! Rack heat and overload membership are transient and are cleared when a ship leaves.
//! Committed item renames project the new name onto the live ball's slim and
//! the `evelocations` cache of pilots whose live observer can see the ball
//! now; that audience is the observer's own scene bubble plus globals.
//! GM `/effect` previews are bounded sent handles recorded per real source
//! ship (one-shot pulses keep no handle), delivered only to the issuing
//! observer and dropped on take or rebind.

use super::*;
use std::collections::{BTreeMap, HashSet, VecDeque};
use std::future::Future;
#[path = "effects.rs"] mod effects;
#[path = "boarding.rs"] pub(crate) mod boarding;
pub(crate) mod appearance;
mod affiliation;
mod npc_control;
mod sovereignty_placement;
pub(crate) mod super_weapons;
pub(crate) mod gm_effects;
pub(crate) mod missiles;
mod tick;
mod unload;
pub(crate) mod npc_sleep;
pub(crate) mod warp;
pub(crate) mod warp_arrival;
mod warp_capacitor;
pub(crate) mod observers;
pub(crate) mod presentation;
mod interest;
mod membership;
use presentation::SceneExit;
use observers::{DestinyAudience, Observer};
use effects::Tractor;

/// carbonengine destiny Ballpark.cpp tick catchup policy.
const MAX_CATCHUP_STEPS: i64 = 5;
/// Destiny destruction effect EXPLOSION.
const DESTRUCTION_EFFECT_EXPLOSION: i64 = 3;
struct Timer(actor::Job);
type Action = Box<dyn FnOnce(&mut System, i64) + Send>;

struct Compressor {
    generation: Arc<std::sync::atomic::AtomicBool>,
    list: i64,
    range: f64,
}

struct ModuleCloak {
    module: i64,
    generation: Arc<std::sync::atomic::AtomicBool>,
    reactivation_ms: i64,
    targeting_ms: i64,
}

/// Live pilot membership: the piloted ball plus the persisted GM NPC-ignore
/// flag seeded by ship preparation or `set_npc_ignored` publication.
struct PilotState { ship_id: i64, npc_ignored: bool, binding: Option<std::sync::Arc<super::containment::PilotBinding>> }

#[derive(Default)]
pub struct System {
    pub(super) id: i64,
    pub(super) sovereignty: sovereignty::State,
    balls: HashMap<i64, Ball>,
    /// Serials issued to live balls for incarnation-fenced scene tokens.
    ball_tokens: HashMap<i64, u64>,
    ball_token_serial: u64,
    pub(super) rocks: HashMap<i64, super::asteroids::Rock>,
    pub(super) asteroid_timers: HashMap<i64, (i64, u64)>,
    pub(super) asteroid_timer_sequence: u64,
    pilots: HashMap<i64, PilotState>,
    observers: HashMap<i64, Observer>,
    observer_epoch: u64,
    interest: interest::Interest,
    membership: membership::Membership,
    pub(super) registry: Arc<Pilots>,
    pub(super) progress_registry: Arc<crate::progress::Registry>,
    pub(super) world: std::sync::Weak<World>,
    target_states: HashMap<i64, TargetState>,
    free_ids: Vec<i64>,
    collision_scratch: Vec<collision::Snapshot>,
    exited_ids: Vec<i64>,
    warp_corrections: Vec<i64>,
    /// Reused scratch for the per-tick completed player-lock sweep; never authoritative state.
    pub(crate) player_lock_candidates: Vec<(i64, i64, i64)>,
    missile_flights: HashMap<i64, missiles::Flight>,
    missile_scratch: Vec<i64>,
    /// Live damageable projectile profiles, one per launched missile or bomb
    /// incarnation; cleared by the same terminals that end its flight.
    damageable_projectiles: HashMap<i64, damageable::DamageableProjectile>,
    /// One reserved Defender target per bomb ID; token fencing remains terminal authority.
    pub(super) defender_reservations: HashSet<i64>,
    /// The one bomb terminal registry: each entry owns its impact callback
    /// until the delay resolver or a removal consumes it exactly once.
    bomb_flights: HashMap<i64, effects::BombFlight>,
    /// Celestial warp-in points (warpin.rs), by celestial ID.
    warp_ins: HashMap<i64, V3>,
    delivery_batching: bool,
    presentation: presentation::Presentation,
    pub(in crate::space) skins: ship_cosmetics::Skins,
    pub(in crate::space) notice_hub: Option<crate::gateway::notices::NoticeHub>,
    timers: BTreeMap<(i64, u64), Timer>,
    pub(in crate::space) attribution: attribution::Local,
    warp_exits: HashMap<i64, warp::WarpIntent>,
    warp_arrivals: HashMap<warp_arrival::WarpArrivalId, warp_arrival::ArrivalEntry>,
    timer_sequence: u64,
    pub(super) ms: i64,
    current_time: i64,
    next_tick_ms: Option<i64>,
    actions: VecDeque<(i64, Action)>,
    pending_uncloak: HashSet<i64>,
    compressors: HashMap<i64, HashMap<i64, Compressor>>,
    module_cloaks: HashMap<i64, ModuleCloak>,
    cloak_reactivation: HashMap<i64, i64>,
    cloak_targeting: HashMap<i64, i64>,
    tractors: HashMap<i64, Tractor>,
    tractor_epochs: HashMap<i64, Arc<()>>,
    pub(super) brains: HashMap<i64, NpcBrain>,
    pub(super) drones: HashMap<i64, drone::DroneBrain>,
    pub(super) drone_reservations: HashMap<u64, drone::Reservation>,
    pub(super) actor_combat: HashMap<i64, combat::ActorCombatRecord>,
    pub(super) combat_facts: Option<combat_facts::CombatFacts>,
    pub(super) nativefact_clock: i64,
    pub(super) effect_jobs: Option<mpsc::UnboundedSender<npc::NpcFuture>>,
    pub(super) effect_staging: Vec<npc::EffectJob>,
    pub(super) deferred_jobs: Option<mpsc::UnboundedSender<npc::NpcFuture>>,
    pub(super) groups: HashMap<u64, npc::SpawnGroup>,
    pub(super) member_groups: HashMap<i64, std::collections::HashSet<u64>>,
    pub(super) due_brains: Vec<i64>,
    pub(super) due_drones: Vec<i64>,
    pub(super) drone_aggressors: HashMap<i64, drone::automatic::Aggressors>,
    pub(super) automatic_drones: Vec<i64>,
    pub(super) automatic_targets: Vec<i64>,
    pub(super) drone_announcements: Vec<i64>,
    pub(super) destiny_flush_acks: Vec<tokio::sync::oneshot::Sender<()>>,
    pub(super) snapshot_context: Option<snapshot::Context>,
    pub(super) projection_sde: Option<Arc<Sde>>,
    pub(crate) heat: crate::services::effects::heat::HeatState,
    pub(super) projections: HashMap<i64, Vec<crate::services::effects::projection::Projection>>,
    pub(super) projection_targets: HashMap<i64, i64>,
    pub(super) projection_revisions: HashMap<i64, u64>,
    pub(super) snapshot_positions: HashMap<i64, (u64, V3)>,
    pub(super) snapshot_generation: u64,
    pub(super) snapshot_due: i64,
    pub(super) loaded_quiescing: bool,
    pub(super) loaded_failed_periods: i64,
    /// Superweapon firing-source lockouts and generation-guarded pulse jobs.
    super_weapons: super_weapons::SuperWeapons,
    gm_previews: HashMap<i64, gm_effects::SentPreviews>,
    /// NPC bounty pools owned by this actor; the shared coordinator is World-side.
    pub(super) bounties: bounties::State,
}
impl System {
    pub(crate) fn sde(&self) -> Option<&crate::sde::Sde> { self.projection_sde.as_deref() }
    pub(crate) fn progress_registry(&self) -> &crate::progress::Registry { &self.progress_registry }
    pub fn new(id: i64, registry: Arc<Pilots>, unix_ms: i64) -> Self {
        let current_time = unix_ms.div_euclid(MILLISECONDS_PER_SECOND);
        Self { id, registry, ms: unix_ms, current_time,
            next_tick_ms: Some(next_aligned_ms(unix_ms, MILLISECONDS_PER_SECOND)), ..Self::default() }
    }
    pub fn current_time(&self) -> i64 { self.current_time }
    /// Schedule relative to this actor's current millisecond clock. Due timers
    /// run only on ticks, ordered by deadline and then insertion order (also for
    /// zero delays). Closures run inside the actor: no await, DB access or calls back into World.
    pub fn schedule(&mut self, delay_ms: u64, f: impl FnOnce(&mut System) + Send + 'static) {
        self.schedule_named(delay_ms, std::any::type_name_of_val(&f), f);
    }
    /// Queue an absolute Unix-millisecond deadline, ordered by insertion on ties.
    pub fn schedule_at(&mut self, due: i64, f: impl FnOnce(&mut System) + Send + 'static) {
        self.schedule_at_named(due, std::any::type_name_of_val(&f), f);
    }
    /// `schedule` under a caller-preserved static name.
    pub(in crate::space) fn schedule_named(&mut self, delay_ms: u64, name: &'static str, f: impl FnOnce(&mut System) + Send + 'static) {
        self.schedule_at_named(self.ms.saturating_add(i64::try_from(delay_ms).unwrap_or(i64::MAX)), name, f);
    }
    pub(in crate::space) fn schedule_at_named(&mut self, due: i64, name: &'static str, f: impl FnOnce(&mut System) + Send + 'static) {
        let sequence = self.timer_sequence;
        self.timer_sequence = sequence.checked_add(1).expect("space timer sequence exhausted");
        self.timers.insert((due, sequence), Timer(actor::Job::named(name, f)));
    }
    /// Container lengths for diagnostics; never iterates or locks.
    pub(in crate::space) fn attribution_counts(&self) -> (u64, u64, u64) {
        (self.balls.len() as u64, self.observers.len() as u64, self.pilots.len() as u64)
    }
    /// Actor simulation time in Unix milliseconds.
    pub fn now_ms(&self) -> i64 { self.ms }
    /// Submit owned async work to the concurrent worker outside the actor.
    pub fn defer(&self, job: impl Future<Output = ()> + Send + 'static) {
        let name = std::any::type_name_of_val(&job);
        if !self.try_defer(crate::runtime_watchdog::watch("defer", name, job)) {
            tracing::warn!(system = self.id, "space effects worker unavailable");
        }
    }
    pub fn warp_in(&self, id: i64) -> Option<V3> { self.warp_ins.get(&id).copied() }
    pub(super) fn asteroid_action(&mut self, f: impl FnOnce(&mut System) + Send + 'static) {
        self.actions.push_back((self.current_time, Box::new(move |system, _| f(system))));
    }
    pub(super) fn set_warp_in(&mut self, id: i64, point: V3) { self.warp_ins.insert(id, point); }
    pub fn ball(&self, id: i64) -> Option<&Ball> {
        self.balls.get(&id)
    }
    pub(super) fn set_ball_owner(&mut self, id: i64, owner: i64) {
        if let Some(ball) = self.balls.get_mut(&id) { ball.owner_id = owner; }
    }

    /// Sample the completed segment at the actor's shared millisecond clock.
    pub fn sample_position(&self, id: i64) -> Option<V3> {
        self.ball(id).map(|ball| ball.position_at(self.ms))
    }

    /// Clone only the requested ball and materialize its sampled position and velocity.
    pub fn sampled_ball(&self, id: i64) -> Option<Ball> {
        self.ball(id).map(|ball| {
            let mut sampled = ball.clone();
            (sampled.pos, sampled.vel) = ball.state_at(self.ms);
            sampled
        })
    }

    pub fn balls(&self) -> impl Iterator<Item = &Ball> {
        self.balls.values()
    }

    pub fn pilots(&self) -> impl Iterator<Item = &Ball> {
        self.pilots.values().filter_map(|p| self.ball(p.ship_id))
    }
    pub(crate) fn persistent_balls(&self) -> impl Iterator<Item = &Ball> {
        self.snapshot_positions.keys().filter_map(|id| self.ball(*id))
    }

    pub fn pilot_ball(&self, char_id: i64) -> Option<&Ball> {
        self.pilots.get(&char_id).and_then(|p| self.ball(p.ship_id))
    }
    /// Actor-owned scene admission, independent of client ballpark delivery:
    /// the pilot's own ball, a non-cloaked always-present member, or a
    /// non-cloaked member of the ego's current bubble.
    pub(crate) fn visible_to(&self, character: i64, ball: i64) -> bool {
        let Some(ego) = self.pilot_ball(character) else { return false };
        let Some(target) = self.ball(ball) else { return false };
        self.local_pair_from(ego, target)
    }
    /// Current ballpark visibility for a pilot with a live matching observer.
    pub(crate) fn fleet_broadcast_visible(&self, character: i64, ball: i64) -> bool {
        self.pilot_observer(character).is_some() && self.visible_to(character, ball)
    }
    /// Snapshot a pilot's real actor-owned damage state; every receiving
    /// actor evaluates live scene visibility of the ball.
    pub(crate) fn fleet_damage_snapshot(&self, character: i64) -> Option<(i64, Value)> {
        let ball = self.pilot_ball(character)?;
        Some((ball.id, ball.damage_state()))
    }

    /// Targeted ballpark function, only while the destination observer owns its ego.
    /// A same-system watcher outside the ball's scene still gets it: fleet
    /// watchlists deliberately reach across scene boundaries.
    pub(crate) fn fleet_damage_update(&mut self, watcher: i64, source_system: i64, ship: i64, damage: Value) {
        let Some((_ego, token)) = self.pilot_observer(watcher) else { return };
        if self.id == source_system && self.scene_ball_covered(token, ship) { return; }
        // michelle.py:875-879,1594-1599: a ballpark function, not a service event.
        let update = event(self.current_time, "OnFleetDamageStateChange", vec![Value::Int(ship), damage]);
        self.emit_destiny(DestinyAudience::Observer(token), vec![update]);
    }
    /// Identity of persistent ball membership, invalidated on replacement.
    pub(crate) fn persistent_generation(&self, ship: i64) -> Option<u64> {
        self.snapshot_positions.get(&ship).map(|(generation, _)| *generation)
    }

    pub fn contains(&self, id: i64) -> bool {
        self.balls.contains_key(&id)
    }

    pub fn insert(&mut self, mut ball: Ball) {
        ball.last_ms = self.current_time * MILLISECONDS_PER_SECOND;
        ball.interpolation_anchor_ms = self.ms;
        ball.old_pos = ball.pos;
        ball.old_vel = ball.vel;
        ball.segment_acceleration = None;
        self.take(ball.id);
        if let Some(char_id) = ball.char_id {
            let binding = self.world.upgrade().and_then(|world| world.bindings.binding(char_id));
            self.pilots.insert(char_id, PilotState { ship_id: ball.id, npc_ignored: false, binding });
            self.snapshot_generation = self.snapshot_generation.checked_add(1).expect("snapshot generation exhausted");
            self.snapshot_positions.insert(ball.id, (self.snapshot_generation, ball.pos));
            if self.snapshot_due == 0 {
                self.snapshot_due = self.ms.saturating_add(crate::config::game().space.position_snapshot_secs.saturating_mul(MILLISECONDS_PER_SECOND));
            }
        }
        let id = ball.id;
        self.balls.insert(ball.id, ball);
        self.scene_insert(id);
        if self.actor_combat.get(&id).is_some_and(|record| matches!(record.phase, combat::CombatPhase::Dying { .. } | combat::CombatPhase::Transition { .. })
            && !record.has_loaded_obligations()) {
            self.actor_combat.remove(&id);
        }
    }
    /// Register one inserted ball: grid placement, the always-present layer
    /// decision, and either the cold baseline or a queued birth entry.
    pub(super) fn scene_insert(&mut self, id: i64) {
        let (pos, global) = { let ball = &self.balls[&id]; (ball.pos, self.roster_global(ball)) };
        self.interest.place(id, pos);
        if global { self.membership.globals.insert(id); }
        let token = self.capture_ball(id).expect("inserted ball");
        self.admit_scene_ball(token);
    }
    /// Release current topology immediately; the queued exit retires the
    /// token's last delivered scene at its own FIFO position.
    pub(super) fn scene_remove(&mut self, id: i64) {
        self.interest.remove(id);
        self.membership.globals.remove(&id);
    }
    /// Missile flights and projectile state end with their ball: the
    /// damageable profile first, so a cancellation callback can no longer hit
    /// the departing incarnation, then the ball's own terminal, every flight
    /// that targets or was launched by it, and its bomb registration.
    fn missile_departure(&mut self, id: i64) {
        self.damageable_projectiles.remove(&id);
        self.defender_reservations.remove(&id);
        self.remove_missile_flight(id);
        self.invalidate_missile_target(id);
        self.invalidate_missile_launcher(id);
        self.remove_bomb_flight(id);
    }
    /// Remove a ball and capture its presentation exit: old observer tickets and
    /// clear IDs for the fleet boundary, the sustained effects to stop, and the
    /// ball's last visibility. The world-side removal is complete here; the
    /// client clear happens only in the queued exit action.
    pub(in crate::space) fn take_for_scene_exit(&mut self, id: i64) -> Option<(Ball, SceneExit)> {
        self.missile_departure(id);
        if (self.is_npc(id) || self.drone(id).is_some()) && self.contains(id) {
            if let Some(generation) = self.combat_membership_generation(id) {
                let _ = self.retire_combat_source(generation);
            }
        }
        self.drone_controller_lost(id);
        self.drone_removed(id);
        self.cleanup_tractors(id);
        self.cancel_warp_intent(id);
        if self.balls.contains_key(&id) {
            crate::services::combat::jamming::clear(self, id);
        }
        let exit = if self.balls.contains_key(&id) {
            let fleet = crate::services::fleets::space_state::remove_ball_tags(self, self.id, id, self.balls[&id].char_id);
            Some(self.capture_scene_exit(id, fleet))
        } else { None };
        self.remove_combat_state(id);
        if !self.balls.contains_key(&id) { return None; }
        self.ball_tokens.remove(&id);
        self.pending_uncloak.remove(&id);
        sovereignty::links::removed(self, id);
        crate::services::effects::heat::remove_ship(self, id);
        self.compressors.remove(&id);
        self.module_cloaks.remove(&id);
        self.cloak_targeting.remove(&id);
        self.super_weapon_ship_left(id);
        self.remove_projections_from(id);
        self.remove_projections_on_departure(id);
        crate::services::effects::active_cycle::ship_departure(self, id);
        self.gm_effect_ball_leaving(id);
        self.stop_all_followers(id);
        self.snapshot_positions.remove(&id);
        let ball = self.balls.remove(&id)?;
        self.scene_remove(id);
        if let Some(char_id) = ball.char_id {
            if self.pilots.get(&char_id).is_some_and(|p| p.ship_id == id) {
                self.pilots.remove(&char_id);
                if let Some(facts) = self.combat_facts.as_mut() { facts.mark_character_scopes_dirty(); }
            }
        }
        if self.brains.contains_key(&id) {
            crate::services::combat::forget_ship(id);
        }
        self.npc_removed(id);
        Some((ball, exit?))
    }

    /// Remove the current ball and queue its native scene departure.
    pub fn take(&mut self, id: i64) -> Option<Ball> {
        let (ball, exit) = self.take_for_scene_exit(id)?;
        self.queue_scene_exit(exit, move |stamp|
            vec![event(stamp, "RemoveBalls", vec![Value::list([Value::Int(id)])])]);
        Some(ball)
    }

    /// Ballpark::StopAllFollowers, for followers of a removed target.
    fn stop_all_followers(&mut self, id: i64) {
        for ball in self.balls.values_mut().filter(|b| b.follow_id == id && matches!(b.mode, MODE_FOLLOW | MODE_ORBIT | MODE_MISSILE)) {
            if ball.mode == MODE_MISSILE {
                ball.massive = false;
                ball.goto_direction(ball.vel);
            } else if ball.mode == MODE_ORBIT && ball.interactive { ball.goto_direction(ball.vel); } else { ball.stop(); }
        }
    }

    pub fn notify_pilot(&self, char_id: i64, name: &str, args: Vec<Value>) {
        self.registry.notify_pilot(char_id, name, args);
    }

    /// The sender captured with this pilot's membership, not the current registry sender.
    pub(crate) fn pilot_sender(&self, character: i64) -> Option<super::NotifyTx> {
        self.pilots.get(&character).and_then(|pilot| pilot.binding.as_ref()).map(|binding| binding.sender().clone())
    }
    pub(crate) fn pilot_bindings(&self) -> impl Iterator<Item = (i64, Option<std::sync::Arc<super::containment::PilotBinding>>)> + '_ {
        self.pilots.iter().map(|(&character, pilot)| (character, pilot.binding.clone()))
    }

    /// Apply a committed item rename to the live ball and eligible pilots.
    ///
    /// `game.items.name` remains the authority; this refreshes the slim and the
    /// location cache of every pilot whose live observer can see the ball now.
    /// The caller is excluded: its own reply already carries the same row.
    pub(crate) fn set_item_name(&mut self, item_id: i64, name: String, row: &Value, caller: i64) {
        let Some(ball) = self.balls.get_mut(&item_id) else { return };
        ball.slim.retain(|(key, _)| *key != "name");
        ball.slim.push(("name", Value::unicode(name)));
        for char_id in self.pilots.keys().copied() {
            if char_id != caller && self.fleet_broadcast_visible(char_id, item_id) {
                self.notify_pilot(char_id, "OnCfgDataChanged", vec![Value::str("evelocations"), row.clone()]);
            }
        }
    }
    fn population(&self) -> usize {
        self.balls.values().filter(|b| b.char_id.is_some()).count()
    }
    pub fn request_entry(&self, char_id: i64) -> Result<(), i64> {
        self.registry.request_entry(self.id, char_id, self.population())
    }
    pub(super) fn is_cold(&self, collisions: bool) -> bool {
        self.actions.is_empty() && self.destiny_flush_acks.is_empty()
            && !self.combat_facts.as_ref().is_some_and(|facts| facts.is_dirty() || facts.batch_count() != 0)
            && !self.timers.first_key_value().is_some_and(|(&(due, _), _)| due <= self.ms)
            && self.module_cloaks.is_empty() && self.brains.is_empty() && self.drones.is_empty() && self.observers.is_empty()
            && !crate::services::effects::heat::active(&self.heat) && !self.balls.values().any(|b|
            (b.free && (b.mode != MODE_STOP || b.vel != [0.0; 3]))
            || (b.cloak != 0 && b.cloak_until > 0))
            && !collision::stationary_contact(&self.balls, collisions)
    }
    pub(super) fn queue_drone_announcement(&mut self, id: i64) {
        self.actions.push_back((self.current_time, Box::new(move |s, _| s.drone_announcements.push(id))));
    }
    /// Signal once every action queued so far has been delivered. An idle
    /// ballpark (empty action queue, no in-flight pending flush, no owed
    /// scene initial) fires the ack at once; otherwise a FIFO marker moves it
    /// into `destiny_flush_acks`, which the tick drains after batches,
    /// initials and announcements — it confirms only its own prefix.
    pub(crate) fn queue_destiny_flush(&mut self, ack: tokio::sync::oneshot::Sender<()>) {
        if self.actions.is_empty() && !self.delivery_batching && !self.scene_work_pending() {
            let _ = ack.send(());
        } else {
            self.actions.push_back((self.current_time, Box::new(move |s, _| s.destiny_flush_acks.push(ack))));
        }
    }
    /// Insert the ball first so a replaced incarnation's exit cleanup precedes
    /// the new Add and entry in the same ordered stream. The birth rides the
    /// same FIFO: its entry marks readiness before the boundary delta Adds it.
    pub fn add_ship(&mut self, mut ball: Ball) {
        ball.last_ms = self.current_time * MILLISECONDS_PER_SECOND;
        self.insert(ball);
    }
    pub fn remove_ball(&mut self, id: i64) -> Option<Ball> {
        let (b, exit) = self.take_for_scene_exit(id)?;
        self.remove_ego_observers(id);
        self.queue_scene_exit(exit, move |stamp|
            vec![event(stamp, "RemoveBalls", vec![Value::list([Value::Int(id)])])]);
        if b.char_id.is_some() { self.registry.pump_queue(self.id, self.population()); }
        Some(b)
    }
    pub fn destroy_ball(&mut self, id: i64) -> Option<Ball> {
        let (mut b, exit) = self.take_for_scene_exit(id)?;
        b.pos = b.position_at(self.ms);
        self.queue_scene_exit(exit, move |stamp| vec![
            event(stamp, "TerminalPlayDestructionEffect", vec![Value::Int(id), Value::Int(DESTRUCTION_EFFECT_EXPLOSION)]),
            event(stamp, "RemoveBalls", vec![Value::list([Value::Int(id)])]),
        ]);
        if b.char_id.is_some() { self.registry.pump_queue(self.id, self.population()); }
        Some(b)
    }
    pub fn set_health(&mut self, id: i64, health: [f64; DAMAGE_LAYER_COUNT]) {
        let Some(b) = self.balls.get_mut(&id) else { return };
        b.health = health;
        let damage = b.damage_state();
        let character = b.char_id;
        self.publish_destiny(id, |stamp| vec![event(stamp, "OnDamageStateChange", vec![Value::Int(id), damage.clone()])]);
        // Every receiving actor evaluates live scene visibility; fleet
        // ownership is consulted only by the deferred worker.
        if let (Some(character), Some(world)) = (character, self.world.upgrade()) {
            let system = self.id;
            self.defer(async move {
                crate::services::fleets::damage::route(world, character, system, id, damage, None).await;
            });
        }
    }
    /// Return the largest active compressor range supporting this type.
    pub(crate) fn compressor_range(&self, ship: i64, type_id: i32, sde: &crate::sde::Sde) -> Option<f64> {
        let ty = sde.types.get(type_id)?;
        self.compressor_range_matching(ship, |list| sde.typelists.contains(list, type_id, ty.group_id, ty.category_id))
    }
    pub(super) fn compressor_range_matching(&self, ship: i64, matches: impl Fn(i64) -> bool) -> Option<f64> {
        self.ball(ship)?;
        self.compressors.get(&ship)?.values()
            .filter(|entry| !entry.generation.load(std::sync::atomic::Ordering::Acquire) && matches(entry.list))
            .map(|entry| entry.range)
            .reduce(f64::max)
    }
    /// Advertise the union of active compressor lists on the ship's slim item.
    pub(crate) fn set_compressor(&mut self, ship: i64, module: i64, generation: Arc<std::sync::atomic::AtomicBool>, list: i64, range: f64) {
        if self.ball(ship).is_none() || list <= 0 || !range.is_finite() || range <= 0.0 { return; }
        self.compressors.entry(ship).or_default().insert(module, Compressor { generation, list, range });
        self.publish_compressors(ship);
    }
    pub(crate) fn remove_compressor(&mut self, ship: i64, module: i64, generation: &Arc<std::sync::atomic::AtomicBool>) {
        let Some(modules) = self.compressors.get_mut(&ship) else { return };
        if !modules.get(&module).is_some_and(|entry| Arc::ptr_eq(&entry.generation, generation)) { return; }
        modules.remove(&module);
        if modules.is_empty() { self.compressors.remove(&ship); }
        self.publish_compressors(ship);
    }
    fn publish_compressors(&mut self, ship: i64) {
        let mut ranges = std::collections::BTreeMap::<i64, f64>::new();
        if let Some(modules) = self.compressors.get(&ship) {
            for entry in modules.values() {
                ranges.entry(entry.list).and_modify(|range| *range = range.max(entry.range)).or_insert(entry.range);
            }
        }
        let Some(ball) = self.balls.get_mut(&ship) else { return };
        ball.slim.retain(|(key, _)| *key != "compression_facility_typelists");
        if !ranges.is_empty() {
            ball.slim.push(("compression_facility_typelists", Value::Dict(ranges.into_iter().map(|(list, range)| (Value::Int(list), Value::Float(range))).collect())));
        }
        let slim = ball.slim_item();
        self.publish_destiny(ship, |stamp| vec![event(stamp, "OnSlimItemChange", vec![Value::Int(ship), slim])]);
    }
    pub(crate) fn cloak_reactivation_ready(&self, module: i64) -> bool {
        self.cloak_reactivation.get(&module).is_none_or(|&until| until <= self.ms)
    }
    pub(crate) fn cloak_targeting_ready(&self, ship: i64) -> bool {
        self.cloak_targeting.get(&ship).is_none_or(|&until| until <= self.ms)
    }
    pub(crate) fn has_module_cloak(&self, ship: i64) -> bool {
        self.module_cloaks.contains_key(&ship)
    }
    pub(crate) fn release_defender_target(&mut self, target: i64) {
        self.defender_reservations.remove(&target);
    }


    pub(crate) fn cloak_obstructed(&self, ship: i64) -> bool {
        let Some(me) = self.sampled_ball(ship) else { return true };
        self.balls.values().filter(|other| other.id != ship && other.cloak == 0).any(|other|
            dist(me.pos, other.position_at(self.ms)) - f64::from(me.radius) - f64::from(other.radius) < MIN_CLOAKING_DISTANCE)
    }
    pub(crate) fn begin_module_cloak(&mut self, ship: i64, module: i64, generation: Arc<std::sync::atomic::AtomicBool>, reactivation_ms: i64, targeting_ms: i64, guard: impl FnOnce(&System) -> bool + Send + 'static, on_success: Option<Box<dyn FnOnce(&mut System) + Send>>) -> bool {
        if self.module_cloaks.contains_key(&ship) || !self.cloak_reactivation_ready(module)
            || self.ball(ship).is_none_or(|b| b.cloak != 0) { return false; }
        let token = self.capture_ball(ship).expect("cloak ship exists");
        self.module_cloaks.insert(ship, ModuleCloak { module, generation: generation.clone(), reactivation_ms, targeting_ms });
        self.actions.push_back((self.current_time, Box::new(move |s, stamp| {
            if !s.ball_token_current(token) { return; }
            let current = s.module_cloaks.get(&ship).is_some_and(|cloak| Arc::ptr_eq(&cloak.generation, &generation));
            if !current || !guard(s) || s.ball(ship).is_none_or(|b| b.cloak != 0) {
                if current {
                    s.module_cloaks.remove(&ship);
                    generation.store(true, std::sync::atomic::Ordering::Relaxed);
                    if crate::services::effects::lifecycle::force_inactive(module, &generation) {
                        s.fitted_module_active(ship, module, false);
                        s.refresh_projection_target(ship);
                    }
                }
                return;
            }
            s.present_cloak(token, stamp);
            if let Some(ball) = s.balls.get_mut(&ship) {
                ball.cloak = 1;
                ball.cloak_until = 0;
            }
            s.detach_warp_arrival(ship);
            s.invalidate_missile_target(ship);
            s.invalidate_missile_launcher(ship);
            if let Some(on_success) = on_success { on_success(s); }
        })));
        true
    }
    pub(crate) fn stop_module_cloak(&mut self, ship: i64, module: i64, generation: &Arc<std::sync::atomic::AtomicBool>) {
        if self.module_cloaks.get(&ship).is_some_and(|cloak| cloak.module == module && Arc::ptr_eq(&cloak.generation, generation)) {
            self.uncloak(ship);
        }
    }
    /// Destiny has no orientation state: reuse its documented stationary missile fallback.
    pub(crate) fn heading(&self, ship: i64) -> Option<V3> {
        let ball = self.sampled_ball(ship)?;
        let direction = if len(ball.vel) > 0.0 { ball.vel }
            else if ball.mode == MODE_GOTO { sub(ball.goto, ball.pos) }
            else { [0.0, 0.0, -1.0] };
        let magnitude = len(direction);
        Some(if magnitude > 0.0 && magnitude.is_finite() { scale(direction, 1.0 / magnitude) } else { [0.0, 0.0, -1.0] })
    }
    /// Queue carbonengine's SetBallPosition without replacing the ball.
    pub(crate) fn set_ball_position(&mut self, ship: i64, position: V3, guard: impl FnOnce(&Ball, &System) -> bool + Send + 'static) {
        if !position.iter().all(|v| v.is_finite()) { return; }
        self.command(ship, move |ball, stamp, system| {
            if !guard(ball, system) { return Vec::new(); }
            ball.pos = position;
            vec![event(stamp, "SetBallPosition", vec![
                Value::Int(ship), Value::Float(position[0]), Value::Float(position[1]), Value::Float(position[2]),
            ])]
        });
    }
    pub fn uncloak(&mut self, ship: i64) -> bool {
        if self.balls.get(&ship).is_none() || (self.balls[&ship].cloak == 0 && !self.module_cloaks.contains_key(&ship)) || !self.pending_uncloak.insert(ship) { return false; }
        let token = self.capture_ball(ship).expect("uncloak ship exists");
        if let Some(cloak) = self.module_cloaks.remove(&ship) {
            cloak.generation.store(true, std::sync::atomic::Ordering::Relaxed);
            if crate::services::effects::lifecycle::force_inactive(cloak.module, &cloak.generation) {
                self.fitted_module_active(ship, cloak.module, false);
                self.refresh_projection_target(ship);
            }
            if self.balls[&ship].cloak != 0 {
                self.cloak_reactivation.insert(cloak.module, self.ms.saturating_add(cloak.reactivation_ms));
                self.cloak_targeting.insert(ship, self.ms.saturating_add(cloak.targeting_ms));
            }
        }
        self.actions.push_back((self.current_time, Box::new(move |s, stamp| {
            if s.ball_token_current(token) { s.apply_uncloak_with_collisions(ship, stamp, s.scene_action_collisions()); }
        })));
        true
    }
    pub(crate) fn drop_jump_cloak(&mut self, ship: i64) -> bool {
        let pilot = self.balls.get(&ship).filter(|ball| ball.cloak == CLOAK_RESTORE).and_then(|ball| ball.char_id);
        if !self.uncloak(ship) { return false; }
        if let Some(pilot) = pilot {
            self.notify_pilot(pilot, "OnJumpCloakCancelled", vec![Value::Int(ship)]);
        }
        true
    }
    pub(super) fn apply_uncloak_with_collisions(&mut self, ship: i64, stamp: i64, collisions: bool) {
        self.pending_uncloak.remove(&ship);
        let Some(ball) = self.balls.get_mut(&ship) else { return };
        if ball.cloak == 0 { return; }
        ball.cloak = 0;
        ball.cloak_until = 0;
        let token = self.capture_ball(ship).expect("uncloak ball exists");
        self.present_uncloak(token, stamp, collisions);
    }
    /// Enqueue fitted motion only while the projection snapshot remains current.
    pub(crate) fn queue_projection_kinematics(&mut self, target: i64, revision: u64, values: crate::services::effects::propulsion::Kinematics) {
        crate::services::effects::propulsion::queue_kinematics(self, target, values);
        let (stamp, action) = self.actions.pop_back().expect("kinematics queues one action");
        self.actions.push_back((stamp, Box::new(move |s, stamp| {
            if s.projection_revision(target) == revision { action(s, stamp); }
        })));
    }
    pub(super) fn queue_drone_kinematics(&mut self, target: i64, generation: u64, revision: u64,
        owner_projection: Option<(i64, u64)>, values: crate::services::effects::propulsion::Kinematics) {
        crate::services::effects::propulsion::queue_kinematics(self, target, values);
        let (stamp, action) = self.actions.pop_back().expect("kinematics queues one action");
        self.actions.push_back((stamp, Box::new(move |s, stamp| {
            if !s.drone_current(target, generation) {
                if let Some(current) = s.drone(target) { s.invalidate_drone_kinematics(target, current.generation); }
                return;
            }
            if s.projection_revision(target) == revision
                && owner_projection.is_none_or(|(owner, revision)| s.projection_revision(owner) == revision) {
                action(s, stamp);
            } else {
                s.invalidate_drone_kinematics(target, generation);
                s.refresh_projection_target(target);
            }
        })));
    }
    /// Queue a command for the next destiny boundary, retaining its enqueue stamp.
    pub fn command(&mut self, ship: i64, f: impl FnOnce(&mut Ball, i64, &System) -> Vec<Value> + Send + 'static) {
        let token = self.capture_ball(ship);
        self.actions.push_back((self.current_time, Box::new(move |s, stamp| {
            if token.is_some_and(|token| s.ball_token_current(token)) {
                let collisions = s.scene_action_collisions();
                s.apply_command(ship, stamp, collisions, f);
            }
        })));
    }

    /// Apply one stamped command inside the ordered stream. The earlier
    /// prefix's ready work settles first and the source's old published home
    /// is captured; the ball is changed, returned and its ready position
    /// refreshed, then the shared delta settles so new holders arrive only
    /// through the result-state Add. The command's own events go only to
    /// bindings that held the exact token before and still hold it.
    fn apply_command(&mut self, ship: i64, stamp: i64, collisions: bool,
        f: impl FnOnce(&mut Ball, i64, &System) -> Vec<Value>) {
        self.settle_scene(stamp, collisions, false);
        let token = self.ball_token(ship);
        let (home, global) = token.map_or((None, false), |token| self.scene_home(token));
        let Some(mut ball) = self.balls.remove(&ship) else { return };
        ball.last_ms = self.current_time * MILLISECONDS_PER_SECOND;
        let old_warp = (ball.mode == MODE_WARP).then_some(
            (ball.effect_stamp, ball.goto, ball.warp.min_range.to_bits(), ball.warp.factor));
        let events = f(&mut ball, stamp, self);
        if old_warp.is_some_and(|old| ball.mode != MODE_WARP
            || old != (ball.effect_stamp, ball.goto, ball.warp.min_range.to_bits(), ball.warp.factor)) {
            self.cancel_warp_intent(ship);
        }
        ball.interpolation_anchor_ms = self.ms;
        ball.old_pos = ball.pos;
        ball.old_vel = ball.vel;
        ball.segment_acceleration = None;
        self.balls.insert(ship, ball);
        self.refresh_scene_location(ship);
        self.settle_scene(stamp, collisions, false);
        if !events.is_empty() {
            if let Some(token) = token {
                self.emit_destiny(DestinyAudience::Retained { token, bubble: home, global }, events);
            }
        }
    }
}
#[cfg(all(test, any(feature = "tests-combat", feature = "tests-effects", feature = "tests-lua", feature = "tests-runtime", feature = "tests-ships", feature = "tests-slash", feature = "tests-social", feature = "tests-space")))] pub(crate) mod tests;
