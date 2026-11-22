//! Actor-owned timed runtime modifier inputs anchored to balls in this
//! system. Mirrors the client's `dbuff/common/baseBurstEffect.py` timed
//! source skyline: repeated pulses from one source retain their distinct
//! deadlines rather than replacing the source, and the live scalar falls back
//! to a weaker retained deadline once a stronger earlier one expires. An
//! entry is live while its deadline is not strictly before the actor's now;
//! one named timer per receiver fires at `deadline + 1` and is fenced by the
//! receiver's armed generation, the ball's `BallToken` incarnation serial and
//! the captured deadline, so stale closures return without mutating.
//! Collection aggregate modes come from the immutable `sde::dbuff` rows the
//! caller resolves; fit application lives in `space/combat/inputs.rs` and
//! fleet admission in the producer. The anchored enumeration feeds cold fit
//! loads, and the per-collection folded state feeds the client dbuff
//! tracker. The state is actor-local and does not survive restart.
use std::collections::{BTreeMap, HashMap};

use crate::sde::dbuff::DbuffAggregate;
use crate::services::CallError;
use crate::services::fit::runtime::{fold_runtime_values, RuntimeModifierSource, RuntimeReceiverKey, RuntimeSourceKey};
use super::system::observers::BallToken;
use super::System;

/// Stable actor timer name for the receiver expiry fence.
const RUNTIME_TIMER_NAME: &str = "space::dbuff::expire";

/// One timed scalar pulse landing on a receiver for a collection.
#[derive(Clone, Copy, Debug)]
pub(crate) struct TimedRuntimeInput {
    pub(crate) receiver: RuntimeReceiverKey,
    pub(crate) collection_id: i32,
    pub(crate) source: RuntimeSourceKey,
    pub(crate) value: f64,
    pub(crate) expires_at_ms: i64,
}

/// The observable result of one mutation of a receiver's source skyline.
/// `changed` is true when the current source scalar moved or the source was
/// removed; `next_expiry_ms` is the earliest retained deadline of that
/// `(receiver, collection, source)` after the mutation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct RuntimeStateChange {
    pub(crate) changed: bool,
    pub(crate) receiver: RuntimeReceiverKey,
    pub(crate) collection_id: i32,
    pub(crate) source: RuntimeSourceKey,
    pub(crate) next_expiry_ms: Option<i64>,
}

/// One source's retained pulses for one collection on one receiver: the
/// collection's SDE aggregate mode and a scalar per deadline.
struct SourceSkyline {
    aggregate: DbuffAggregate,
    pulses: HashMap<i64, f64>,
}

impl SourceSkyline {
    /// Strongest live scalar and its deadline. Entries at their exact
    /// deadline are still live; equal scalars keep the later deadline.
    fn winner(&self, now_ms: i64) -> Option<(f64, i64)> {
        self.pulses.iter()
            .filter(|(&deadline, _)| deadline >= now_ms)
            .max_by(|(deadline_a, value_a), (deadline_b, value_b)| match self.aggregate {
                DbuffAggregate::Maximum => value_a.total_cmp(value_b).then(deadline_a.cmp(deadline_b)),
                DbuffAggregate::Minimum => value_b.total_cmp(value_a).then(deadline_a.cmp(deadline_b)),
            })
            .map(|(&deadline, &value)| (value, deadline))
    }

    fn next_deadline(&self) -> Option<i64> {
        self.pulses.keys().min().copied()
    }
}

/// Whether a receiver key is anchored to the ball leaving the scene.
fn anchored_to(receiver: &RuntimeReceiverKey, ball: i64, char_id: Option<i64>) -> bool {
    match *receiver {
        RuntimeReceiverKey::Ship(ship) => ship == ball,
        RuntimeReceiverKey::Module { ship, .. } => ship == ball,
        RuntimeReceiverKey::Drone { ship, drone } => ship == ball || drone == ball,
        RuntimeReceiverKey::Character(character) => Some(character) == char_id,
    }
}

