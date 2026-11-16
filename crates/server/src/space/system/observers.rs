//! Actor-owned ordered Destiny delivery and observer binding isolation.
//! Events are consumed by `eve/client/script/remote/michelle.py`; bindings,
//! pending batches and instance tokens are actor-owned in-memory state.
//! Each binding is pending until the boundary projection delivers its
//! SetState; pending observers receive no ordinary scene traffic, every
//! binding carries a fresh epoch even on the same channel, and the one-shot
//! completion resolves only after the first delivery actually sent.
//! Scene membership itself lives in `interest`/`membership`; this module
//! owns channels, epochs and the counted fanout entry.
//! A committed ship loss re-aims the departed ego's surviving informed
//! bindings to its replacement through one queued FIFO handoff.

use super::*;
use super::interest::BubbleId;
use super::membership::SceneCtx;

/// What one binding was last delivered; `bubble` is `None` for an observer
/// admitted on the global layer alone.
pub(super) struct Informed {
    pub(super) bubble: Option<BubbleId>,
    pub(super) ego_token: Option<BallToken>,
}

pub(super) struct Observer {
    pub(super) ego: i64,
    pub(super) epoch: u64,
    pub(super) tx: DestinyTx,
    pub(super) pending: Vec<Value>,
    /// The delivered scene identity; `None` while the initial state is owed.
    pub(super) informed: Option<Informed>,
    /// One-shot registration result owed to the registering future. Its loss
    /// is never a cancellation: the binding lives by DestinyTx, epoch and
    /// prune alone, and a failed send never undoes a delivered SetState.
    pub(super) completion: Option<tokio::sync::oneshot::Sender<bool>>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct ObserverToken { pub(super) client: i64, pub(super) epoch: u64 }

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct BallToken { id: i64, serial: u64 }
impl BallToken {
    pub(in crate::space) fn new(id: i64, serial: u64) -> Self { Self { id, serial } }
    pub(in crate::space) fn id(self) -> i64 { self.id }
    pub(super) fn serial(self) -> u64 { self.serial }
}

/// The recipient sets of the counted delivery; every variant except
/// `Observer` needs an informed binding, so pending initials never see
/// ordinary scene traffic.
pub(in crate::space) enum DestinyAudience {
    /// Every informed observer.
    System,
    /// Informed observers whose committed scene covers the ball; its pilot
    /// is covered even while it is cloaked.
    Visible { id: i64, cloak: u8 },
    /// The one informed binding whose delivered ego is this exact token.
    ExactEgo(BallToken),
    /// Every informed holder except the binding of this exact ego token.
    /// Only a confirmed global reveal uses it.
    GlobalExceptEgo(BallToken),
    Observer(ObserverToken),
    /// Informed observers last-informed on one bubble, except pilots of one
    /// ball.
    Scene { bubble: BubbleId, except_ego: Option<i64> },
    /// The recipients of a ball's last delivered scene: its committed
    /// bubble, the always-present layer, or only its own pilot when cloaked.
    Departed { token: BallToken, cloak: u8, bubble: Option<BubbleId>, global: bool },
    /// The observers that held the exact token through its captured home
    /// before a native action and still hold it after: bindings the settle
    /// just rewrote get their state in the Add and never see the history.
    Retained { token: BallToken, bubble: Option<BubbleId>, global: bool },
}

impl System {
    pub(crate) fn capture_ball(&mut self, id: i64) -> Option<BallToken> {
        if !self.balls.contains_key(&id) { return None; }
        let serial = *self.ball_tokens.entry(id).or_insert_with(|| {
            self.ball_token_serial = self.ball_token_serial.checked_add(1).expect("ball token exhausted");
            self.ball_token_serial
        });
        Some(BallToken { id, serial })
    }
    pub(crate) fn pilot_observer(&self, character: i64) -> Option<(i64, ObserverToken)> {
        let ego = self.pilot_ball(character)?.id;
        self.observers.iter().find(|(_, observer)| observer.ego == ego && observer.informed.is_some() && !observer.tx.is_closed())
            .map(|(&client, observer)| (ego, ObserverToken { client, epoch: observer.epoch }))
    }
    pub(super) fn remove_ego_observers(&mut self, ego: i64) {
        let tokens: Vec<_> = self.observers.iter().filter(|(_, o)| o.ego == ego)
            .map(|(&client, o)| ObserverToken { client, epoch: o.epoch }).collect();
        for token in tokens { self.remove_observer(token); }
    }
    pub(crate) fn ball_token_current(&self, token: BallToken) -> bool {
        self.ball_tokens.get(&token.id) == Some(&token.serial) && self.balls.contains_key(&token.id)
    }
    pub(crate) fn observer_ego(&self, client: i64) -> Option<i64> {
        self.observers.get(&client).map(|observer| observer.ego)
    }
    /// A ticket valid for scene-bound sends: only an informed binding has a
    /// scene to address.
    pub(crate) fn observer_token(&self, client: i64) -> Option<ObserverToken> {
        self.observers.get(&client).filter(|observer| observer.informed.is_some() && !observer.tx.is_closed())
            .map(|observer| ObserverToken { client, epoch: observer.epoch })
    }
    pub(in crate::space) fn set_observer_ego(&mut self, client: i64, ego: i64) {
        let Some(observer) = self.observers.get_mut(&client) else { return };
        let (old, epoch) = (observer.ego, observer.epoch);
        if old == ego { return; }
        observer.ego = ego;
        self.scene_ego_changed(ObserverToken { client, epoch }, old, ego);
    }

