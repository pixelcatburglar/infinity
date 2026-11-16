//! Scene presentation for ballpark observers: initial state, cloak and uncloak
//! reveals, real departure sequencing and the replayable set of sustained
//! module effects, all consumed by `eve/client/script/remote/michelle.py` and
//! fxSequencer.py. Sustained ("Current") effects are replayed with their
//! original duration, remaining repeat and cycle phase; transient
//! fire-and-forget effects are never recorded. Presentation state is
//! actor-owned memory only: there is no per-observer ball copy and no replay
//! of historical shots or explosions; scene membership lives in `membership`.

use super::*;
use super::observers::{BallToken, DestinyAudience, ObserverToken};
use crate::space::{special_fx, FxModule};
use std::collections::BTreeMap;
use std::sync::atomic::AtomicBool;

/// Whether a published module effect outlives its firing cycle for scene replay.
pub(crate) enum FxRetention {
    Transient,
    Current,
}

/// The wire parameters of one module effect activation, kept for replay.
#[derive(Clone)]
pub(crate) struct ModuleFx {
    pub module: FxModule,
    pub guid: String,
    pub charge: Option<i32>,
    pub offensive: bool,
    pub duration_ms: f64,
    pub repeat: i64,
    pub graphic_info: Value,
}

/// One admitted activation: the bank master module and its shared stop flag
/// identify it across deferred worker phases and distinguish it from any
/// successor activation on the same module.
struct FxActivation {
    generation: Arc<AtomicBool>,
    source: BallToken,
    target: Option<BallToken>,
}

/// A sustained effect the actor accepted, with its checked replay schedule.
/// `start_ms` is the actor millisecond at which the real start was accepted and
/// `end_ms` is the checked `start + duration * repeats` deadline. The record
/// stays bound to the activation's own stop flag, so a superseded activation's
/// retirement cleans up only its own records.
struct CurrentFx {
    master: i64,
    generation: Arc<AtomicBool>,
    version: u64,
    source: BallToken,
    target: Option<BallToken>,
    fx: ModuleFx,
    start_ms: i64,
    end_ms: i64,
}

/// Actor-owned presentation bookkeeping: admitted module activations and the
/// sustained effects they currently show.
#[derive(Default)]
pub(in crate::space) struct Presentation {
    activations: HashMap<i64, FxActivation>,
    current: BTreeMap<(i64, i64), CurrentFx>,
    endpoints: HashMap<BallToken, HashSet<(i64, i64)>>,
    candidates: HashSet<(i64, i64)>,
    version: u64,
}

/// Borrowed coverage for the effect projector: one bubble roster plus the
/// always-present layer and the binding's own ego token, borrowed from the
/// membership sets so `Presentation` can evaluate without owning them.
pub(super) struct SceneView<'a> {
    pub(super) local: Option<&'a HashSet<BallToken>>,
    pub(super) global: &'a HashSet<BallToken>,
    pub(super) ego: Option<BallToken>,
}
impl SceneView<'_> {
    pub(super) fn contains(&self, token: BallToken) -> bool {
        self.ego == Some(token) || self.global.contains(&token)
            || self.local.is_some_and(|set| set.contains(&token))
    }
}