/// Bounded actor maps of timed source skylines plus the per-receiver armed
/// timer. Stores Copy scalars only: receiver and source keys, collection
/// aggregate modes, pulse scalars and deadlines.
#[derive(Default)]
pub(crate) struct RuntimeTimedState {
    receivers: HashMap<RuntimeReceiverKey, HashMap<(i32, RuntimeSourceKey), SourceSkyline>>,
    /// Per-receiver armed timer `(generation, deadline)`. Generations come
    /// from one monotonic counter so a removed and reapplied receiver never
    /// reuses an old one.
    armed: HashMap<RuntimeReceiverKey, (u64, i64)>,
    timer_sequence: u64,
}

impl RuntimeTimedState {
    /// Land one pulse on its `(receiver, collection, source)` skyline. An
    /// already-expired deadline never becomes live state; a repeated deadline
    /// keeps the stronger scalar for the skyline's aggregate.
    pub(crate) fn apply_timed(
        &mut self,
        input: TimedRuntimeInput,
        aggregate: DbuffAggregate,
        now_ms: i64,
    ) -> Result<RuntimeStateChange, CallError> {
        if !input.value.is_finite() {
            return Err(CallError::Internal(anyhow::anyhow!("runtime input value is not finite")));
        }
        let key = (input.collection_id, input.source);
        if input.expires_at_ms < now_ms {
            let next_expiry_ms = self.receivers.get(&input.receiver)
                .and_then(|sources| sources.get(&key))
                .and_then(SourceSkyline::next_deadline);
            return Ok(RuntimeStateChange {
                changed: false,
                receiver: input.receiver,
                collection_id: input.collection_id,
                source: input.source,
                next_expiry_ms,
            });
        }
        let skyline = self.receivers.entry(input.receiver).or_default()
            .entry(key).or_insert_with(|| SourceSkyline { aggregate, pulses: HashMap::new() });
        let before = skyline.winner(now_ms).map(|(value, _)| value);
        let mode = skyline.aggregate;
        skyline.pulses.entry(input.expires_at_ms)
            .and_modify(|value| *value = fold_runtime_values(mode, [*value, input.value]).unwrap_or(input.value))
            .or_insert(input.value);
        let after = skyline.winner(now_ms).map(|(value, _)| value);
        Ok(RuntimeStateChange {
            changed: before != after,
            receiver: input.receiver,
            collection_id: input.collection_id,
            source: input.source,
            next_expiry_ms: skyline.next_deadline(),
        })
    }

    /// Remove every retained deadline of the receiver strictly before
    /// `now_ms`, dropping empty source skylines, and report one change per
    /// affected source.
    pub(crate) fn expire_before(&mut self, receiver: RuntimeReceiverKey, now_ms: i64) -> Vec<RuntimeStateChange> {
        let Some(sources) = self.receivers.get_mut(&receiver) else { return Vec::new() };
        let mut changes = Vec::new();
        sources.retain(|&(collection_id, source), skyline| {
            // The expiring pulses governed the source until this sweep, so
            // the before winner spans every retained pulse; the after winner
            // spans only those still live now.
            let before = skyline.winner(i64::MIN).map(|(value, _)| value);
            let retained = skyline.pulses.len();
            skyline.pulses.retain(|deadline, _| *deadline >= now_ms);
            if skyline.pulses.len() == retained { return true; }
            let after = skyline.winner(now_ms).map(|(value, _)| value);
            changes.push(RuntimeStateChange {
                changed: before != after,
                receiver,
                collection_id,
                source,
                next_expiry_ms: skyline.next_deadline(),
            });
            !skyline.pulses.is_empty()
        });
        if sources.is_empty() { self.clear_receiver(receiver); }
        changes
    }

    /// Drop all state and the armed timer of one receiver. The returned
    /// changes are actor-local bookkeeping, not a publication.
    pub(crate) fn remove_receiver(&mut self, receiver: RuntimeReceiverKey) -> Vec<RuntimeStateChange> {
        self.armed.remove(&receiver);
        let Some(sources) = self.receivers.remove(&receiver) else { return Vec::new() };
        sources.into_keys().map(|(collection_id, source)| RuntimeStateChange {
            changed: true,
            receiver,
            collection_id,
            source,
            next_expiry_ms: None,
        }).collect()
    }