    /// Admit a committed ship loss's replacement while the departed ego's
    /// informed bindings are still captured. The old exit and the new birth
    /// already ride the FIFO ahead of the one bounded handoff queued here; it
    /// only re-aims bindings whose tickets still hold the departed ego, then
    /// the ordinary settle produces the replacement Add. Stale epochs, a
    /// different incarnation on the old id or a moved session leave every
    /// binding untouched.
    pub(crate) fn add_loss_replacement(
        &mut self,
        departed: BallToken,
        ball: Ball,
        expected: Option<infinity_core::session::OnlineSession>,
    ) {
        let tickets = self.scene_observers(&DestinyAudience::ExactEgo(departed));
        let (id, character) = (ball.id, ball.char_id);
        self.add_ship(ball);
        let replacement = self.capture_ball(id).expect("inserted ball");
        if tickets.is_empty() { return; }
        self.actions.push_back((self.current_time, Box::new(move |s, _| {
            if !s.ball_token_current(replacement)
                || s.ball_token(departed.id()).is_some_and(|current| current != replacement)
                || character.is_some_and(|c| !s.combat_session_current(c, expected.as_ref())) { return; }
            for (ticket, _) in tickets {
                if s.observer_token(ticket.client) != Some(ticket)
                    || s.ticket_ego(ticket) != Some(departed.id())
                    || !s.observers.get(&ticket.client).is_some_and(|observer|
                        observer.informed.as_ref().is_some_and(|i| i.ego_token == Some(departed))) { continue; }
                s.set_observer_ego(ticket.client, replacement.id());
            }
        })));
    }
    fn eligible(ctx: &SceneCtx<'_>, client: i64, observer: &Observer, audience: &DestinyAudience, cutoff: u64) -> bool {
        if observer.epoch > cutoff || observer.tx.is_closed() { return false; }
        match audience {
            DestinyAudience::Observer(token) => token.client == client && token.epoch == observer.epoch,
            _ => {
                if observer.informed.is_none() { return false; }
                match audience {
                    DestinyAudience::System => true,
                    DestinyAudience::Visible { id, cloak } => (*cloak == 0 || observer.ego == *id) && ctx.covers(observer, *id),
                    DestinyAudience::ExactEgo(token) => observer.informed.as_ref().is_some_and(|i| i.ego_token == Some(*token)),
                    DestinyAudience::GlobalExceptEgo(token) => observer.informed.as_ref().is_some_and(|i| i.ego_token != Some(*token)),
                    DestinyAudience::Scene { bubble, except_ego } => {
                        observer.informed.as_ref().is_some_and(|i| i.bubble == Some(*bubble))
                            && except_ego.is_none_or(|e| e != observer.ego)
                    }
                    DestinyAudience::Departed { token, cloak, bubble, global } => {
                        observer.informed.as_ref().is_some_and(|i| i.ego_token == Some(*token))
                            || (*cloak == 0 && (*global || bubble.is_some_and(|b| observer.informed.as_ref().is_some_and(|i| i.bubble == Some(b)))))
                    }
                    DestinyAudience::Retained { token, bubble, global } => {
                        let Some(informed) = &observer.informed else { return false };
                        if ctx.relocated.contains(&client) { return false; }
                        let held = informed.ego_token == Some(*token) || *global
                            || (bubble.is_some() && informed.bubble == *bubble);
                        held && (informed.ego_token == Some(*token) || ctx.global.contains(token)
                            || informed.bubble.is_some_and(|b| ctx.committed.get(&b).is_some_and(|s| s.contains(token))))
                    }
                    DestinyAudience::Observer(_) => unreachable!(),
                }
            }
        }
    }
    fn send_observer(observer: &mut Observer, batching: bool, events: Vec<Value>) -> bool {
        if observer.tx.is_closed() { return false; }
        if batching { observer.pending.extend(events); true }
        else { observer.tx.send(SceneDelivery::Destiny(events)).is_ok() }
    }
    pub(super) fn emit_before(&mut self, audience: DestinyAudience, events: Vec<Value>, cutoff: u64) -> bool {
        let started = std::time::Instant::now();
        let submitted = events.len() as u64;
        let mut copies = 0;
        let sent = self.emit_before_counted(audience, events, cutoff, &mut copies);
        self.attribution.delivered(submitted, copies, started.elapsed().as_nanos());
        sent
    }
    fn emit_before_counted(&mut self, audience: DestinyAudience, events: Vec<Value>, cutoff: u64, copies: &mut u64) -> bool {
        if events.is_empty() { return false; }
        if let DestinyAudience::Observer(token) = &audience {
            if let std::collections::hash_map::Entry::Occupied(mut entry) = self.observers.entry(token.client) {
                if entry.get().epoch != token.epoch || token.epoch > cutoff { return false; }
                *copies += 1;
                if Self::send_observer(entry.get_mut(), self.delivery_batching, events) { return true; }
                drop(entry);
                if !self.fail_initial(*token) { self.remove_observer(*token); }
            }
            return false;
        }
        let recipients: Vec<i64> = {
            let ctx = SceneCtx::new(&self.balls, &self.ball_tokens, &self.membership);
            let mut recipients = Vec::new();
            self.for_audience_clients(&ctx, &audience, |client| {
                if self.observers.get(&client).is_some_and(|observer| Self::eligible(&ctx, client, observer, &audience, cutoff)) {
                    recipients.push(client);
                }
            });
            recipients
        };
        if recipients.is_empty() { self.prune_observers(); return false; }
        let batching = self.delivery_batching;
        let mut remaining = recipients.len();
        let mut original = Some(events);
        let mut sent = false;
        let mut dead = Vec::new();
        for client in recipients {
            let Some(observer) = self.observers.get_mut(&client) else { continue };
            let drop_dead = |o: &mut Observer| (o.epoch, o.ego,
                o.informed.as_ref().and_then(|i| i.bubble), o.completion.take());
            if observer.tx.is_closed() {
                dead.push((client, drop_dead(observer)));
                self.observers.remove(&client);
                continue;
            }
            remaining -= 1;
            let events = if remaining == 0 { original.take().unwrap() } else { original.as_ref().unwrap().clone() };
            *copies += 1;
            let ok = Self::send_observer(observer, batching, events);
            sent |= ok;
            // A refused send retires the binding like a closed channel:
            // pending completion answered once, indices dropped, previews reset.
            if !ok {
                let dead_one = drop_dead(observer);
                self.observers.remove(&client);
                dead.push((client, dead_one));
            }
        }
        for (client, (epoch, ego, bubble, done)) in dead {
            if let Some(done) = done { let _ = done.send(false); }
            self.scene_binding_removed(ObserverToken { client, epoch }, ego, bubble);
            self.gm_effect_observer_reset(client);
        }
        sent
    }
    /// Visit client ids the audience might address. System and a confirmed
    /// global reveal visit every binding; a bubble or an ego visits only its
    /// index. A client id is not itself a delivery authorization.
    fn for_audience_clients(&self, ctx: &SceneCtx<'_>, audience: &DestinyAudience, mut visit: impl FnMut(i64)) {
        let ids = |set: Option<&HashSet<i64>>, visit: &mut dyn FnMut(i64)| {
            if let Some(set) = set { for &client in set { visit(client); } }
        };
        let union = |local: Option<&HashSet<i64>>, ego: Option<&HashSet<i64>>, visit: &mut dyn FnMut(i64)| {
            ids(local, visit);
            if let Some(ego) = ego {
                for &client in ego {
                    if !local.is_some_and(|set| set.contains(&client)) { visit(client); }
                }
            }
        };
        match audience {
            DestinyAudience::System | DestinyAudience::GlobalExceptEgo(_) => {
                for &client in self.observers.keys() { visit(client); }
            }
            DestinyAudience::Scene { bubble, .. } => ids(ctx.bubble_bindings.get(bubble), &mut visit),
            DestinyAudience::ExactEgo(BallToken { id: ego, .. }) => ids(ctx.egos.get(ego), &mut visit),
            DestinyAudience::Visible { id, .. } => {
                let Some(token) = self.ball_token(*id) else { return };
                let (home, global) = self.scene_home(token);
                if global { for &client in self.observers.keys() { visit(client); } }
                else { union(home.and_then(|b| ctx.bubble_bindings.get(&b)), ctx.egos.get(id), &mut visit); }
            }
            DestinyAudience::Departed { token, cloak, bubble, global } => {
                if *cloak == 0 && *global { for &client in self.observers.keys() { visit(client); } }
                else {
                    let local = if *cloak == 0 { bubble.and_then(|b| ctx.bubble_bindings.get(&b)) } else { None };
                    union(local, ctx.egos.get(&token.id()), &mut visit);
                }
            }
            DestinyAudience::Retained { token, bubble, global } => {
                if *global { for &client in self.observers.keys() { visit(client); } }
                else { union(bubble.and_then(|b| ctx.bubble_bindings.get(&b)), ctx.egos.get(&token.id()), &mut visit); }
            }
            DestinyAudience::Observer(token) => visit(token.client),
        }
    }
    pub(in crate::space) fn emit_destiny(&mut self, audience: DestinyAudience, events: Vec<Value>) -> bool {
        self.emit_before(audience, events, u64::MAX)
    }
    pub(crate) fn publish_destiny(&mut self, about: i64, factory: impl FnOnce(i64) -> Vec<Value>) {
        let Some(ball) = self.ball(about) else { return };
        let audience = DestinyAudience::Visible { id: ball.id, cloak: ball.cloak };
        let mut has = false;
        {
            let ctx = SceneCtx::new(&self.balls, &self.ball_tokens, &self.membership);
            self.for_audience_clients(&ctx, &audience, |client| {
                has |= self.observers.get(&client).is_some_and(|observer| Self::eligible(&ctx, client, observer, &audience, u64::MAX));
            });
        }
        if has {
            let events = factory(self.current_time);
            self.emit_destiny(audience, events);
        } else { self.prune_observers(); }
    }
    pub(super) fn begin_destiny_batch(&mut self) { self.delivery_batching = true; }
    pub(super) fn finish_destiny_batch(&mut self) {
        self.delivery_batching = false;
        let mut dead = Vec::new();
        self.observers.retain(|&client, observer| {
            let keep = !observer.tx.is_closed()
                && (observer.pending.is_empty()
                    || observer.tx.send(SceneDelivery::Destiny(std::mem::take(&mut observer.pending))).is_ok());
            if !keep {
                dead.push((client, observer.epoch, observer.ego,
                    observer.informed.as_ref().and_then(|i| i.bubble), observer.completion.take()));
            }
            keep
        });
        for (client, epoch, ego, bubble, done) in dead {
            if let Some(done) = done { let _ = done.send(false); }
            self.scene_binding_removed(ObserverToken { client, epoch }, ego, bubble);
            self.gm_effect_observer_reset(client);
        }
    }
    pub(super) fn prune_observers(&mut self) {
        let tokens: Vec<_> = self.observers.iter().filter(|(_, o)| o.tx.is_closed())
            .map(|(&client, o)| ObserverToken { client, epoch: o.epoch }).collect();
        for token in tokens { self.remove_observer(token); }
    }