impl Presentation {
    fn remove_current(&mut self, key: &(i64, i64)) -> Option<CurrentFx> {
        let record = self.current.remove(key)?;
        for token in [Some(record.source), record.target].into_iter().flatten() {
            if let Some(keys) = self.endpoints.get_mut(&token) {
                keys.remove(key);
                if keys.is_empty() { self.endpoints.remove(&token); }
            }
        }
        Some(record)
    }
    /// Any admitted activation or sustained-effect record still retained:
    /// residual presentation work is a sleep blocker. Read-only.
    pub(in crate::space) fn has_activity(&self) -> bool {
        !(self.activations.is_empty() && self.current.is_empty() && self.endpoints.is_empty())
    }
    fn insert_current(&mut self, key: (i64, i64), record: CurrentFx) {
        let _ = self.remove_current(&key);
        for token in [Some(record.source), record.target].into_iter().flatten() {
            self.endpoints.entry(token).or_default().insert(key);
        }
        self.current.insert(key, record);
    }
    /// The authoritative stop event for one recorded sustained effect.
    fn fx_stop_event(record: &CurrentFx, stamp: i64) -> Value {
        special_fx(stamp, record.source.id(), Some(record.fx.module), record.target.map(|t| t.id()),
            record.fx.charge.map(i64::from), &record.fx.guid, record.fx.offensive, 0, 0,
            Value::Float(record.fx.duration_ms), record.fx.repeat, None, 0, record.fx.graphic_info.clone())
    }
    /// Replay arguments for a live record at `now`: the remaining
    /// repeat, the original start FILETIME and the current cycle phase in
    /// 100 ns ticks. `None` once the checked deadline has passed or a value
    /// cannot be represented.
    fn fx_replay_args(record: &CurrentFx, now: i64) -> Option<(i64, i64, i64)> {
        if now >= record.end_ms { return None; }
        let repeats = ((record.end_ms - now) as f64 / record.fx.duration_ms).ceil();
        if !repeats.is_finite() || repeats < 1.0 || repeats >= i64::MAX as f64 { return None; }
        let phase_ms = (now - record.start_ms) as f64 % record.fx.duration_ms;
        if !phase_ms.is_finite() || phase_ms < 0.0 { return None; }
        let phase = phase_ms / MILLISECONDS_PER_SECOND as f64 * FILETIME_SECOND as f64;
        if !phase.is_finite() || phase < 0.0 || phase >= i64::MAX as f64 { return None; }
        let start_time = record.start_ms.checked_mul(FILETIME_SECOND / MILLISECONDS_PER_SECOND)
            .and_then(|ticks| ticks.checked_add(infinity_core::consts::FILETIME_UNIX_EPOCH))?;
        Some((repeats as i64 - 1, start_time, phase as i64))
    }
    /// One replayed start event for a live record at `now`.
    fn fx_replay_event(record: &CurrentFx, now: i64, stamp: i64) -> Option<Value> {
        let (repeat, start_time, phase) = Self::fx_replay_args(record, now)?;
        Some(special_fx(stamp, record.source.id(), Some(record.fx.module), record.target.map(|t| t.id()),
            record.fx.charge.map(i64::from), &record.fx.guid, record.fx.offensive, 1, 1,
            Value::Float(record.fx.duration_ms), repeat, Some(start_time), phase, record.fx.graphic_info.clone()))
    }
    /// The record's own generation still owns its master's activation slot.
    fn fx_live(&self, record: &CurrentFx) -> bool {
        self.activations.get(&record.master)
            .is_some_and(|activation| Arc::ptr_eq(&activation.generation, &record.generation))
    }
    /// The effect stop/replay pairs for one coverage change. `changed` is
    /// only this cohort's delta, so candidates come from the endpoint index,
    /// not a scan; both coverage sides arrive as borrowed views. Records the
    /// observer keeps are silent: a stop fires when both endpoints were
    /// covered and either is no longer covered; a replay fires when they were
    /// not both covered and both become covered on a live record.
    pub(super) fn scene_fx_changes(&mut self, old: SceneView<'_>, new: SceneView<'_>,
        changed: &[BallToken], stamp: i64, now: i64,
        current: impl Fn(BallToken) -> bool) -> (Vec<Value>, Vec<Value>) {
        let mut candidates = std::mem::take(&mut self.candidates);
        for token in changed {
            if let Some(keys) = self.endpoints.get(token) {
                candidates.extend(keys.iter().copied());
            }
        }
        let mut stops = Vec::new();
        let mut starts = Vec::new();
        for key in candidates.drain() {
            let Some(record) = self.current.get(&key) else { continue };
            let covered = |view: &SceneView<'_>| view.contains(record.source)
                && record.target.is_none_or(|target| view.contains(target));
            let (before, after) = (covered(&old), covered(&new));
            if before && !after {
                stops.push(Self::fx_stop_event(record, stamp));
            } else if !before && after && self.fx_live(record)
                && current(record.source) && record.target.is_none_or(|t| current(t)) {
                if let Some(event) = Self::fx_replay_event(record, now, stamp) { starts.push(event); }
            }
        }
        self.candidates = candidates;
        (stops, starts)
    }
}