    /// Every live source scalar anchored to `ball`, with `char_id` admitting
    /// `Character` receivers, sorted by `(receiver, collection, source)` for
    /// the cold fit loads that snapshot them.
    pub(crate) fn sources_anchored(&self, ball: i64, char_id: Option<i64>, now_ms: i64) -> Vec<RuntimeModifierSource> {
        let mut sources: Vec<RuntimeModifierSource> = self.receivers.iter()
            .filter(|(receiver, _)| anchored_to(receiver, ball, char_id))
            .flat_map(|(receiver, skylines)| {
                // Rebuild the Copy key field-by-field so each variant keeps a
                // real production construction site on this cold-load path.
                let receiver = receiver.canonical();
                skylines.iter().filter_map(move |(&(collection_id, source), skyline)|
                    skyline.winner(now_ms).map(|(value, _)| RuntimeModifierSource { receiver, collection_id, source, value }))
            })
            .collect();
        sources.sort_by_key(|s| (s.receiver, s.collection_id, s.source));
        sources
    }

    /// The receiver's per-collection client state: the folded value of all
    /// its sources' live winners and the latest live deadline across those
    /// sources (the client `Aggregator.GetFinalExpiryTime`). Timed sources
    /// always report `Some` expiry; the `Option` keeps the wire shape for a
    /// future untimed source. Sorted by collection; collections without a
    /// live winner are omitted.
    pub(crate) fn collection_state(&self, receiver: RuntimeReceiverKey, now_ms: i64) -> Vec<(i32, f64, Option<i64>)> {
        let Some(sources) = self.receivers.get(&receiver) else { return Vec::new() };
        let mut collections: BTreeMap<i32, (DbuffAggregate, Vec<f64>, i64)> = BTreeMap::new();
        for (&(collection_id, _), skyline) in sources {
            let Some((value, _)) = skyline.winner(now_ms) else { continue };
            let Some(deadline) = skyline.pulses.iter().filter(|(&d, _)| d >= now_ms).map(|(&d, _)| d).max() else { continue };
            let entry = collections.entry(collection_id).or_insert((skyline.aggregate, Vec::new(), i64::MIN));
            entry.1.push(value);
            entry.2 = entry.2.max(deadline);
        }
        collections.into_iter().map(|(collection_id, (aggregate, values, expiry))|
            (collection_id, fold_runtime_values(aggregate, values).expect("collection state requires a live winner"), Some(expiry))).collect()
    }

    /// The source's current winner as a Copy scalar, or `None` when no pulse
    /// of it is live. The projection carries no expiry.
    pub(crate) fn source_snapshot(
        &self,
        receiver: RuntimeReceiverKey,
        collection_id: i32,
        source: RuntimeSourceKey,
        now_ms: i64,
    ) -> Option<RuntimeModifierSource> {
        let (value, _) = self.receivers.get(&receiver)?.get(&(collection_id, source))?.winner(now_ms)?;
        Some(RuntimeModifierSource { receiver, collection_id, source, value })
    }

    /// Earliest retained deadline across all of the receiver's collections
    /// and sources; a deadline equal to now is still live.
    pub(crate) fn next_expiry(&self, receiver: RuntimeReceiverKey) -> Option<i64> {
        self.receivers.get(&receiver)?.values().filter_map(SourceSkyline::next_deadline).min()
    }

    /// The armed `(generation, deadline)` pair a timer closure was captured
    /// with; `None` when nothing is armed.
    fn armed(&self, receiver: RuntimeReceiverKey) -> Option<(u64, i64)> {
        self.armed.get(&receiver).copied()
    }

    fn armed_deadline(&self, receiver: RuntimeReceiverKey) -> Option<i64> {
        self.armed(receiver).map(|(_, deadline)| deadline)
    }

    /// Take the next monotonic timer generation and record the armed deadline.
    fn arm(&mut self, receiver: RuntimeReceiverKey, deadline: i64) -> u64 {
        self.timer_sequence = self.timer_sequence.checked_add(1).expect("runtime timer generation exhausted");
        self.armed.insert(receiver, (self.timer_sequence, deadline));
        self.timer_sequence
    }

    fn disarm(&mut self, receiver: RuntimeReceiverKey) {
        self.armed.remove(&receiver);
    }

    fn clear_receiver(&mut self, receiver: RuntimeReceiverKey) {
        self.receivers.remove(&receiver);
        self.armed.remove(&receiver);
    }

