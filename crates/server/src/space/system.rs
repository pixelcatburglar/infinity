//! Actor-owned ballpark membership, visibility, actions, motion and timers,
//! serving destiny updates consumed by `eve/client/script/remote/michelle.py`.
//! Celestials are supplied from the SDE; dynamic state and timers are in memory.
//! Changed pilot positions are saved to `game.items` by the snapshot worker.
//!
//! One FIFO holds captured AddBalls updates and mode, propulsion, cloak and
//! missile-launch actions. Membership changes immediately, while AddBalls
//! captures its wire state at enqueue. An action enqueued at logical time C is
//! stamped C and applied at the next boundary before evolution to C+1.
//! `current_time` starts at load-time Unix seconds and advances once per fixed
//! one-second evolution, independently of the wall clock. Timers and NPC
//! decisions run on 100 ms housekeeping ticks; snapshots follow motion.
//! Explicit pilot actions cancel jump cloak once; expiry uses the raw uncloak queue.
//! Uncloak batches restore the ball and clear its client shader for every observer.
//! After more than five due steps, catchup clamps to one logical step and the
//! next deadline is the next whole wall second, an intentional departure from
//! native cadence. The clamped interpolation anchor is actual execution time;
//! the logical counter is not reset to wall time.
//!
//! DB effects run on workers; timers, warp-exit callbacks and non-position
//! dynamic state, including projected effects, do not survive restart. Legacy
//! sphere collisions do not emit bumps. Taking, replacing or destroying an NPC
//! releases its in-memory combat health and capacitor state; temporary player
//! removal preserves that state.
//! Live compressor ranges use actor-owned module generations and static type lists.
//! Taking a ball also ends its in-memory ECM lockout before its pilot leaves.
//! Rack heat and overload membership are transient and are cleared when a ship leaves.
use super::*;
use std::collections::{BTreeMap, HashSet, VecDeque};
use std::future::Future;
#[path = "effects.rs"] mod effects;
#[path = "boarding.rs"] pub(crate) mod boarding;
use effects::Tractor;

/// carbonengine destiny Ballpark.cpp tick catchup policy.
const MAX_CATCHUP_STEPS: i64 = 5;
/// Destiny destruction effect EXPLOSION.
const DESTRUCTION_EFFECT_EXPLOSION: i64 = 3;
/// Server policy for targeted missiles: destiny/src/Thunkers.cpp LaunchMissile
/// uses aimed launches to initialize direction toward an uncloaked target.
const MISSILE_AIMED_LAUNCH: bool = true;
/// Server policy for ordinary missiles; destiny/src/Thunkers.cpp sets ball massiveness from this flag.
const MISSILE_MASSIVE: bool = false;
/// brawl/client/war_fog_svc.py:58 uncloak effect duration in milliseconds.
const UNCLOAK_FX_DURATION_MS: i64 = 5;
pub(crate) type Timer = Box<dyn FnOnce(&mut System) + Send>;
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