/// The client-facing half of a real ball removal, captured while the ball was
/// still in the world and delivered by the queued exit action.
pub(in crate::space) struct SceneExit {
    /// The departing ball's own token; a same-id successor never inherits its recipients.
    token: BallToken,
    cloak: u8,
    /// The bubble roster it was last delivered through, if any.
    bubble: Option<interest::BubbleId>,
    /// Whether it was last delivered through the always-present layer.
    global: bool,
    stops: Vec<CurrentFx>,
    fleet: crate::services::fleets::space_state::FleetExit,
}

impl System {
    /// Both record endpoints inside the observer's delivered scene.
    fn fx_scene_has(&self, observer: ObserverToken, record: &CurrentFx) -> bool {
        self.scene_has(observer, record.source)
            && record.target.is_none_or(|target| self.scene_has(observer, target))
    }

    fn publish_fx_stop(&mut self, record: CurrentFx) {
        let stamp = self.current_time;
        self.fx_emit(record.source.id(), record.target, move || vec![Presentation::fx_stop_event(&record, stamp)]);
    }

    /// Both record endpoints still alive at their captured incarnation.
    fn fx_endpoints_current(&self, record: &CurrentFx) -> bool {
        self.ball_token_current(record.source)
            && record.target.is_none_or(|target| self.ball_token_current(target))
    }


    /// Emit events to every informed observer whose delivered scene covers
    /// the source and, when present, the target endpoint of the effect. The
    /// factory runs only when a recipient exists, like the original path.
    fn fx_emit(&mut self, source: i64, target: Option<BallToken>, events: impl FnOnce() -> Vec<Value>) {
        let cloak = self.ball(source).map_or(0, |ball| ball.cloak);
        let mut recipients = Vec::new();
        for (token, _) in self.scene_observers(&DestinyAudience::Visible { id: source, cloak }) {
            if target.is_some_and(|t| !self.scene_has(token, t)) { continue; }
            recipients.push(token);
        }
        if recipients.is_empty() { self.prune_observers(); return; }
        let mut events = events();
        let count = recipients.len();
        for (i, token) in recipients.into_iter().enumerate() {
            let batch = if i + 1 == count { std::mem::take(&mut events) } else { events.clone() };
            self.emit_destiny(DestinyAudience::Observer(token), batch);
        }
    }


    /// SetState replays the bare OnSpecialFX argument tuple that `special_fx`
    /// wraps as `(stamp,(name,args))`.
    fn fx_args_tuple(event: &Value) -> Value {
        event.items().and_then(|fields| fields.get(1)).and_then(Value::items)
            .and_then(|call| call.get(1)).cloned().unwrap_or(Value::None)
    }


