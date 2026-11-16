//! Per-system ball state, destiny wire snapshots and event broadcasting.
//! Observer snapshots and incremental events use one ordered Destiny channel.
//! `eve/client/script/remote/michelle.py:491-532` consumes destiny updates;
//! `eve/client/script/parklife/autopilot.py:260-261,329-332` consumes gate
//! destinations. The binary layout follows carbonengine destiny `Thunkers.cpp`
//! WriteBallToStream, and warp constants refer to `Ballpark.cpp` SetupWarpConstants.
//! Ball wire encoding is implemented in `wire.rs`.
//!
//! Celestials come from the SDE; persistent unpiloted hulls are restored from
//! game items. Changed persistent positions are saved periodically and at shutdown.
//! Pilot presence is process-local and uses live notification channels; it does not
//! coordinate presence across server processes.
//! Only player-piloted ships set the explicit interactive ball field.
//!
//! The actor runs wall-aligned 100 ms housekeeping, timers and NPC decisions;
//! ball kinetics advance in fixed one-second steps. Player and NPC lock timing
//! is controlled by the combat configuration. The actor-owned `current_time`
//! is the integer destiny stamp: an action enqueued at C is stamped C and applied
//! at the next boundary before evolution to C+1. Wire snapshots use whole integer
//! stamps. Between boundaries `Ball::position_at`, `System::sample_position` and
//! `System::sampled_ball` sample the completed segment from its old state and
//! frozen inputs, using the same native `Ballpark::Integrate` arithmetic as ticks
//! and a wall-clock anchor, not fractional destiny stamps or separate substeps.
//! Sampled balls carry position and velocity from the same instant.
//! Observer bindings partition each system's scene into coarse grid bubbles;
//! the initial SetState and every later Add/Remove ride the same ordered
//! channel through one boundary projection per bubble. Five production call
//! sites bind an observer through `send_state`: the beyonce session bind,
//! `UpdateStateRequest`, boarding finish, a same-system teleport and the
//! post-loss capsule.
//!
//!
//! GOTO stores an absolute point; direction commands project from the current
//! position while their events retain the raw direction. `Ballpark::WarpTo`
//! (`Ballpark.cpp:4102-4113`) falls back to GotoPoint for a raw destination
//! strictly below 100 km, without WarpFX.
//!
//! Missile flight uses native steering and actor-owned target contact and expiry.
//! Warp follows native alignment and acceleration. Legacy scalar-sphere collisions
//! are authoritative; bump events, orientations, continuous native
//! formation-follow, miniballs, AddBalls2 and unsupported ball modes are not
//! implemented.
//! Shared vector distances are defined here for simulation and service callers.
//! Shared special effects follow fxSequencer.OnSpecialFX's 14-field protocol.
//! GM slash commands can batch-preload public solar systems through
//! `preload`; it reuses this module's one cold-load authority without adding
//! a second initialization path or changing actor lifetimes.

use infinity_core::consts::{AU, CATEGORY_SHIP, DAMAGE_LAYER_COUNT, FILETIME_SECOND, SECONDS_PER_MINUTE};
use crate::db::Db;
use crate::sde::Sde;
use anyhow::Result;
use marshal::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

mod actor;
pub(crate) mod containment;
mod attribution;
pub(crate) mod asteroids;
pub(crate) mod ship_cosmetics;
pub(crate) mod system;
mod projections;
pub(crate) mod preload;
mod npc;
pub(crate) mod drone;
pub(crate) mod combat;
pub(crate) mod damageable;
pub(crate) mod sovereignty;
pub(crate) mod combat_facts;
pub(crate) mod targeting;
mod warpin;
pub(crate) mod snapshot;
pub(crate) mod bounties;
pub(crate) mod unload;
mod persistent_ships;
mod stop;
mod goto;
mod follow;
mod orbit;
mod missile;
pub(crate) mod collision;
mod warp;
mod wire;
mod loading;
pub use warp::{WarpOutcome, WarpState};
pub use npc::{HOSTILE_TO_ALL, NpcAction, NpcBrain, NpcConfig, NpcEffects, NpcFuture};
pub use system::System;
use actor::SystemHandle;

// DSTBALLMODE
pub const MODE_GOTO: u8 = 0;
pub const MODE_FOLLOW: u8 = 1;
pub const MODE_STOP: u8 = 2;
pub const MODE_WARP: u8 = 3;
pub const MODE_ORBIT: u8 = 4;
pub const MODE_MISSILE: u8 = 5;
pub const MODE_RIGID: u8 = 11;
// BALLFLAGS
const FLAG_FREE: u8 = 0x01;
const FLAG_GLOBAL: u8 = 0x02;
const FLAG_MASSIVE: u8 = 0x04;
const FLAG_INTERACTIVE: u8 = 0x08;
// DSTCLOAKMODE (IDstConstants.h): the session-change cloak a ship arrives with.
pub const CLOAK_RESTORE: u8 = 2;
/// appConst.py: minCloakingDistance, metres from a surrounding surface.
pub(crate) const MIN_CLOAKING_DISTANCE: f64 = 2_000.0;
const DESTINY_FULLSTATE: u8 = 0;
const DESTINY_BALLS: u8 = 1;