#[derive(Default)]
pub struct System {
    pub(super) id: i64,
    balls: HashMap<i64, Ball>,
    pilots: HashMap<i64, i64>,
    observers: HashMap<i64, (i64, DestinyTx)>,
    registry: Arc<Pilots>,
    pub(super) world: std::sync::Weak<World>,
    target_states: HashMap<i64, TargetState>,
    free_ids: Vec<i64>,
    collision_scratch: Vec<collision::Snapshot>,
    exited_ids: Vec<i64>,
    /// Celestial warp-in points (warpin.rs), by celestial ID.
    warp_ins: HashMap<i64, V3>,
    pending: Option<HashMap<i64, Vec<Value>>>,
    timers: BTreeMap<(i64, u64), Timer>,
    warp_exits: HashMap<i64, Timer>,
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
    pub(super) effect_jobs: Option<mpsc::UnboundedSender<npc::EffectJob>>,
    pub(super) deferred_jobs: Option<mpsc::UnboundedSender<npc::NpcFuture>>,
    pub(super) groups: HashMap<u64, npc::SpawnGroup>,
    pub(super) member_groups: HashMap<i64, std::collections::HashSet<u64>>,
    pub(super) due_brains: Vec<i64>,
    pub(super) due_drones: Vec<i64>,
    pub(super) drone_aggressors: HashMap<i64, drone::automatic::Aggressors>,
    pub(super) automatic_drones: Vec<i64>,
    pub(super) automatic_targets: Vec<i64>,
    pub(super) drone_announcements: Vec<i64>,
    pub(super) drone_launch_acks: Vec<tokio::sync::oneshot::Sender<()>>,
    pub(super) snapshot_context: Option<snapshot::Context>,
    pub(super) projection_sde: Option<Arc<Sde>>,
    pub(crate) heat: crate::services::effects::heat::HeatState,
    pub(super) projections: HashMap<i64, Vec<crate::services::effects::projection::Projection>>,
    pub(super) projection_targets: HashMap<i64, i64>,
    pub(super) projection_revisions: HashMap<i64, u64>,
    pub(super) snapshot_positions: HashMap<i64, (u64, V3)>,
    pub(super) snapshot_generation: u64,
    pub(super) snapshot_due: i64,
}
impl System {
    pub(crate) fn sde(&self) -> Option<&crate::sde::Sde> { self.projection_sde.as_deref() }
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
        self.schedule_at(self.ms.saturating_add(i64::try_from(delay_ms).unwrap_or(i64::MAX)), f);
    }
    /// Queue an absolute Unix-millisecond deadline, ordered by insertion on ties.
    pub fn schedule_at(&mut self, due: i64, f: impl FnOnce(&mut System) + Send + 'static) {
        let sequence = self.timer_sequence;
        self.timer_sequence = sequence.checked_add(1).expect("space timer sequence exhausted");
        self.timers.insert((due, sequence), Box::new(f));
    }
    /// Actor simulation time in Unix milliseconds.
    pub fn now_ms(&self) -> i64 { self.ms }
    /// Submit owned async work to the concurrent worker outside the actor.
    pub fn defer(&self, job: impl Future<Output = ()> + Send + 'static) {
        if self.deferred_jobs.as_ref().is_none_or(|tx| tx.send(Box::pin(job)).is_err()) {
            tracing::warn!(system = self.id, "space effects worker unavailable");
        }
    }
    pub fn warp_in(&self, id: i64) -> Option<V3> { self.warp_ins.get(&id).copied() }
    /// Queue cancellation of any previous landing intent before a new warp request.
    pub fn clear_warp_exit(&mut self, id: i64) {
        self.actions.push_back((self.current_time, Box::new(move |s, _| { s.warp_exits.remove(&id); })));
    }

    /// Register the latest landing intent after its warp command in the FIFO.
    /// The registration must follow a valid warp request in the same FIFO.
    /// The production service validates before uncloak, clearing, and registration.
    /// A valid duplicate native request may replace the latest consumer's intent.
    pub fn on_warp_exit(&mut self, id: i64, f: impl FnOnce(&mut System) + Send + 'static) {
        self.actions.push_back((self.current_time, Box::new(move |s, _| {
            match s.balls.get(&id).map(|b| b.mode) {
                Some(MODE_WARP) => { s.warp_exits.insert(id, Box::new(f)); }
                Some(MODE_GOTO) => f(s),
                _ => {}
            }
        })));
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
        self.pilots.values().filter_map(|id| self.ball(*id))
    }
    pub(crate) fn persistent_balls(&self) -> impl Iterator<Item = &Ball> {
        self.snapshot_positions.keys().filter_map(|id| self.ball(*id))
    }

    pub fn pilot_ball(&self, char_id: i64) -> Option<&Ball> {
        self.pilots.get(&char_id).and_then(|id| self.ball(*id))
    }
    /// Actor-owned visibility, independent of client ballpark delivery.
    pub(crate) fn visible_to(&self, character: i64, ball: i64) -> bool {
        let Some(ego) = self.pilot_ball(character) else { return false };
        self.ball(ball).is_some_and(|target| target.cloak == 0 || target.id == ego.id)
    }
    fn pilot_observer(&self, character: i64) -> Option<&(i64, DestinyTx)> {
        let ego = self.pilot_ball(character)?.id;
        self.observers.values().find(|(observer_ego, tx)| *observer_ego == ego && !tx.is_closed())
    }
    /// Current ballpark visibility for a pilot with a live matching observer.
    pub(crate) fn fleet_broadcast_visible(&self, character: i64, ball: i64) -> bool {
        self.pilot_observer(character).is_some() && self.visible_to(character, ball)
    }
    /// Snapshot a pilot's real actor-owned damage state and visibility.
    pub(crate) fn fleet_damage_snapshot(&self, character: i64) -> Option<(i64, u8, Value)> {
        let ball = self.pilot_ball(character)?;
        Some((ball.id, ball.cloak, ball.damage_state()))
    }

    /// Targeted ballpark function, only while the destination observer owns its ego.
    pub(crate) fn fleet_damage_update(&self, watcher: i64, source_system: i64, ship: i64, cloak: u8, damage: Value) {
        let Some((ego, _)) = self.pilot_observer(watcher) else { return };
        if self.id == source_system && (cloak == 0 || ship == *ego) { return; }
        // michelle.py:875-879,1594-1599: a ballpark function, not a service event.
        let update = event(self.current_time, "OnFleetDamageStateChange", vec![Value::Int(ship), damage]);
        self.registry.notify_pilot(watcher, "DoDestinyUpdate", vec![Value::list([update]), Value::Bool(false)]);
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
            self.pilots.insert(char_id, ball.id);
            self.snapshot_generation = self.snapshot_generation.checked_add(1).expect("snapshot generation exhausted");
            self.snapshot_positions.insert(ball.id, (self.snapshot_generation, ball.pos));
            if self.snapshot_due == 0 {
                self.snapshot_due = self.ms.saturating_add(crate::config::game().space.position_snapshot_secs.saturating_mul(MILLISECONDS_PER_SECOND));
            }
        }
        self.balls.insert(ball.id, ball);
    }

    pub fn take(&mut self, id: i64) -> Option<Ball> {
        self.drone_controller_lost(id);
        self.drone_removed(id);
        self.cleanup_tractors(id);
        self.warp_exits.remove(&id);
        if self.balls.contains_key(&id) {
            crate::services::combat::jamming::clear(self, id);
        }
        let ball = self.balls.remove(&id)?;
        crate::services::effects::heat::remove_ship(self, id);
        self.compressors.remove(&id);
        self.module_cloaks.remove(&id);
        self.cloak_targeting.remove(&id);
        self.remove_projections_from(id);
        self.remove_projections_on_departure(id);
        self.stop_all_followers(id);
        self.snapshot_positions.remove(&id);
        if let Some(char_id) = ball.char_id {
            if self.pilots.get(&char_id) == Some(&id) {
                self.pilots.remove(&char_id);
            }
        }
        if self.brains.contains_key(&id) {
            crate::services::combat::forget_ship(id);
        }
        self.npc_removed(id);
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
    fn population(&self) -> usize {
        self.balls.values().filter(|b| b.char_id.is_some()).count()
    }
    pub fn request_entry(&self, char_id: i64) -> Result<(), i64> {
        self.registry.request_entry(self.id, char_id, self.population())
    }
    pub(super) fn is_cold(&self, collisions: bool) -> bool {
        self.actions.is_empty()
            && !self.timers.first_key_value().is_some_and(|(&(due, _), _)| due <= self.ms)
            && self.module_cloaks.is_empty() && self.brains.is_empty() && self.drones.is_empty() && self.observers.is_empty()
            && !crate::services::effects::heat::active(&self.heat) && !self.balls.values().any(|b|
            (b.free && (b.mode != MODE_STOP || b.vel != [0.0; 3]))
            || (b.cloak != 0 && b.cloak_until > 0))
            && !collision::stationary_contact(&self.balls, collisions)
    }
    pub fn tick(&mut self, ms: i64) {
        self.tick_with_collisions(ms, collision::enabled());
    }
    pub(super) fn tick_with_collisions(&mut self, ms: i64, collisions: bool) {
        self.observers.retain(|_, (_, tx)| !tx.is_closed());
        self.ms = ms;
        self.expire_projections();
        let next_tick_ms = *self.next_tick_ms.get_or_insert(MILLISECONDS_PER_SECOND);
        let due = if ms >= next_tick_ms {
            (ms - next_tick_ms) / MILLISECONDS_PER_SECOND + 1
        } else { 0 };
        let steps = if due > MAX_CATCHUP_STEPS {
            tracing::warn!(system = self.id, due, "destiny tick catchup clamped");
            self.next_tick_ms = Some(next_aligned_ms(ms, MILLISECONDS_PER_SECOND));
            1
        } else {
            self.next_tick_ms = Some(next_tick_ms + due * MILLISECONDS_PER_SECOND);
            due
        };
        if !self.is_cold(collisions) { self.pending = Some(HashMap::new()); }
        for step in 0..steps {
            let count = self.actions.len();
            for _ in 0..count {
                let (stamp, action) = self.actions.pop_front().unwrap();
                action(self, stamp);
            }
            if self.balls.values().any(|b| b.free && (b.mode != MODE_STOP || b.vel != [0.0; 3]))
                || collision::stationary_contact(&self.balls, collisions) {
                self.target_states.clear();
                self.target_states.extend(self.balls.iter().map(|(&id, b)| (id, TargetState {
                    pos: b.pos, vel: b.vel, radius: b.radius,
                })));
                self.free_ids.clear();
                self.free_ids.extend(self.balls.iter().filter_map(|(&id, b)| b.free.then_some(id)));
                self.free_ids.sort_unstable();
                let logical_ms = (self.current_time + 1) * MILLISECONDS_PER_SECOND;
                let anchor_ms = if due > MAX_CATCHUP_STEPS { ms }
                    else { next_tick_ms + step * MILLISECONDS_PER_SECOND };
                for index in 0..self.free_ids.len() {
                    let id = self.free_ids[index];
                    let b = self.balls.get_mut(&id).unwrap();
                    let aligning = b.mode == MODE_WARP && !b.is_warping();
                    b.evolve_behavior_for_ball(logical_ms, anchor_ms, &self.target_states, self.current_time);
                    let activated = aligning && b.is_warping();
                    self.target_states.get_mut(&id).unwrap().vel = b.vel;
                    if activated { self.stop_all_followers(id); self.drone_controller_lost(id); }
                }
                collision::freeze(&mut self.collision_scratch, &self.balls, collisions);
                collision::apply(&self.collision_scratch, &mut self.balls);
                self.exited_ids.clear();
                for snapshot in self.collision_scratch.iter().filter(|snapshot| snapshot.free) {
                    let id = &snapshot.id;
                    let b = self.balls.get_mut(id).unwrap();
                    let was_warping = b.is_warping();
                    b.evolve_position(logical_ms, snapshot.time_factor);
                    if was_warping && b.mode == MODE_STOP {
                        if let Some(char_id) = b.char_id {
                            crate::lua::on_event(crate::lua::events::GameEvent::WarpArrived {
                                char_id, system_id: self.id,
                            });
                        }
                        if !collisions || self.warp_exits.contains_key(id) {
                            self.exited_ids.push(*id);
                        }
                    }
                }
                for index in 0..self.exited_ids.len() {
                    let id = self.exited_ids[index];
                    // Native WarpDistance restores massiveness during the completed step
                    // (Ballpark.cpp:4330,4355); correct it at the next pre-tick.
                    if let Some(update) = collision::reset_event(id, self.current_time + 1, collisions) {
                        self.broadcast(id, vec![update]);
                    }
                    if let Some(callback) = self.warp_exits.remove(&id) { callback(self); }
                }
            }
            self.current_time += 1;
        }
        if self.snapshot_positions.is_empty() {
            self.snapshot_due = 0;
        } else if self.snapshot_due != 0 && ms >= self.snapshot_due {
            self.snapshot_due = ms.saturating_add(crate::config::game().space.position_snapshot_secs.saturating_mul(MILLISECONDS_PER_SECOND));
            snapshot::schedule(self);
        }
        if self.is_cold(collisions) {
            self.pending = None;
            return;
        }
        let filetime = crate::client::filetime_from_unix_ms(ms);
        let expired: Vec<_> = self.balls.values().filter(|b|
            b.cloak != 0 && b.cloak_until > 0 && b.cloak_until <= filetime).map(|b| b.id).collect();
        for id in expired { self.uncloak(id); }
        let obstructed: Vec<_> = self.module_cloaks.keys().copied().filter(|&ship| self.cloak_obstructed(ship)).collect();
        for ship in obstructed { self.uncloak(ship); }
        while self.timers.first_key_value().is_some_and(|(&(due, _), _)| due <= ms) {
            let (_, timer) = self.timers.pop_first().unwrap();
            timer(self);
        }
        crate::services::effects::heat::tick(self);
        self.think_npcs();
        self.think_drones();
        if let Some(pending) = self.pending.take() {
            for (client, events) in pending {
                if let Some((_, tx)) = self.observers.get(&client) {
                    if tx.send(events).is_err() { self.observers.remove(&client); }
                }
            }
        }
        let mut announcements = std::mem::take(&mut self.drone_announcements);
        for id in announcements.iter().copied() { self.announce_drone(id); }
        announcements.clear();
        self.drone_announcements = announcements;
        let mut acks = std::mem::take(&mut self.drone_launch_acks);
        for ack in acks.drain(..) { let _ = ack.send(()); }
        self.drone_launch_acks = acks;
    }
    fn deliver(&mut self, about: Option<(i64, u8)>, events: Vec<Value>) {
        self.observers.retain(|&client, (ego, tx)| {
            if tx.is_closed() { return false; }
            if about.is_some_and(|(id, cloak)| cloak != 0 && id != *ego) { return true; }
            if let Some(pending) = &mut self.pending {
                pending.entry(client).or_default().extend(events.iter().cloned());
                true
            } else { tx.send(events.clone()).is_ok() }
        });
    }
    pub fn broadcast(&mut self, about: i64, events: Vec<Value>) {
        let visibility = self.ball(about).map(|b| (b.id, b.cloak));
        self.deliver(visibility, events);
    }
    pub(super) fn queue_drone_announcement(&mut self, id: i64) {
        self.actions.push_back((self.current_time, Box::new(move |s, _| s.drone_announcements.push(id))));
    }
    pub(super) fn queue_drone_launch_ack(&mut self, ack: tokio::sync::oneshot::Sender<()>) {
        self.actions.push_back((self.current_time, Box::new(move |s, _| s.drone_launch_acks.push(ack))));
    }
    pub fn add_ship(&mut self, mut ball: Ball) {
        ball.last_ms = self.current_time * MILLISECONDS_PER_SECOND;
        if ball.cloak == 0 {
            let update = add_balls_event(self.current_time, &ball, collision::enabled());
            self.actions.push_back((self.current_time, Box::new(move |s, _| s.deliver(None, vec![update]))));
        }
        self.insert(ball);
    }
    /// Insert a neutral missile now, then launch and start its impact timer at the next destiny boundary.
    pub fn launch_missile(&mut self, ball: Ball, target: i64, launcher: i64, delay_ms: u64,
        impact: impl FnOnce(&mut System) + Send + 'static) {
        let id = ball.id;
        self.add_ship(ball);
        self.actions.push_back((self.current_time, Box::new(move |s, stamp| {
            let Some(mut ball) = s.balls.remove(&id) else { return };
            ball.last_ms = s.current_time * MILLISECONDS_PER_SECOND;
            ball.interpolation_anchor_ms = s.ms;
            let owner = launcher.checked_abs().and_then(|owner_id| {
                if owner_id == id { Some(&ball) } else { s.balls.get(&owner_id) }
            }).map(|b| (b.pos, b.vel, b.max_vel));
            if let Some(owner) = owner.filter(|_| target >= 0) {
                let target_ball = if target == launcher { None } else if target == id {
                    Some(&ball)
                } else { s.balls.get(&target) };
                let target_ball = target_ball.map(|b| (b.id, b.pos, b.radius, b.cloak));
                ball.launch_native_missile(owner, target_ball, launcher, stamp);
            }
            let visibility = (id, ball.cloak);
            s.balls.insert(id, ball);
            let update = event(stamp, "LaunchMissile", vec![
                Value::Int(id), Value::Int(target), Value::Int(launcher),
                Value::Bool(MISSILE_AIMED_LAUNCH), Value::Bool(MISSILE_MASSIVE),
            ]);
            s.deliver(Some(visibility), vec![update]);
            s.schedule(delay_ms, impact);
        })));
    }
    pub fn remove_ball(&mut self, id: i64) -> Option<Ball> {
        let b = self.take(id)?;
        self.observers.retain(|_, (ego, _)| *ego != id);
        if b.cloak == 0 {
            let update = event(self.current_time, "RemoveBalls", vec![Value::list([Value::Int(id)])]);
            self.actions.push_back((self.current_time, Box::new(move |s, _| s.deliver(None, vec![update]))));
        }
        if b.char_id.is_some() { self.registry.pump_queue(self.id, self.population()); }
        Some(b)
    }
    pub fn destroy_ball(&mut self, id: i64) -> Option<Ball> {
        let mut b = self.take(id)?;
        b.pos = b.position_at(self.ms);
        let stamp = self.current_time;
        let visibility = (b.id, b.cloak);
        let events = vec![
            event(stamp, "TerminalPlayDestructionEffect", vec![Value::Int(id), Value::Int(DESTRUCTION_EFFECT_EXPLOSION)]),
            event(stamp, "RemoveBalls", vec![Value::list([Value::Int(id)])]),
        ];
        self.actions.push_back((stamp, Box::new(move |s, _| s.deliver(Some(visibility), events))));
        if b.char_id.is_some() { self.registry.pump_queue(self.id, self.population()); }
        Some(b)
    }
    pub fn set_health(&mut self, id: i64, health: [f64; DAMAGE_LAYER_COUNT]) {
        let Some(b) = self.balls.get_mut(&id) else { return };
        b.health = health;
        let damage = b.damage_state();
        let character = b.char_id;
        let cloak = b.cloak;
        let update = event(self.current_time, "OnDamageStateChange", vec![Value::Int(id), damage.clone()]);
        self.broadcast(id, vec![update]);
        // The actor captures visibility; fleet ownership is consulted only by the deferred worker.
        if let (Some(character), Some(world)) = (character, self.world.upgrade()) {
            let system = self.id;
            self.defer(async move {
                crate::services::fleets::damage::route(world, character, system, id, cloak, damage, None).await;
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
        self.broadcast(ship, vec![event(self.current_time, "OnSlimItemChange", vec![Value::Int(ship), slim])]);
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
    pub(crate) fn cloak_obstructed(&self, ship: i64) -> bool {
        let Some(me) = self.sampled_ball(ship) else { return true };
        self.balls.values().filter(|other| other.id != ship && other.cloak == 0).any(|other|
            dist(me.pos, other.position_at(self.ms)) - f64::from(me.radius) - f64::from(other.radius) < MIN_CLOAKING_DISTANCE)
    }
    pub(crate) fn begin_module_cloak(&mut self, ship: i64, module: i64, generation: Arc<std::sync::atomic::AtomicBool>, reactivation_ms: i64, targeting_ms: i64, guard: impl FnOnce(&System) -> bool + Send + 'static, on_success: Option<Box<dyn FnOnce(&mut System) + Send>>) -> bool {
        if self.module_cloaks.contains_key(&ship) || !self.cloak_reactivation_ready(module)
            || self.ball(ship).is_none_or(|b| b.cloak != 0) { return false; }
        self.module_cloaks.insert(ship, ModuleCloak { module, generation: generation.clone(), reactivation_ms, targeting_ms });
        self.actions.push_back((self.current_time, Box::new(move |s, stamp| {
            let current = s.module_cloaks.get(&ship).is_some_and(|cloak| Arc::ptr_eq(&cloak.generation, &generation));
            if !current || !guard(s) || s.ball(ship).is_none_or(|b| b.cloak != 0) {
                if current {
                    s.module_cloaks.remove(&ship);
                    generation.store(true, std::sync::atomic::Ordering::Relaxed);
                    if crate::services::effects::lifecycle::force_inactive(module, &generation) {
                        s.refresh_projection_target(ship);
                    }
                }
                return;
            }
            s.deliver(None, vec![event(stamp, "CloakBall", vec![Value::Int(ship)])]);
            if let Some(ball) = s.balls.get_mut(&ship) {
                ball.cloak = 1;
                ball.cloak_until = 0;
            }
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
        if let Some(cloak) = self.module_cloaks.remove(&ship) {
            cloak.generation.store(true, std::sync::atomic::Ordering::Relaxed);
            if crate::services::effects::lifecycle::force_inactive(cloak.module, &cloak.generation) {
                self.refresh_projection_target(ship);
            }
            if self.balls[&ship].cloak != 0 {
                self.cloak_reactivation.insert(cloak.module, self.ms.saturating_add(cloak.reactivation_ms));
                self.cloak_targeting.insert(ship, self.ms.saturating_add(cloak.targeting_ms));
            }
        }
        self.actions.push_back((self.current_time, Box::new(move |s, stamp| s.apply_uncloak(ship, stamp))));
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
    fn apply_uncloak(&mut self, ship: i64, stamp: i64) {
        self.apply_uncloak_with_collisions(ship, stamp, collision::enabled());
    }
    pub(super) fn apply_uncloak_with_collisions(&mut self, ship: i64, stamp: i64, collisions: bool) {
        self.pending_uncloak.remove(&ship);
        let Some(ball) = self.balls.get_mut(&ship) else { return };
        if ball.cloak == 0 { return; }
        ball.cloak = 0;
        ball.cloak_until = 0;
        let add = add_balls_event(stamp, ball, collisions);
        let own = event(stamp, "UncloakBall", vec![Value::Int(ship)]);
        let fx = special_fx(stamp, ship, None, None, "effects.Uncloak", 1, 0, UNCLOAK_FX_DURATION_MS, None);
        // Native UncloakBall restores massiveness (Ballpark.cpp:5257-5287).
        let reset = collision::reset_event(ship, stamp, collisions);
        self.observers.retain(|&client, (ego, tx)| {
            if tx.is_closed() { return false; }
            let update = if *ego == ship { own.clone() } else { add.clone() };
            let owner_reset = if *ego == ship { reset.as_ref() } else { None };
            if let Some(pending) = &mut self.pending {
                let updates = pending.entry(client).or_default();
                updates.push(update);
                if let Some(reset) = owner_reset { updates.push(reset.clone()); }
                updates.push(fx.clone());
                true
            } else {
                let mut updates = Vec::with_capacity(2 + usize::from(owner_reset.is_some()));
                updates.push(update);
                if let Some(reset) = owner_reset { updates.push(reset.clone()); }
                updates.push(fx.clone());
                tx.send(updates).is_ok()
            }
        });
    }
    pub fn observe(&mut self, client_id: i64, ego: i64, tx: DestinyTx) -> Value {
        let system_id = self.id;
        let tick = self.current_time;
        let collisions = collision::enabled();
        self.observers.insert(client_id, (ego, tx));
        let mut state = vec![DESTINY_FULLSTATE];
        state.extend_from_slice(&(tick as i32).to_le_bytes());
        let mut slims = Vec::new();
        let mut damage = Vec::new();
        for b in self.balls.values().filter(|b| b.cloak == 0 || b.id == ego) {
            b.write(&mut state, collisions);
            slims.push(b.slim_item());
            if b.free {
                damage.push((Value::Int(b.id), b.damage_state()));
            }
        }
        let sol = Value::instance(
            "eve.common.script.util.slimItem.SlimItem",
            [("itemID", Value::Int(system_id)), ("typeID", Value::Int(crate::consts::TYPE_SOLAR_SYSTEM.into())), ("ownerID", Value::Int(1))],
        );
        let bag = Value::instance(
            "utillib.KeyVal",
            [
                ("state", Value::str(state)),
                ("ego", Value::Int(ego)),
                ("slims", Value::List(slims)),
                ("damageState", Value::Dict(damage)),
                ("effectStates", Value::list([])),
                ("allianceBridges", Value::list([])),
                ("droneState", Value::list(self.drone_state_rows(ego))),
                ("solItem", sol),
                ("industryLevel", Value::Int(0)),
                ("researchLevel", Value::Int(0)),
                ("dbuffState", Value::list([])),
            ],
        );

        event(tick, "SetState", vec![bag])
    }
    pub fn unobserve(&mut self, client_id: i64) { self.observers.remove(&client_id); }
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
        self.actions.push_back((self.current_time, Box::new(move |s, stamp| {
            let Some(mut ball) = s.balls.remove(&ship) else { return };
            ball.last_ms = s.current_time * MILLISECONDS_PER_SECOND;
            let old_warp = (ball.mode == MODE_WARP).then_some(
                (ball.effect_stamp, ball.goto, ball.warp.min_range.to_bits(), ball.warp.factor));
            let events = f(&mut ball, stamp, s);
            if old_warp.is_some_and(|old| ball.mode != MODE_WARP
                || old != (ball.effect_stamp, ball.goto, ball.warp.min_range.to_bits(), ball.warp.factor)) {
                s.warp_exits.remove(&ship);
            }
            ball.interpolation_anchor_ms = s.ms;
            ball.old_pos = ball.pos;
            ball.old_vel = ball.vel;
            ball.segment_acceleration = None;
            let visibility = (ball.id, ball.cloak);
            s.balls.insert(ship, ball);
            if !events.is_empty() { s.deliver(Some(visibility), events); }
        })));
    }
}
#[cfg(test)] pub(crate) mod tests;