    /// Accept one binding as pending: epoch always advances, a superseded
    /// pending completion answers false exactly once, GM previews reset and
    /// a closed channel is refused. The initial state is owed until a real
    /// boundary projection delivers it and sends the completion.
    pub fn observe(&mut self, client_id: i64, ego: i64, tx: DestinyTx, completed: tokio::sync::oneshot::Sender<bool>) -> bool {
        self.gm_effect_observer_reset(client_id);
        if let Some(old) = self.observers.remove(&client_id) {
            if let Some(pending) = old.completion { let _ = pending.send(false); }
            let old_token = ObserverToken { client: client_id, epoch: old.epoch };
            self.scene_binding_removed(old_token, old.ego, old.informed.and_then(|i| i.bubble));
        }
        self.observer_epoch = self.observer_epoch.checked_add(1).expect("observer epoch exhausted");
        if tx.is_closed() { let _ = completed.send(false); return false; }
        self.commit_cold_scene();
        let epoch = self.observer_epoch;
        self.observers.insert(client_id, Observer { ego, epoch, tx, pending: Vec::new(), informed: None, completion: Some(completed) });
        self.scene_binding_added(ObserverToken { client: client_id, epoch }, ego);
        true
    }

    pub(super) fn finish_initial(&mut self, observer: ObserverToken) -> bool {
        let Some(entry) = self.observers.get_mut(&observer.client)
            .filter(|entry| entry.epoch == observer.epoch && entry.informed.is_some()) else { return false };
        let Some(done) = entry.completion.take() else { return false };
        let _ = done.send(true);
        true
    }
    fn remove_observer(&mut self, observer: ObserverToken) -> bool {
        if !self.observers.get(&observer.client).is_some_and(|entry| entry.epoch == observer.epoch) { return false; }
        let entry = self.observers.remove(&observer.client).unwrap();
        if let Some(done) = entry.completion { let _ = done.send(false); }
        self.scene_binding_removed(observer, entry.ego, entry.informed.and_then(|i| i.bubble));
        self.gm_effect_observer_reset(observer.client);
        true
    }
    pub(super) fn fail_initial(&mut self, observer: ObserverToken) -> bool {
        if !self.observers.get(&observer.client).is_some_and(|entry|
            entry.epoch == observer.epoch && entry.completion.is_some()) { return false; }
        self.remove_observer(observer)
    }