    /// Drop every receiver anchored to a ball leaving the scene, including
    /// its armed timers.
    fn remove_for_ball(&mut self, ball: i64, char_id: Option<i64>) {
        self.receivers.retain(|receiver, _| !anchored_to(receiver, ball, char_id));
        self.armed.retain(|receiver, _| !anchored_to(receiver, ball, char_id));
    }
}

impl System {
    /// Land one timed runtime input on a live receiver and re-arm the
    /// receiver's earliest expiry. A dead receiver or unknown collection is
    /// an internal refusal, never a silent acceptance.
    pub(crate) fn apply_runtime_input(&mut self, input: TimedRuntimeInput) -> Result<RuntimeStateChange, CallError> {
        if self.runtime_receiver_token(input.receiver).is_none() {
            return Err(CallError::Internal(anyhow::anyhow!("runtime receiver has no live ball")));
        }
        let aggregate = self.sde()
            .and_then(|sde| sde.dbuffs.get(input.collection_id))
            .map(|collection| collection.aggregate)
            .ok_or_else(|| CallError::Internal(anyhow::anyhow!("unknown dbuff collection")))?;
        let change = self.runtime_inputs.apply_timed(input, aggregate, self.now_ms())?;
        self.arm_runtime_timer(input.receiver);
        Ok(change)
    }

    /// Expire every retained deadline of the receiver strictly before now and
    /// re-arm its earliest deadline. A receiver whose ball is gone loses its
    /// actor state instead and reports no change.
    pub(crate) fn expire_runtime_inputs(&mut self, receiver: RuntimeReceiverKey) -> Vec<RuntimeStateChange> {
        if self.runtime_receiver_token(receiver).is_none() {
            self.runtime_inputs.remove_receiver(receiver);
            return Vec::new();
        }
        let changes = self.runtime_inputs.expire_before(receiver, self.now_ms());
        self.arm_runtime_timer(receiver);
        changes
    }

    /// Live source scalars anchored to `ball` for a cold fit load; `char_id`
    /// admits `Character` receivers. Sorted by `(receiver, collection, source)`.
    pub(crate) fn runtime_sources_anchored(&self, ball: i64, char_id: Option<i64>) -> Vec<RuntimeModifierSource> {
        self.runtime_inputs.sources_anchored(ball, char_id, self.now_ms())
    }

    /// The receiver's per-collection folded value and final expiry for the
    /// client dbuff tracker.
    pub(crate) fn runtime_collection_state(&self, receiver: RuntimeReceiverKey) -> Vec<(i32, f64, Option<i64>)> {
        self.runtime_inputs.collection_state(receiver, self.now_ms())
    }

    /// Send the ship's complete current dbuff state to its pilot: the client
    /// tracker replaces its whole state per message and an empty list clears it.
    pub(in crate::space) fn notify_runtime_state(&self, ship: i64) {
        let Some(pilot) = self.ball(ship).and_then(|ball| ball.char_id) else { return };
        let entries = self.runtime_collection_state(RuntimeReceiverKey::Ship(ship));
        self.notify_pilot(pilot, "OnDbuffUpdated", crate::services::effects::notify::command_burst_dbuff_updated(ship, &entries));
    }

    /// Publish the resolved changes once and send every affected ship's pilot
    /// its complete dbuff state: the ships the fit publication moved, plus
    /// any receivers the caller names (a landed pulse moves the final expiry
    /// even when no scalar changed).
    fn publish_runtime_state(&mut self, changes: &[RuntimeStateChange], extra: impl IntoIterator<Item = i64>) {
        let mut ships = self.publish_runtime_changes(changes);
        for ship in extra {
            if !ships.contains(&ship) { ships.push(ship); }
        }
        for ship in ships { self.notify_runtime_state(ship); }
    }