    /// Register one activation's replay identity: called once at the first
    /// legal cycle start, never per repeat. Returns false when the source ball
    /// no longer exists to anchor the replay, or a required target is missing.
    pub(crate) fn begin_fx_activation(&mut self, master: i64, source: i64, target: Option<i64>, generation: Arc<AtomicBool>) -> bool {
        let Some(source) = self.capture_ball(source) else { return false };
        let target = match target {
            Some(target) => match self.capture_ball(target) {
                Some(token) => Some(token),
                None => return false,
            },
            None => None,
        };
        self.presentation.activations.insert(master, FxActivation { generation, source, target });
        true
    }
    /// Publish one module effect through the actor. A Transient effect keeps the
    /// original fire path with no record; a Current start records its checked
    /// replay schedule and schedules the one shared expiry for this real start.
    pub(crate) fn publish_module_fx(&mut self, master: i64, generation: &Arc<AtomicBool>, fx: ModuleFx, start: bool, retention: FxRetention) -> bool {
        let Some(activation) = self.presentation.activations.get(&master)
            .filter(|activation| Arc::ptr_eq(&activation.generation, generation)) else { return false };
        let (source, target, activation_generation) = (activation.source, activation.target, activation.generation.clone());
        let stamp = self.current_time;
        if let FxRetention::Transient = retention {
            self.fx_emit(source.id(), target, move || vec![special_fx(
                stamp, source.id(), Some(fx.module), target.map(|t| t.id()), fx.charge.map(i64::from),
                &fx.guid, fx.offensive, i64::from(start), i64::from(start), Value::Float(fx.duration_ms),
                fx.repeat, None, 0, fx.graphic_info)]);
            return true;
        }
        if !self.ball_token_current(source) || target.is_some_and(|t| !self.ball_token_current(t)) { return false; }
        let Some(item) = fx.module.item else {
            tracing::error!(master, "sustained module effect rejected: module has no fitted item");
            return false;
        };
        let key = (master, item);
        if !start {
            if !self.presentation.current.get(&key)
                .is_some_and(|record| Arc::ptr_eq(&record.generation, generation)) { return false; }
            let record = self.presentation.remove_current(&key).expect("record present");
            self.publish_fx_stop(record);
            return true;
        }
        if !fx.duration_ms.is_finite() || fx.duration_ms <= 0.0 {
            tracing::error!(master, "sustained module effect rejected: duration not finite and positive");
            return false;
        }
        let Some(span_ms) = (if fx.repeat <= 0 { Some(1i64) } else { fx.repeat.checked_add(1) })
            .and_then(|repeats| {
                let span = (fx.duration_ms * repeats as f64).ceil();
                (span.is_finite() && span > 0.0 && span < i64::MAX as f64).then_some(span as i64)
            }) else {
                tracing::error!(master, "sustained module effect rejected: unrepresentable repeat schedule");
                return false;
            };
        let Some(end_ms) = self.ms.checked_add(span_ms) else {
            tracing::error!(master, "sustained module effect rejected: unrepresentable deadline");
            return false;
        };
        self.presentation.version = self.presentation.version.checked_add(1).expect("module effect version exhausted");
        let version = self.presentation.version;
        let start_ms = self.ms;
        self.fx_emit(source.id(), target, {
            let fx = fx.clone();
            move || vec![special_fx(stamp, source.id(), Some(fx.module), target.map(|t| t.id()),
                fx.charge.map(i64::from), &fx.guid, fx.offensive, 1, 1, Value::Float(fx.duration_ms),
                fx.repeat, None, 0, fx.graphic_info)]
        });
        self.presentation.insert_current(key, CurrentFx {
            master, generation: activation_generation, version, source, target, fx, start_ms, end_ms,
        });
        self.schedule_at(end_ms, move |s| s.expire_current_fx(key, version));
        true
    }

    /// Authoritative retirement of one activation: the activation slot drops
    /// only while it still holds this generation; the generation's own records
    /// get their one stop each regardless, and no successor is touched.
    pub(crate) fn retire_fx_activation(&mut self, master: i64, generation: &Arc<AtomicBool>) {
        if self.presentation.activations.get(&master).is_some_and(|a| Arc::ptr_eq(&a.generation, generation)) {
            self.presentation.activations.remove(&master);
        }
        let keys: Vec<_> = self.presentation.current.range((master, i64::MIN)..=(master, i64::MAX))
            .filter(|(_, record)| Arc::ptr_eq(&record.generation, generation))
            .map(|(&key, _)| key).collect();
        for key in keys {
            if let Some(record) = self.presentation.remove_current(&key) { self.publish_fx_stop(record); }
        }
    }

    /// The one expiry per real start: a version match stops and removes the
    /// record; a stale timer from a superseded start passes without touching
    /// the successor.
    fn expire_current_fx(&mut self, key: (i64, i64), version: u64) {
        if !self.presentation.current.get(&key).is_some_and(|record| record.version == version) { return; }
        let Some(record) = self.presentation.remove_current(&key) else { return };
        self.publish_fx_stop(record);
    }

    /// Remove every sustained-effect record a departing ball sourced or
    /// targeted; the exit action emits their stops.
    fn drain_fx_for_departure(&mut self, id: i64) -> Vec<CurrentFx> {
        let Some(token) = self.ball_token(id) else { return Vec::new(); };
        let keys: Vec<_> = self.presentation.endpoints.get(&token)
            .map(|found| found.iter().copied().collect()).unwrap_or_default();
        keys.into_iter().filter_map(|key| self.presentation.remove_current(&key)).collect()
    }

    /// Drop a departing ball's remaining presentation bookkeeping: activations
    /// it owned and its actor SKIN projection. Wire-visible records are drained
    /// by the caller into the exit, never silently forgotten.
    pub(super) fn forget_presentation(&mut self, ball: BallToken) {
        let id = ball.id();
        self.presentation.activations.retain(|_, activation| activation.source.id() != id);
        self.skins.remove(id);
    }