    /// Flush one observer's queued Destiny batch ahead of a scene-bound notify.
    /// A stale ticket or dead channel reports false; a failed send removes the
    /// observer rather than falling back to the ordinary channel.
    pub(in crate::space) fn flush_observer_pending(&mut self, observer: ObserverToken) -> bool {
        let Some(entry) = self.observers.get_mut(&observer.client) else { return false };
        if entry.epoch != observer.epoch { return false; }
        if entry.pending.is_empty()
            || entry.tx.send(SceneDelivery::Destiny(std::mem::take(&mut entry.pending))).is_ok() { return true; }
        self.remove_observer(observer);
        false
    }

    /// Send a scene-bound notify on the observer's own channel, flushing its
    /// pending Destiny first so the batch keeps its position ahead of it.
    pub(crate) fn emit_scene_notify(&mut self, observer: ObserverToken, event: &'static str, args: Vec<Value>) -> bool {
        if self.observers.get(&observer.client).is_none_or(|e| e.epoch != observer.epoch || e.informed.is_none()) { return false; }
        if !self.flush_observer_pending(observer) { return false; }
        let Some(entry) = self.observers.get_mut(&observer.client) else { return false };
        if entry.tx.send(SceneDelivery::Notify { event, args }).is_ok() { return true; }
        self.remove_observer(observer);
        false
    }