    /// Land timed runtime inputs from any producer: apply each to the actor
    /// skyline (a refused input is logged and skipped), publish the resident
    /// fit changes once for the batch, then send every `Ship` receiver's
    /// pilot its complete dbuff state (a pulse always moves the final
    /// expiry). Returns how many inputs were accepted.
    pub(crate) fn land_runtime_inputs(&mut self, inputs: &[TimedRuntimeInput]) -> usize {
        let mut changes = Vec::with_capacity(inputs.len());
        let mut ships: Vec<i64> = Vec::new();
        for input in inputs {
            match self.apply_runtime_input(*input) {
                Ok(change) => {
                    changes.push(change);
                    if let RuntimeReceiverKey::Ship(ship) = input.receiver {
                        if !ships.contains(&ship) { ships.push(ship); }
                    }
                }
                Err(error) => tracing::warn!(?error, receiver = ?input.receiver,
                    collection = input.collection_id, producer = ?input.source.producer,
                    "runtime input refused"),
            }
        }
        let accepted = changes.len();
        self.publish_runtime_state(&changes, ships);
        accepted
    }

    /// Actor-local read of one source's current winner; no DB or World call.
    pub(crate) fn runtime_source_snapshot(
        &self,
        receiver: RuntimeReceiverKey,
        collection_id: i32,
        source: RuntimeSourceKey,
    ) -> Option<RuntimeModifierSource> {
        self.runtime_inputs.source_snapshot(receiver, collection_id, source, self.now_ms())
    }

    /// The receiver's live ball incarnation token, lazily allocated for a
    /// present ball. Ship and module receivers fence on their ship ball,
    /// drones on the drone ball and characters on the piloted ball; `None`
    /// when no live ball anchors the receiver.
    fn runtime_receiver_token(&mut self, receiver: RuntimeReceiverKey) -> Option<BallToken> {
        let ball = match receiver {
            RuntimeReceiverKey::Ship(ship) | RuntimeReceiverKey::Module { ship, .. } => ship,
            RuntimeReceiverKey::Drone { drone, .. } => drone,
            RuntimeReceiverKey::Character(character) => self.pilot_ball(character)?.id,
        };
        self.capture_ball(ball)
    }

    /// Arm the receiver's earliest retained deadline at `deadline + 1`: an
    /// entry at its exact deadline is still live, so the first due firing is
    /// the next millisecond. An unrepresentable `i64::MAX + 1` stays live
    /// unarmed until a finite deadline or receiver cleanup.
    fn arm_runtime_timer(&mut self, receiver: RuntimeReceiverKey) {
        let next = self.runtime_inputs.next_expiry(receiver);
        if next == self.runtime_inputs.armed_deadline(receiver) { return; }
        let Some(token) = self.runtime_receiver_token(receiver) else {
            self.runtime_inputs.disarm(receiver);
            return;
        };
        match next {
            Some(deadline) => {
                let generation = self.runtime_inputs.arm(receiver, deadline);
                if let Some(due) = deadline.checked_add(1) {
                    self.schedule_at_named(due, RUNTIME_TIMER_NAME, move |system| {
                        system.expire_runtime_timer(receiver, token, generation, deadline);
                    });
                }
            }
            None => self.runtime_inputs.disarm(receiver),
        }
    }

    /// Named receiver-expiry callback. Returns without mutation unless the
    /// captured generation, the captured ball incarnation and the armed
    /// deadline all still hold; a valid firing expires the receiver, re-arms
    /// its next deadline and lands the scalar changes through the resident
    /// fit publication. Never awaits, locks or touches DB or World.
    fn expire_runtime_timer(&mut self, receiver: RuntimeReceiverKey, token: BallToken, generation: u64, deadline: i64) {
        if self.runtime_inputs.armed(receiver) != Some((generation, deadline))
            || !self.ball_token_current(token)
            || self.runtime_inputs.next_expiry(receiver) != Some(deadline)
        {
            return;
        }
        let changes = self.expire_runtime_inputs(receiver);
        for change in &changes {
            tracing::debug!(receiver = ?change.receiver, collection = change.collection_id, producer = ?change.source.producer,
                source = change.source.source_id, incarnation = change.source.incarnation, changed = change.changed,
                next_expiry_ms = ?change.next_expiry_ms, "runtime input expired");
        }
        self.publish_runtime_state(&changes, std::iter::empty());
    }

    /// Drop every runtime receiver anchored to a ball leaving the scene: the
    /// ship itself, its module and drone keys, and the pilot's character key.
    pub(in crate::space) fn remove_runtime_ball(&mut self, ball: i64, char_id: Option<i64>) {
        self.runtime_inputs.remove_for_ball(ball, char_id);
    }
}

#[cfg(all(test, feature = "tests-space"))]
mod tests;