    /// Capture one departing ball's exit while it is still in the world: the
    /// old fleet tickets, the sustained effects to stop and its last visibility.
    pub(super) fn capture_scene_exit(&mut self, id: i64, fleet: crate::services::fleets::space_state::FleetExit) -> SceneExit {
        let token = self.capture_ball(id).expect("scene-exit ball exists");
        let cloak = self.balls[&id].cloak;
        let (bubble, global) = self.scene_home(token);
        let stops = self.drain_fx_for_departure(id);
        self.forget_presentation(token);
        SceneExit { token, cloak, bubble, global, stops, fleet }
    }

    /// First delivery of a new epoch: the native SetState built from the
    /// observer's own scene (its bubble roster, the always-present layer and
    /// its ego) with the current sustained effects, then the pilot's fleet
    /// snapshot and SKIN presentation, all on the observer's ordered channel.
    /// The pending completion resolves only once the state actually sent.
    pub(super) fn present_initial(&mut self, observer: ObserverToken, collisions: bool) -> bool {
        let Some(ego) = self.ticket_ego(observer) else { return false };
        if self.observers.get(&observer.client).is_none_or(|entry|
            entry.epoch != observer.epoch || entry.informed.is_some() || entry.completion.is_none()) { return false; }
        let tick = self.current_time;
        let bubble = self.bubble_of(ego);
        let ego_token = self.ball_token(ego);
        if ego_token.is_some_and(|token| !self.scene_ready(token)) { return false; }
        if bubble.and_then(|b| self.membership.committed.get(&b)).into_iter()
            .flatten().chain(self.membership.global_committed.iter())
            .any(|&token| !self.ball_token_current(token)) { return false; }
        let mut ids: Vec<i64> = Vec::new();
        if let Some(set) = bubble.and_then(|b| self.membership.committed.get(&b)) {
            ids.extend(set.iter().map(|token| token.id()));
        }
        ids.extend(self.membership.global_committed.iter().map(|token| token.id()));
        ids.push(ego);
        ids.sort_unstable();
        ids.dedup();
        let mut state = vec![DESTINY_FULLSTATE];
        state.extend_from_slice(&(tick as i32).to_le_bytes());
        let mut slims = Vec::new();
        let mut damage = Vec::new();
        let mut scene_ids: HashSet<i64> = HashSet::new();
        for id in ids {
            let Some(b) = self.balls.get(&id) else { continue };
            if b.cloak != 0 && b.id != ego { continue; }
            scene_ids.insert(id);
            b.write(&mut state, collisions);
            slims.push(b.slim_item());
            if b.free { damage.push((Value::Int(b.id), b.damage_state())); }
        }
        let now = self.ms;
        let mut effect_states = Vec::new();
        let mut selected = std::mem::take(&mut self.presentation.candidates);
        for id in &scene_ids {
            let Some(token) = self.ball_token(*id) else { continue };
            let Some(keys) = self.presentation.endpoints.get(&token) else { continue };
            selected.extend(keys.iter().copied());
        }
        let mut keys: Vec<(i64, i64)> = selected.drain().collect();
        keys.sort_unstable();
        for key in keys {
            let Some(record) = self.presentation.current.get(&key) else { continue };
            if !self.presentation.fx_live(record) || !self.fx_endpoints_current(record) { continue; }
            if !scene_ids.contains(&record.source.id())
                || record.target.is_some_and(|target| !scene_ids.contains(&target.id())) { continue; }
            if let Some(event) = Presentation::fx_replay_event(record, now, tick) {
                effect_states.push(Self::fx_args_tuple(&event));
            }
        }
        self.presentation.candidates = selected;
        let sol = Value::instance("eve.common.script.util.slimItem.SlimItem", [
            ("itemID", Value::Int(self.id)), ("typeID", Value::Int(infinity_core::consts::TYPE_SOLAR_SYSTEM.into())), ("ownerID", Value::Int(1)),
        ]);
        let bag = Value::instance("utillib.KeyVal", [
            ("state", Value::str(state)), ("ego", Value::Int(ego)), ("slims", Value::List(slims)),
            ("damageState", Value::Dict(damage)), ("effectStates", Value::List(effect_states)), ("allianceBridges", Value::list([])),
            ("droneState", Value::list(self.drone_state_rows(ego))), ("solItem", sol), ("industryLevel", Value::Int(0)),
            ("researchLevel", Value::Int(0)), ("dbuffState", Value::list([])),
        ]);
        if !self.emit_destiny(DestinyAudience::Observer(observer), vec![event(tick, "SetState", vec![bag])]) { return false; }
        // SetState must land as its own batch ahead of any later scene traffic.
        if !self.flush_observer_pending(observer) { return false; }
        self.scene_binding_informed(observer, bubble, ego_token);
        if let Some(character) = self.observer_character(observer) {
            let snapshot = crate::services::fleets::space_state::observer_snapshot(self, self.id, character, &[]);
            self.emit_scene_notify(observer, "OnFleetStateChange", vec![snapshot]);
        }
        self.present_skins(observer, None);
        self.finish_initial(observer)
    }