/// SI time conversion.
const MILLISECONDS_PER_SECOND: i64 = 1000;
/// SI distance conversion.
const METRES_PER_KILOMETRE: f64 = 1000.0;
/// SI mass conversion: kilograms per tonne.
const KILOGRAMS_PER_TONNE: f64 = 1000.0;
/// Carbonengine Ballpark::GotoDirection projection distance.
const GOTO_DIRECTION_REACH: f64 = 1e17;
 
/// Existing server damage-state placeholder policy.
const DAMAGE_STATE_SHIELD_RECHARGE_TAU: f64 = 100_000.0;
/// Carbonengine Ballpark constructor friction.
const DESTINY_FRICTION: f64 = 1e6;
/// Carbonengine Ballpark::Integrate Taylor-series threshold.
const INTEGRATE_TAYLOR_THRESHOLD: f64 = 1e-10;
/// Carbonengine Potential fixed-body effective mass, also used for rigid celestials.
const CELESTIAL_STATIC_MASS: f64 = 1e34;


pub type V3 = [f64; 3];
#[derive(Clone, Copy)]
struct TargetState {
    pos: V3,
    vel: V3,
    radius: f32,
}


pub(crate) fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
pub(crate) fn add(a: V3, b: V3) -> V3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}
pub(crate) fn scale(a: V3, s: f64) -> V3 {
    [a[0] * s, a[1] * s, a[2] * s]
}
pub(crate) fn cross(a: V3, b: V3) -> V3 {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}
pub(crate) fn dot(a: V3, b: V3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn len(a: V3) -> f64 {
    (a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt()
}
/// Euclidean distance between two positions.
pub fn dist(a: V3, b: V3) -> f64 {
    len(sub(a, b))
}
/// Carbonengine Vector3d normalization preserves a zero vector.
pub(crate) fn norm(a: V3) -> V3 {
    let l = len(a);
    if l == 0.0 { a } else { scale(a, 1.0 / l) }
}

/// Convert milliseconds to seconds.
pub fn ms_to_seconds(ms: f64) -> f64 { ms / MILLISECONDS_PER_SECOND as f64 }
/// Convert minutes to seconds without discarding fractional minutes.
pub fn minutes_to_seconds(minutes: f64) -> f64 { minutes * SECONDS_PER_MINUTE as f64 }
/// Convert seconds to milliseconds without discarding fractions.
pub fn seconds_to_ms_f64(seconds: f64) -> f64 { seconds * MILLISECONDS_PER_SECOND as f64 }
/// Convert seconds to whole milliseconds.
pub fn seconds_to_ms(seconds: f64) -> u64 { seconds_to_ms_f64(seconds) as u64 }
/// Convert kilograms to tonnes.
pub fn kg_to_tonnes(kg: f64) -> f64 { kg / KILOGRAMS_PER_TONNE }
/// Convert metres to kilometres.
pub fn metres_to_km(metres: f64) -> f64 { metres / METRES_PER_KILOMETRE }
/// Shared player and NPC targeting duration from effective dogma attributes.
pub fn lock_time_ms(scan_resolution: f64, signature_radius: f64) -> u64 {
    let combat = &crate::config::game().combat;
    let signature = signature_radius.max(1.0).asinh();
    seconds_to_ms((combat.lock_time_factor / (scan_resolution.max(1.0) * signature * signature))
        .clamp(combat.lock_time_min_secs, combat.lock_time_max_secs))
}

/// Simulation clock in Unix milliseconds (not a Destiny event stamp).
pub fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis() as i64
}
/// Strictly next wall-clock boundary for a positive interval in milliseconds.
fn next_aligned_ms(unix_ms: i64, interval_ms: i64) -> i64 {
    unix_ms.saturating_add(interval_ms - unix_ms.rem_euclid(interval_ms))
}


 

#[derive(Clone)]
pub struct Ball {
    pub id: i64,
    pub type_id: i32,
    pub owner_id: i64,
    pub corp_id: i64,
    pub alliance_id: i64,
    pub char_id: Option<i64>,
    pub mode: u8,
    pub radius: f32,
    /// Cached effective pilot-ship dogma signature radius for actor targeting; not serialized to destiny.
    pub signature_radius: Option<f64>,
    pub mass: f64,
    pub max_vel: f32,
    pub agility: f32,
    pub speed_fraction: f32,
    pub pos: V3,
    pub vel: V3,
    /// Start state and frozen inputs of the last completed movement segment.
    pub old_pos: V3,
    pub old_vel: V3,
    pub segment_acceleration: Option<V3>,
    pub goto: V3,
    pub follow_id: i64,
    pub follow_range: f32,
    pub free: bool,
    pub global: bool,
    pub massive: bool,
    /// Destiny isInteractive, independent of movement; Partitionable.cpp AddBall and Thunkers WriteBallToStream.
    pub interactive: bool,
    pub warp: WarpState,
    /// Last integration time in logical destiny milliseconds.
    pub last_ms: i64,
    /// Wall-clock segment anchor, separate from logical `last_ms`; native Ball.h
    /// lines 181-182 retain the old/new interpolation times.
    pub interpolation_anchor_ms: i64,
    /// carbonengine destiny AddBall, LaunchMissile and RealWarp effect stamp, in destiny ticks;
    /// independent of integration milliseconds.
    pub effect_stamp: i64,
    /// Cloak mode (0 = visible). Cloaked balls exist only for their owner.
    pub cloak: u8,
    /// When an arrival cloak lapses (FILETIME), to tell a lapse from a later cloak.
    pub cloak_until: i64,
    /// Shield, armor and structure left, as fractions (damage state).
    pub health: [f64; DAMAGE_LAYER_COUNT],
    /// For missiles: the ship that launched it (destiny's mOwnerId).
    pub launcher: i64,
    /// Extra slim item fields (jumps for gates, names...).
    pub slim: Vec<(&'static str, Value)>,
}

/// Native cached whole-tick exponential factor.
fn integration_factor(mass: f64, friction: f64) -> f64 {
    if mass <= 0.0 { 0.0 } else { (-friction / mass).exp() }
}

/// Carbonengine Ballpark::Integrate and Vector3d reciprocal-division ordering.
fn integrate(position: V3, velocity: V3, acceleration: V3, mass: f64, friction: f64, factor: f64, time: f64) -> (V3, V3) {
    if time == 0.0 { return (position, velocity); }
    let time_factor = if time == 1.0 { factor } else if mass <= 0.0 { 0.0 } else { (-friction / mass * time).exp() };
    let friction2 = friction * friction;
    let ook2 = 1.0 / friction2;
    let ook = 1.0 / friction;
    let position = if friction < INTEGRATE_TAYLOR_THRESHOLD * mass * time {
        scale(add(scale(add(scale(acceleration, -friction * time),
            scale(add(scale(acceleration, time), scale(scale(scale(velocity, friction), 1.0 / mass), time)), friction)), mass),
            scale(position, friction2)), ook2)
    } else {
        scale(add(scale(add(scale(scale(acceleration, mass), time_factor - 1.0),
            scale(sub(add(scale(acceleration, time), velocity), scale(velocity, time_factor)), friction)), mass),
            scale(position, friction2)), ook2)
    };
    let velocity = scale(sub(scale(acceleration, mass),
        scale(sub(scale(acceleration, mass), scale(velocity, friction)), time_factor)), ook);
    (position, velocity)
}

impl Ball {

    /// Stateless position view of the most recently completed movement segment.
    pub fn position_at(&self, t_ms: i64) -> V3 {
        self.state_at(t_ms).0
    }

    /// Sample position and velocity together without changing the tick state.
    fn state_at(&self, t_ms: i64) -> (V3, V3) {
        let elapsed = t_ms.saturating_sub(self.interpolation_anchor_ms).clamp(0, MILLISECONDS_PER_SECOND);
        if self.is_warping() {
            let t = (self.last_ms / MILLISECONDS_PER_SECOND - self.effect_stamp - 1) as f64
                + ms_to_seconds(elapsed as f64);
            let (pos, vel, _) = self.warp_distance(self.old_pos, self.old_vel, t, true);
            return (pos, vel);
        }
        let Some(acceleration) = self.segment_acceleration else { return (self.pos, self.vel); };
        if elapsed == 0 { return (self.old_pos, self.old_vel); }
        if elapsed == MILLISECONDS_PER_SECOND { return (self.pos, self.vel); }
        let mass = self.mass * self.agility as f64;
        integrate(self.old_pos, self.old_vel, acceleration, mass, DESTINY_FRICTION,
            0.0, ms_to_seconds(elapsed as f64))
    }

    fn evolve_behavior_for_ball(&mut self, ms: i64, anchor: i64, others: &HashMap<i64, TargetState>, current_time: i64) {
        if !self.free || ms <= self.last_ms { return; }
        self.interpolation_anchor_ms = anchor;
        if matches!(self.mode, MODE_FOLLOW | MODE_ORBIT | MODE_MISSILE) && !others.contains_key(&self.follow_id) {
            if self.mode == MODE_MISSILE {
                self.massive = false;
                self.goto_direction(self.vel);
            } else if self.mode == MODE_ORBIT && self.interactive { self.goto_direction(self.vel); } else { self.stop(); }
        }
        if self.mode == MODE_STOP { self.evolve_stop(); }
        self.old_pos = self.pos;
        self.old_vel = self.vel;
        self.segment_acceleration = None;
        let mass = self.mass * self.agility as f64;
        let acceleration = if mass <= 0.0 { V3::default() } else {
            match self.mode {
                MODE_GOTO => self.evolve_goto(),
                MODE_FOLLOW => self.evolve_follow(&others[&self.follow_id]),
                MODE_ORBIT => self.evolve_old_style_orbit(&others[&self.follow_id], current_time),
                MODE_MISSILE => self.evolve_missile(&others[&self.follow_id], current_time),
                MODE_WARP => self.evolve_warp(current_time),
                _ => V3::default(),
            }
        };
        self.segment_acceleration = Some(acceleration);
    }

    fn evolve_position(&mut self, ms: i64, full_factor: f64) {
        if !self.free || ms <= self.last_ms {
            self.last_ms = self.last_ms.max(ms);
            return;
        }
        if self.is_warping() {
            let t = (self.last_ms / MILLISECONDS_PER_SECOND - self.effect_stamp) as f64;
            let (pos, vel, exit) = self.warp_distance(self.pos, self.vel, t, false);
            self.pos = pos;
            self.vel = vel;
            if exit {
                self.massive = true;
                self.stop();
                self.old_pos = pos;
                self.old_vel = vel;
                let mass = self.mass * self.agility as f64;
                (self.pos, self.vel) = integrate(pos, vel, self.segment_acceleration.unwrap_or_default(),
                    mass, DESTINY_FRICTION, full_factor, ms_to_seconds((ms - self.last_ms) as f64));
            }
        } else if let Some(acceleration) = self.segment_acceleration {
            let dt = ms_to_seconds((ms - self.last_ms) as f64);
            let mass = self.mass * self.agility as f64;
            (self.pos, self.vel) = integrate(self.pos, self.vel, acceleration, mass, DESTINY_FRICTION,
                full_factor, dt);
        }
        self.last_ms = ms;
    }
}



/// One ordered scene delivery on a client's ballpark channel: a native Destiny
/// event batch, or a scene-bound notification that must keep its position in
/// the channel's order (the fleet snapshot after SetState, Add or Remove).
pub enum SceneDelivery {
    Destiny(Vec<Value>),
    Notify { event: &'static str, args: Vec<Value> },
}
pub type DestinyTx = mpsc::UnboundedSender<SceneDelivery>;
/// A client's notification channel: (event, args) scattered on its client.
pub type NotifyTx = mpsc::UnboundedSender<(String, Vec<Value>)>;

/// Jump keys from the jump queue stay valid this long (s).
pub const JUMP_KEY_LIFE: i64 = 60;

/// Pilots waiting to jump into a full system, and the entry keys already issued.
#[derive(Default)]
struct JumpQueue {
    waiting: Vec<i64>,
    /// charID -> FILETIME the key was issued
    keys: HashMap<i64, i64>,
}


/// AddBalls for one ball.
fn add_balls_event(tick: i64, ball: &Ball, collisions: bool) -> Value {
    let mut state = vec![DESTINY_BALLS];
    state.extend_from_slice(&(tick as i32).to_le_bytes());
    ball.write(&mut state, collisions);
    // michelle.py AddBalls: free balls carry their current three-layer damage.
    let damage = if ball.free { vec![(Value::Int(ball.id), ball.damage_state())] } else { Vec::new() };
    let chunk = Value::tuple([Value::str(state), Value::list([ball.slim_item()]), Value::Dict(damage)]);
    event(tick, "AddBalls", vec![chunk])
}

/// `(stamp, (funcName, args))`
pub fn event(tick: i64, name: &str, args: Vec<Value>) -> Value {
    Value::tuple([Value::Int(tick), Value::tuple([Value::str(name), Value::Tuple(args)])])
}

/// fxSequencer.py: nonpositive durations select the effect classification's default.
pub(crate) const SPECIAL_FX_UNSPECIFIED_DURATION: i64 = -1;

/// Module identity sent on OnSpecialFX: the real fitted item when one fired,
/// or only a compatible type when no module item exists.
#[derive(Clone, Copy)]
pub(crate) struct FxModule {
    pub(crate) item: Option<i64>,
    pub(crate) type_id: i32,
}

/// Encode fxSequencer.OnSpecialFX's 14 arguments without normalizing values.
/// Duration is an Int for space transitions and a Float for module/GM effects.
/// Replay start_time is FILETIME; time_from_start is a phase in 100 ns ticks.
#[allow(clippy::too_many_arguments)]
pub(crate) fn special_fx(
    stamp: i64, ship: i64, module: Option<FxModule>, target: Option<i64>, other_type: Option<i64>,
    guid: &str, offensive: bool, start: i64, active: i64, duration: Value, repeat: i64,
    start_time: Option<i64>, time_from_start: i64, graphic_info: Value,
) -> Value {
    event(stamp, "OnSpecialFX", vec![
        Value::Int(ship),
        module.and_then(|m| m.item).map(Value::Int).unwrap_or(Value::None),
        module.map(|m| Value::Int(i64::from(m.type_id))).unwrap_or(Value::None),
        target.map(Value::Int).unwrap_or(Value::None),
        other_type.map(Value::Int).unwrap_or(Value::None),
        Value::str(guid),
        Value::Bool(offensive),
        Value::Int(start),
        Value::Int(active),
        duration,
        Value::Int(repeat),
        start_time.map(Value::Int).unwrap_or(Value::None),
        Value::Int(time_from_start),
        graphic_info,
    ])
}

/// Shared notification registry and admission queues, safe to use inside an actor.
#[derive(Default)]
pub struct Pilots {
    capacity: usize,
    notify: Mutex<HashMap<i64, (NotifyTx, Option<infinity_core::session::OnlineSession>)>>,
    queues: Mutex<HashMap<i64, JumpQueue>>,
}
impl Pilots {
    pub fn register_pilot(&self, char_id: i64, tx: NotifyTx) {
        crate::poison::lock_or_exit(self.notify.lock()).insert(char_id, (tx, None));
    }

    pub(crate) fn set_online_session(&self, char_id: i64, session: Option<infinity_core::session::OnlineSession>) {
        let mut notify = crate::poison::lock_or_exit(self.notify.lock());
        let Some(entry) = notify.get_mut(&char_id) else { return };
        if let (Some(live), Some(next)) = (entry.1.as_ref(), session.as_ref()) {
            if live.session_id != next.session_id { return; }
        } else if session.is_none() && entry.1.is_some() {
            return;
        }
        entry.1 = session;
    }

    pub(crate) fn online_session(&self, char_id: i64) -> Option<infinity_core::session::OnlineSession> {
        crate::poison::lock_or_exit(self.notify.lock()).get(&char_id).filter(|(tx, _)| !tx.is_closed()).and_then(|(_, session)| session.clone()).filter(|s| s.character_id == char_id)
    }

    pub(crate) fn replace_online_session(&self, character: i64, expected: Option<&infinity_core::session::OnlineSession>, next: Option<infinity_core::session::OnlineSession>) -> bool {
        let mut notify = crate::poison::lock_or_exit(self.notify.lock());
        match notify.get_mut(&character) {
            Some((tx, session)) if !tx.is_closed() => {
                if expected.is_none() || session.as_ref() != expected { return false; }
                if next.as_ref().is_some_and(|s| s.character_id != character || Some(s.session_id) != expected.map(|e| e.session_id)) { return false; }
                *session = next;
                true
            }
            _ => expected.is_none() && next.is_none(),
        }
    }

    pub(crate) fn station_guests(&self, station_id: i64) -> Vec<infinity_core::session::OnlineSession> {
        crate::poison::lock_or_exit(self.notify.lock()).iter().filter_map(|(&id, (tx, session))| {
            session.as_ref().filter(|s| !tx.is_closed() && s.character_id == id && s.station_id == Some(station_id)).cloned()
        }).collect()
    }

    /// Live session snapshot of every online pilot, cloned under the one notify lock. The
    /// result is instantaneous: a session may change or disconnect before the caller uses it.
    /// No ordering is imposed; callers sort outside the lock.
    pub(crate) fn online_sessions(&self) -> Vec<infinity_core::session::OnlineSession> {
        crate::poison::lock_or_exit(self.notify.lock()).iter().filter_map(|(&id, (tx, session))| {
            session.as_ref().filter(|s| !tx.is_closed() && s.character_id == id).cloned()
        }).collect()
    }

    pub(crate) fn unregister_pilot_generation(&self, char_id: i64, session_id: Option<i64>) -> bool {
        let mut notify = crate::poison::lock_or_exit(self.notify.lock());
        let Some((_, session)) = notify.get(&char_id) else { return false };
        if let Some(sid) = session_id {
            if session.as_ref().is_none_or(|live| live.session_id != sid) { return false; }
        }
        notify.remove(&char_id);
        drop(notify);
        for q in crate::poison::lock_or_exit(self.queues.lock()).values_mut() {
            q.waiting.retain(|c| *c != char_id);
        }
        true
    }

    pub fn is_online(&self, char_id: i64) -> bool {
        crate::poison::lock_or_exit(self.notify.lock()).get(&char_id).is_some_and(|(tx, _)| !tx.is_closed())
    }

    pub fn online_pilots(&self) -> Vec<i64> {
        crate::poison::lock_or_exit(self.notify.lock()).iter().filter_map(|(&id, (tx, _))| (!tx.is_closed()).then_some(id)).collect()
    }

    pub fn notify_pilot(&self, char_id: i64, event: &str, args: Vec<Value>) {
        if let Some((tx, _)) = crate::poison::lock_or_exit(self.notify.lock()).get(&char_id) {
            let _ = tx.send((event.to_string(), args));
        }
    }

    /// May `char_id` jump into `system_id` now? `Err(position)` puts them in its jump queue
    /// (jumpQueue.py); a key issued from the queue lets them in within JUMP_KEY_LIFE.
    pub fn request_entry(&self, system_id: i64, char_id: i64, population: usize) -> Result<(), i64> {
        if self.capacity == 0 {
            return Ok(());
        }
        {
            let mut queues = crate::poison::lock_or_exit(self.queues.lock());
            let q = queues.entry(system_id).or_default();
            let now = crate::client::filetime_now();
            q.keys.retain(|_, issued| now - *issued < JUMP_KEY_LIFE * FILETIME_SECOND);
            if q.keys.remove(&char_id).is_some() {
                q.waiting.retain(|c| *c != char_id);
                return Ok(());
            }
            if q.waiting.is_empty() && population + q.keys.len() < self.capacity {
                return Ok(());
            }
            if !q.waiting.contains(&char_id) {
                q.waiting.push(char_id);
            }
        }
        self.pump_queue(system_id, population);
        let queues = crate::poison::lock_or_exit(self.queues.lock());
        let Some(q) = queues.get(&system_id) else { return Err(1) };
        if q.keys.contains_key(&char_id) {
            // Room opened while queueing: the key is theirs, the next attempt goes through.
            return Err(0);
        }
        let space = self.capacity.saturating_sub(population + q.keys.len()) as i64;
        let idx = q.waiting.iter().position(|c| *c == char_id).unwrap_or(0) as i64;
        Err((idx + 1 - space).max(1))
    }

    /// Issue entry keys for free room in `system_id` and tell everyone queued for it.
    pub fn pump_queue(&self, system_id: i64, population: usize) {
        if self.capacity == 0 {
            return;
        }
        let (update, recipients) = {
            let mut queues = crate::poison::lock_or_exit(self.queues.lock());
            let Some(q) = queues.get_mut(&system_id) else { return };
            let now = crate::client::filetime_now();
            q.keys.retain(|_, issued| now - *issued < JUMP_KEY_LIFE * FILETIME_SECOND);
            while !q.waiting.is_empty() && population + q.keys.len() < self.capacity {
                let next = q.waiting.remove(0);
                q.keys.insert(next, now);
            }
            let space = self.capacity.saturating_sub(population + q.keys.len()) as i64;
            // OnJumpQueueUpdate(solarsystemID, space, queue, jumpKeys, keyLife)
            let update = vec![
                Value::Int(system_id),
                Value::Int(space),
                Value::list(q.waiting.iter().map(|c| Value::Int(*c))),
                Value::Dict(q.keys.iter().map(|(c, t)| (Value::Int(*c), Value::Int(*t))).collect()),
                Value::Int(JUMP_KEY_LIFE),
            ];
            let recipients: Vec<i64> = q.waiting.iter().chain(q.keys.keys()).copied().collect();
            (update, recipients)
        };
        for char_id in recipients {
            self.notify_pilot(char_id, "OnJumpQueueUpdate", update.clone());
        }
    }
}

/// Actor facade. Actor closures must never await, access the DB or call World.
pub struct World {
    db: Db,
    sde: Arc<Sde>,
    progress_registry: Arc<crate::progress::Registry>,
    systems: Arc<Mutex<HashMap<i64, SystemHandle>>>,
    /// Quiescent, explicitly marked NPC cohorts of cleanly stopped actors,
    /// restored by the same system's next cold load; same-process only.
    sleeping_npcs: Mutex<HashMap<i64, system::npc_sleep::SleepingNpcs>>,
    observers: Mutex<HashMap<i64, i64>>,
    pilots: Arc<Pilots>,
    bindings: Arc<containment::Pilots>,
    weak: std::sync::Weak<World>,
    notice_hub: crate::gateway::notices::NoticeHub,
    sovereignty_ready: Arc<std::sync::atomic::AtomicBool>,
    /// Derived development-index capability, installed at startup and on
    /// publication; a cache only, the database stays authoritative.
    indices: Mutex<crate::services::sovereignty::indices::model::IndexCapability>,
    /// Shared NPC bounty batch coordination; live amounts stay with each
    /// system's actor, this only carries identity and payout ownership.
    pub(crate) bounties: Arc<bounties::Coordinator>,
}
impl World {
    pub fn new(db: Db, sde: Arc<Sde>, capacity: usize) -> Arc<Self> {
        let progress_registry = db.progress_registry.clone();
        Arc::new_cyclic(|weak| Self { db, sde, progress_registry, systems: Default::default(),
            observers: Default::default(), pilots: Arc::new(Pilots { capacity, ..Pilots::default() }), bindings: Default::default(),
            sleeping_npcs: Default::default(),
            weak: weak.clone(), notice_hub: crate::gateway::notices::NoticeHub::new(), sovereignty_ready: Default::default(), bounties: Arc::new(bounties::Coordinator::new()),
            indices: Mutex::new(crate::services::sovereignty::indices::model::IndexCapability::Unavailable(
                crate::services::sovereignty::indices::model::IndexUnavailable::HostUnavailable)) })
    }
    pub(crate) fn notice_hub(&self) -> crate::gateway::notices::NoticeHub { self.notice_hub.clone() }
    pub(crate) fn progress_registry(&self) -> &crate::progress::Registry { &self.progress_registry }
    pub(crate) fn shared(&self) -> Option<Arc<Self>> { self.weak.upgrade() }
    pub fn sde(&self) -> &Arc<Sde> { &self.sde }
    pub(crate) fn db(&self) -> &Db { &self.db }
    pub fn register_pilot(&self, char_id: i64, tx: NotifyTx) { self.pilots.register_pilot(char_id, tx); }
    pub(crate) fn unregister_pilot_generation(&self, char_id: i64, session_id: Option<i64>) -> bool {
        self.pilots.unregister_pilot_generation(char_id, session_id)
    }
    pub(crate) fn set_online_session(&self, char_id: i64, session: Option<infinity_core::session::OnlineSession>) {
        self.pilots.set_online_session(char_id, session);
    }
    pub(crate) fn online_session(&self, char_id: i64) -> Option<infinity_core::session::OnlineSession> { self.pilots.online_session(char_id) }
    pub(crate) fn replace_online_session(&self, character: i64, expected: Option<&infinity_core::session::OnlineSession>, next: Option<infinity_core::session::OnlineSession>) -> bool { self.pilots.replace_online_session(character, expected, next) }
    pub(crate) fn station_guests(&self, station_id: i64) -> Vec<infinity_core::session::OnlineSession> { self.pilots.station_guests(station_id) }
    pub(crate) fn online_sessions(&self) -> Vec<infinity_core::session::OnlineSession> { self.pilots.online_sessions() }
    pub fn is_online(&self, char_id: i64) -> bool { self.pilots.is_online(char_id) }
    pub fn online_pilots(&self) -> Vec<i64> { self.pilots.online_pilots() }
    pub fn notify_pilot(&self, char_id: i64, name: &str, args: Vec<Value>) { self.pilots.notify_pilot(char_id, name, args); }
    fn running(&self, id: i64) -> Option<SystemHandle> {
        // A closed survivor is either an explicit stop in flight or a failed
        // tombstone; only its supervisor removes it, never a lazy sweep here.
        crate::poison::lock_or_exit(self.systems.lock()).get(&id).filter(|handle| !handle.is_closed()).cloned()
    }
    /// Refuse new loaded use in every running system, then wait until admitted
    /// finite work has settled; callers flush afterwards inside their own
    /// timeout. Business transactions holding the gate complete on their own.
    pub async fn quiesce_loaded(&self) {
        let handles: Vec<_> = crate::poison::lock_or_exit(self.systems.lock()).values().filter(|h| !h.is_closed()).cloned().collect();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut armed = 0usize;
        for handle in &handles {
            let tx = tx.clone();
            if handle.send(actor::Job::new(move |s| { s.begin_loaded_quiesce(); s.drain_loaded(tx); })).is_ok() {
                armed += 1;
            }
        }
        drop(tx);
        while armed > 0 && rx.recv().await.is_some() { armed -= 1; }
    }
    /// Flush persistent positions in running systems without loading any systems.
    pub async fn flush_positions(&self) -> Result<u64> {
        let handles: Vec<_> = crate::poison::lock_or_exit(self.systems.lock()).values().filter(|h| !h.is_closed()).cloned().collect();
        let mut flushes = tokio::task::JoinSet::new();
        for handle in handles {
            flushes.spawn(snapshot::flush(handle, self.db.clone()));
        }
        let mut written = 0;
        let mut errors = Vec::new();
        while let Some(result) = flushes.join_next().await {
            match result {
                Ok(Ok(count)) => written += count,
                Ok(Err(error)) => errors.push(format!("{error:#}")),
                Err(error) => errors.push(format!("position flush task failed: {error}")),
            }
        }
        if errors.is_empty() { Ok(written) }
        else { Err(anyhow::anyhow!("position flush failed after writing {written} positions: {}", errors.join("; "))) }
    }
    /// No implicit loading. A missing/dead actor returns None.
    pub async fn with<R: Send + 'static>(&self, id: i64, f: impl FnOnce(&mut System) -> R + Send + 'static) -> Option<R> {
        self.running(id)?.with(f).await
    }
    /// No implicit loading; no-op if the system is not running.
    pub fn post(&self, id: i64, f: impl FnOnce(&mut System) + Send + 'static) {
        self.post_named(id, std::any::type_name_of_val(&f), f);
    }
    /// No implicit loading; no-op if the system is not running. The delay starts
    /// when the actor processes this message; the closure runs on a due tick.
    pub fn schedule(&self, system: i64, delay_ms: u64, f: impl FnOnce(&mut System) + Send + 'static) {
        let name = std::any::type_name_of_val(&f);
        self.post_named(system, name, move |s| s.schedule_named(delay_ms, name, f));
    }
    /// Wait for a timer tick without loading a missing system.
    pub async fn after(&self, system: i64, delay_ms: u64) -> bool {
        actor::after(self.running(system), delay_ms).await
    }
    /// Prepare the ship's SKIN projection once for a raw admission, while the
    /// caller holds the market guard that serializes cosmetics producers.
    /// Non-ship balls never query. PreparedShip admissions carry their own
    /// projection and do not pass through this path.
    async fn prepare_ship_skin(&self, ball: &Ball) -> Result<Option<ship_cosmetics::PreparedSkin>> {
        if self.sde.types.get(ball.type_id).and_then(|t| t.category_id) != Some(CATEGORY_SHIP) { return Ok(None); }
        let fact = ship_cosmetics::Ship { ship_id: ball.id, type_id: ball.type_id, character_id: ball.char_id, is_npc: false };
        crate::services::ship_cosmetics_mgr::state::prepare(&self.db, &self.sde, &[fact], crate::client::filetime_now()).await
            .map(|mut prepared| prepared.pop())
            .map_err(|error| match error {
                crate::services::CallError::Internal(error) => error,
                crate::services::CallError::User { msg, .. } => anyhow::anyhow!(msg),
            })
    }
    async fn deliver_ship(&self, system: i64, ball: Ball, prepared: Option<ship_cosmetics::PreparedSkin>) -> Result<()> {
        let ship = ball.id;
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.send_to_loaded(system, actor::Job::new(move |s| {
            s.add_ship(ball);
            if let (Some(token), Some(prepared)) = (s.capture_ball(ship), prepared) { s.install_skin(token, prepared); }
            let _ = tx.send(());
        })).await?;
        rx.await.map_err(|_| anyhow::anyhow!("system actor stopped"))
    }
    pub async fn add_ship(&self, system: i64, ball: Ball) -> Result<()> {
        // Only a real ship pays for the cosmetics cold sequence; everything
        // else keeps the original load path with no market guard at all.
        if self.sde.types.get(ball.type_id).and_then(|t| t.category_id) == Some(CATEGORY_SHIP) {
            let admission = self.admission(system).map_err(|error| anyhow::anyhow!(error))?;
            let market = crate::services::market::lock().await;
            admission.check()?;
            self.system_locked(system, &market).await?;
            let prepared = self.prepare_ship_skin(&ball).await?;
            return self.deliver_ship(system, ball, prepared).await;
        }
        self.system(system).await?;
        self.deliver_ship(system, ball, None).await
    }

    /// Explicit NPC admission: installs the canonical no_skin projection in
    /// the same actor job as the Add, with no NPC-specific or personal SKIN
    /// preparation. Reuses the existing system loading and delivery path
    /// unchanged.
    pub(crate) async fn add_npc_ship(&self, system: i64, ball: Ball) -> Result<()> {
        let prepared = ship_cosmetics::PreparedSkin::npc(&ball);
        self.deliver_ship(system, ball, Some(prepared)).await
    }
    pub(crate) async fn add_ship_locked(&self, system: i64, ball: Ball,
        market: &crate::services::market::economy_lock::Guard) -> Result<()> {
        self.system_locked(system, market).await?;
        let prepared = self.prepare_ship_skin(&ball).await?;
        let ship = ball.id;
        self.with(system, move |s| {
            s.add_ship(ball);
            if let (Some(token), Some(prepared)) = (s.capture_ball(ship), prepared) { s.install_skin(token, prepared); }
        }).await.ok_or_else(|| anyhow::anyhow!("system actor stopped"))
    }
    pub async fn remove_ball(&self, system: i64, id: i64) -> Option<Ball> {
        self.with(system, move |s| s.remove_ball(id)).await.flatten()
    }
    pub async fn destroy_ball(&self, system: i64, id: i64) -> Option<Ball> {
        self.with(system, move |s| s.destroy_ball(id)).await.flatten()
    }
    pub async fn request_entry(&self, system: i64, pilot: i64) -> Result<(), i64> {
        self.with(system, move |s| s.request_entry(pilot)).await.unwrap_or(Err(1))
    }
    /// Scene-bound updates for every observer whose delivered scene covers
    /// `about`; a factory that encodes nothing when its scene is empty.
    pub(crate) fn publish_destiny(&self, system: i64, about: i64, factory: impl FnOnce(i64) -> Vec<Value> + Send + 'static) {
        let name = std::any::type_name_of_val(&factory);
        self.post_named(system, name, move |s| s.publish_destiny(about, factory));
    }
    /// Bind one client's ballpark channel to a system and ego. The same client
    /// binds serially: the previous system's `unobserve` is awaited before the
    /// new job is sent, and a failed bind cleans the map inside this future
    /// before it returns, so a later bind cannot interleave. The reply rides
    /// the actor job: `Ok` only after the actor's pending initial actually
    /// delivered its SetState; a refused channel, a dropped sender or a dead
    /// actor all fail the future. `send_to_loaded` resends only an unsent job.
    pub async fn observe(&self, system: i64, client: i64, ego: i64, tx: DestinyTx) -> Result<()> {
        self.system(system).await?;
        let previous = crate::poison::lock_or_exit(self.observers.lock()).insert(client, system);
        if let Some(previous) = previous.filter(|&p| p != system) {
            self.with(previous, move |s| s.unobserve(client)).await;
        }
        let (reply, rx) = tokio::sync::oneshot::channel();
        let sent = self.send_to_loaded(system, actor::Job::new(move |s| { let _ = s.observe(client, ego, tx, reply); })).await;
        if sent.is_ok() && rx.await == Ok(true) { return Ok(()); }
        let mut observers = crate::poison::lock_or_exit(self.observers.lock());
        if observers.get(&client) == Some(&system) { observers.remove(&client); }
        Err(anyhow::anyhow!("system actor stopped or observer channel closed"))
    }
    /// Drop one client's binding map entry and post its unobserve; only the
    /// map serializes same-client binds, so a posted teardown of the previous
    /// system can never remove the newer entry's registration.
    pub fn unobserve(&self, client: i64) {
        let system = crate::poison::lock_or_exit(self.observers.lock()).remove(&client);
        if let Some(system) = system { self.post(system, move |s| s.unobserve(client)); }
    }
}

#[cfg(all(test, feature = "tests-space"))] mod tests;