    /// Scene-bound notify to the live informed observer currently piloting `character`'s ship.
    pub(crate) fn notify_scene_pilot(&mut self, character: i64, event: &'static str, args: Vec<Value>) -> bool {
        let Some((_, token)) = self.pilot_observer(character) else { return false };
        self.emit_scene_notify(token, event, args)
    }

    /// Snapshot the currently eligible recipients; not a durable delivered set.
    pub(in crate::space) fn scene_observers(&self, audience: &DestinyAudience) -> Vec<(ObserverToken, i64)> {
        let ctx = SceneCtx::new(&self.balls, &self.ball_tokens, &self.membership);
        let mut out = Vec::new();
        self.for_audience_clients(&ctx, audience, |client| {
            let Some(observer) = self.observers.get(&client) else { return };
            if !Self::eligible(&ctx, client, observer, audience, u64::MAX) { return; }
            out.push((ObserverToken { client, epoch: observer.epoch }, observer.ego));
        });
        out
    }

    /// The pilot character whose ball a live observer's ego currently is.
    pub(in crate::space) fn observer_character(&self, observer: ObserverToken) -> Option<i64> {
        self.observers.get(&observer.client).filter(|entry| entry.epoch == observer.epoch)
            .and_then(|entry| self.ball(entry.ego).and_then(|ball| ball.char_id))
    }

    /// The ego of a still-current observer ticket; stale tickets get nothing.
    pub(in crate::space) fn ticket_ego(&self, observer: ObserverToken) -> Option<i64> {
        self.observers.get(&observer.client).filter(|entry| entry.epoch == observer.epoch).map(|entry| entry.ego)
    }
    pub fn unobserve(&mut self, client_id: i64) {
        if let Some(epoch) = self.observers.get(&client_id).map(|entry| entry.epoch) {
            self.remove_observer(ObserverToken { client: client_id, epoch });
        } else {
            self.gm_effect_observer_reset(client_id);
        }
    }
}

#[cfg(all(test, feature = "tests-space"))]
mod tests;