    /// A ball that just became visible: the owner keeps the native UncloakBall
    /// sequence on its exact ego token; every other informed holder of the
    /// revealed layer gets the Add and the reveal FX once, then the sustained
    /// effects now reaching each once, its fleet snapshot and the SKIN
    /// presentation of the revealed ball. A global ball reaches every other
    /// informed scene, not only its physical bubble. The direct add commits
    /// the token so the shared delta never re-adds it.
    pub(super) fn present_uncloak(&mut self, ball: BallToken, stamp: i64, collisions: bool) {
        let ship = ball.id();
        let Some(ball_ref) = self.ball(ship) else { return };
        let add = add_balls_event(stamp, ball_ref, collisions);
        let own = event(stamp, "UncloakBall", vec![Value::Int(ship)]);
        let fx = special_fx(stamp, ship, None, None, None, "effects.Uncloak", false, 1, 0,
            Value::Int(SPECIAL_FX_UNSPECIFIED_DURATION), 0, None, 0, Value::None);
        // Native UncloakBall restores massiveness (Ballpark.cpp:5257-5287).
        let reset = collision::reset_event(ship, stamp, collisions);
        let mut owner = vec![own];
        if let Some(reset) = reset { owner.push(reset); }
        owner.push(fx.clone());
        self.emit_destiny(DestinyAudience::ExactEgo(ball), owner);
        let global = self.membership.globals.contains(&ship);
        let bubble = self.bubble_of(ship);
        if global {
            self.emit_destiny(DestinyAudience::GlobalExceptEgo(ball), vec![add, fx]);
        } else if let Some(bubble) = bubble {
            self.emit_destiny(DestinyAudience::Scene { bubble, except_ego: Some(ship) }, vec![add, fx]);
        }
        self.commit_scene_visibility(ball, bubble, global, true);
        let now = self.ms;
        let holders = if global { self.scene_observers(&DestinyAudience::GlobalExceptEgo(ball)) }
            else if let Some(bubble) = bubble { self.scene_observers(&DestinyAudience::Scene { bubble, except_ego: Some(ship) }) }
            else { Vec::new() };
        let keys: Vec<(i64, i64)> = self.presentation.endpoints.get(&ball)
            .map(|found| found.iter().copied().collect()).unwrap_or_default();
        for (token, _) in holders {
            let events: Vec<Value> = keys.iter().filter_map(|key| {
                let record = self.presentation.current.get(key)?;
                if !self.presentation.fx_live(record) || !self.fx_endpoints_current(record) || !self.fx_scene_has(token, record) { return None; }
                Presentation::fx_replay_event(record, now, stamp)
            }).collect();
            if !events.is_empty() { self.emit_destiny(DestinyAudience::Observer(token), events); }
            if let Some(character) = self.observer_character(token) {
                let snapshot = crate::services::fleets::space_state::observer_snapshot(self, self.id, character, &[]);
                self.emit_scene_notify(token, "OnFleetStateChange", vec![snapshot]);
            }
            self.present_skins(token, Some(ball));
        }
    }


    /// Cloak loss: the owner receives CloakBall once and keeps the ball.
    /// Foreign holders of its last delivered scene also receive CloakBall;
    /// their client removes the ball, so the server sends no RemoveBalls.
    /// Only a foreign holder that actually loses both effect endpoints gets
    /// the stop and the local fleet clear. Records and the activation stay
    /// for the real departure.
    pub(super) fn present_cloak(&mut self, ball: BallToken, stamp: i64) {
        let ship = ball.id();
        let (home, global) = self.scene_home(ball);
        let foreign = if global { Some(DestinyAudience::GlobalExceptEgo(ball)) }
            else { home.map(|bubble| DestinyAudience::Scene { bubble, except_ego: Some(ship) }) };
        let holders = foreign.as_ref().map(|audience| self.scene_observers(audience)).unwrap_or_default();
        let keys: Vec<(i64, i64)> = self.presentation.endpoints.get(&ball)
            .map(|found| found.iter().copied().collect()).unwrap_or_default();
        for (token, _) in &holders {
            for key in &keys {
                let Some(record) = self.presentation.current.get(key) else { continue };
                let covered = self.scene_has(*token, record.source)
                    && record.target.is_none_or(|target| self.scene_has(*token, target));
                if !covered { continue; }
                let stop = Presentation::fx_stop_event(record, stamp);
                self.emit_destiny(DestinyAudience::Observer(*token), vec![stop]);
            }
        }
        let cloak = special_fx(stamp, ship, None, None, None, "effects.Cloak", false, 1, 0,
            Value::Int(SPECIAL_FX_UNSPECIFIED_DURATION), 0, None, 0, Value::None);
        self.emit_destiny(DestinyAudience::ExactEgo(ball), vec![cloak.clone(), event(stamp, "CloakBall", vec![Value::Int(ship)])]);
        if let Some(audience) = foreign { self.emit_destiny(audience, vec![cloak, event(stamp, "CloakBall", vec![Value::Int(ship)])]); }
        self.commit_scene_visibility(ball, home, global, false);
        for (token, _) in holders {
            if let Some(character) = self.observer_character(token) {
                let snapshot = crate::services::fleets::space_state::observer_snapshot(self, self.id, character, &[ship]);
                self.emit_scene_notify(token, "OnFleetStateChange", vec![snapshot]);
            }
        }
    }

    /// Queue the client half of a real removal at the captured epoch: sustained
    /// effects stop to the observers whose delivered scene covered both
    /// endpoints, then the caller's native events to the ball's last holders,
    /// then the fleet snapshot with clears for the saved old tickets. The
    /// emitted remove commits the token so the shared delta never repeats it;
    /// observers of a later epoch and stale tickets get nothing.
    pub(in crate::space) fn queue_scene_exit(&mut self, exit: SceneExit, factory: impl FnOnce(i64) -> Vec<Value> + Send + 'static) {
        let cutoff = self.observer_epoch;
        let SceneExit { token, cloak, bubble, global, stops, fleet } = exit;
        self.actions.push_back((self.current_time, Box::new(move |s, stamp| {
            for stop in &stops {
                let mut seen = HashSet::new();
                for endpoint in [Some(stop.source), stop.target].into_iter().flatten() {
                    let (home, endpoint_global) = s.scene_home(endpoint);
                    let audience = if endpoint_global { DestinyAudience::System }
                        else if let Some(bubble) = home { DestinyAudience::Scene { bubble, except_ego: None } }
                        else { DestinyAudience::ExactEgo(endpoint) };
                    for (observer, _) in s.scene_observers(&audience) {
                        if observer.epoch > cutoff || !seen.insert((observer.client, observer.epoch)) { continue; }
                        if !s.scene_has(observer, stop.source) { continue; }
                        if !stop.target.is_none_or(|t| s.scene_has(observer, t)) { continue; }
                        s.emit_before(DestinyAudience::Observer(observer), vec![Presentation::fx_stop_event(stop, stamp)], cutoff);
                    }
                }
            }
            let events = factory(stamp);
            if !events.is_empty() {
                s.emit_before(DestinyAudience::Departed { token, cloak, bubble, global }, events, cutoff);
            }
            s.scene_token_gone(token);
            crate::services::fleets::space_state::deliver_exit(s, fleet);
        })));
    }
}

#[cfg(all(test, feature = "tests-space"))] mod tests;
