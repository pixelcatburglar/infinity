//! Scene readiness and the shared per-bubble committed rosters projected
//! onto observer channels. A ball joins a roster only after its own queued
//! birth entry applies, so the single action FIFO also orders births against
//! every other scene change. `interest` stays the only current topology;
//! `Membership` is the one owner of the ready projection (the layer and home
//! each born token aims at), the last-delivered committed rosters, every
//! reverse index over them and the dedup dirty sets. Each observed bubble
//! keeps one committed token set; a settle emits shared remove-then-add
//! deltas once per dirty bubble, then per-binding transfers, replayed
//! effects, fleet snapshots and SKIN entries. A bubble without observers
//! runs real topology and physics but publishes nothing, and its committed
//! state is released when its last binding leaves. A settle with no dirty
//! work returns without touching rosters, two-set observer views or effect
//! scans. The rosters are `eve/client/script/remote/michelle.py`
//! AddBalls/RemoveBalls deltas; pending initials are presented from the
//! fresh committed state.

use super::*;
use super::interest::BubbleId;
use super::observers::{BallToken, DestinyAudience, Observer, ObserverToken};
use super::presentation::SceneView;
use super::tick::{add_balls_batch, remove_balls_event};

mod settle;
/// The aimed or delivered layer of one token.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Home { Bubble(BubbleId), Global }

/// One born token's projection record: `ready` is the layer the projector
/// aims at (`None` while the token is hidden or its ball is unplaced) and
/// `published` is the home it was last delivered through. The entry itself
/// is the birth-ready identity: a cloaked ball keeps it with no aimed
/// layer, and a real exit removes it only at the queued exit's own FIFO
/// position.
struct Publication {
    ready: Option<Home>,
    published: Option<Home>,
}

/// Publication state beside the topology: the ready projection, the
/// delivered rosters, the reverse indices and the dedup dirty sets.
/// Observers own their channels, epochs and completions; every recipient
/// index lives here exactly once.
#[derive(Default)]
pub(in crate::space) struct Membership {
    /// Per born token: aimed layer and last delivered home.
    pubs: HashMap<BallToken, Publication>,
    /// The aimed member roster per bubble: born, uncloaked and not global.
    ready: HashMap<BubbleId, HashSet<BallToken>>,
    /// The aimed always-present roster.
    ready_global: HashSet<BallToken>,
    /// The last delivered member tokens per observed bubble.
    pub(super) committed: HashMap<BubbleId, HashSet<BallToken>>,
    /// The last delivered always-present roster shared by every scene.
    pub(super) global_committed: HashSet<BallToken>,
    /// Balls currently authorized for the always-present layer.
    pub(super) globals: HashSet<i64>,
    /// Informed bindings per observed bubble, by client id.
    bubble_bindings: HashMap<BubbleId, HashSet<i64>>,
    /// Observer client to its ego ball id.
    binding_ego: HashMap<i64, i64>,
    /// Ego ball id to its observer clients.
    egos: HashMap<i64, HashSet<i64>>,
    /// Bindings whose delivered scene the last settle rewrote; a Retained
    /// audience must not replay the action that just delivered them.
    pub(super) relocated: HashSet<i64>,
    /// Dedup ids of the next settle's work; never event queues.
    dirty_bubbles: HashSet<BubbleId>,
    dirty_global: bool,
    dirty_bindings: HashSet<i64>,
    pending_initials: HashSet<i64>,
    active_collisions: Option<bool>,
    /// Every insert queues a FIFO birth once the cold baseline committed.
    births_queued: bool,
}

impl Membership {
    /// Re-aim one born token's ready layer, keeping the grouped sets in
    /// sync; `mark` records the touched layers dirty for the next settle.
    fn aim(&mut self, token: BallToken, home: Option<Home>, mark: bool) {
        let Some(entry) = self.pubs.get_mut(&token) else { return };
        let old = std::mem::replace(&mut entry.ready, home);
        if old == home { return; }
        if let Some(h) = old { self.remove_ready(token, h); }
        if let Some(h) = home { self.insert_ready(token, h); }
        if mark {
            for h in [old, home].into_iter().flatten() {
                match h {
                    Home::Bubble(b) => { self.dirty_bubbles.insert(b); }
                    Home::Global => self.dirty_global = true,
                }
            }
        }
    }
    fn insert_ready(&mut self, token: BallToken, home: Home) {
        match home {
            Home::Bubble(b) => { self.ready.entry(b).or_default().insert(token); }
            Home::Global => { self.ready_global.insert(token); }
        }
    }
    fn remove_ready(&mut self, token: BallToken, home: Home) {
        match home {
            Home::Bubble(b) => {
                if let Some(set) = self.ready.get_mut(&b) {
                    set.remove(&token);
                    if set.is_empty() { self.ready.remove(&b); }
                }
            }
            Home::Global => { self.ready_global.remove(&token); }
        }
    }
    /// The delivered-home reverse index entry for one token.
    fn publish(&mut self, token: BallToken, home: Option<Home>) {
        if let Some(entry) = self.pubs.get_mut(&token) { entry.published = home; }
    }
    /// Release an unobserved bubble's delivered roster once; its tokens keep
    /// their ready aim but no delivered home. Returns the released tokens.
    fn release_bubble(&mut self, bubble: BubbleId) -> Vec<BallToken> {
        let mut released = Vec::new();
        if let Some(set) = self.committed.remove(&bubble) {
            for token in set {
                self.publish(token, None);
                released.push(token);
            }
        }
        released
    }
    /// Rewrite one token's published-home reverse index from its own aimed
    /// home checked against the delivered roster that home names; a token
    /// whose aim was not actually delivered keeps no published home.
    fn resolve_published(&mut self, token: BallToken) {
        let home = self.pubs.get(&token).and_then(|p| p.ready).filter(|h| match h {
            Home::Global => self.global_committed.contains(&token),
            Home::Bubble(b) => self.committed.get(b).is_some_and(|set| set.contains(&token)),
        });
        if let Some(entry) = self.pubs.get_mut(&token) { entry.published = home; }
    }
}

/// Read-only view of the delivered membership for audience selection inside
/// the counted fanout; never a second visibility authority.
pub(super) struct SceneCtx<'a> {
    pub(super) balls: &'a HashMap<i64, Ball>,
    pub(super) tokens: &'a HashMap<i64, u64>,
    pub(super) committed: &'a HashMap<BubbleId, HashSet<BallToken>>,
    pub(super) global: &'a HashSet<BallToken>,
    /// Bindings the running or last settle just rewrote.
    pub(super) relocated: &'a HashSet<i64>,
    /// Informed bindings per observed bubble, by client id. A client id is
    /// only a candidate: the caller still checks the live observer.
    pub(super) bubble_bindings: &'a HashMap<BubbleId, HashSet<i64>>,
    /// Ego ball id to its observer clients, including bindings still pending
    /// their first delivery.
    pub(super) egos: &'a HashMap<i64, HashSet<i64>>,
}
impl<'a> SceneCtx<'a> {
    pub(super) fn new(balls: &'a HashMap<i64, Ball>, tokens: &'a HashMap<i64, u64>,
        membership: &'a Membership) -> Self {
        Self {
            balls, tokens,
            committed: &membership.committed, global: &membership.global_committed,
            relocated: &membership.relocated,
            bubble_bindings: &membership.bubble_bindings, egos: &membership.egos,
        }
    }
    pub(super) fn covers(&self, observer: &Observer, id: i64) -> bool {
        let Some(informed) = &observer.informed else { return false };
        let Some(ball) = self.balls.get(&id) else { return false; };
        let Some(&serial) = self.tokens.get(&id) else { return false; };
        let token = BallToken::new(id, serial);
        if informed.ego_token == Some(token) { return true; }
        if ball.cloak != 0 { return false; }
        self.global.contains(&token)
            || informed.bubble.is_some_and(|b| self.committed.get(&b).is_some_and(|set| set.contains(&token)))
    }
}

impl System {
    pub(super) fn admit_scene_ball(&mut self, token: BallToken) {
        if self.membership.births_queued {
            self.actions.push_back((self.current_time, Box::new(move |s, _| {
                if s.ball_token_current(token) { s.scene_born(token); }
            })));
        } else {
            self.scene_born(token);
        }
    }
    /// A birth entry applying at its own FIFO position: the token becomes
    /// ready and aims at its real layer. The caller already checked the
    /// token is still current, so a stale birth never activates a same-id
    /// successor.
    fn scene_born(&mut self, token: BallToken) {
        let id = token.id();
        let aimed = {
            let Some(ball) = self.balls.get(&id) else { return };
            if ball.cloak != 0 { None }
            else if self.membership.globals.contains(&id) { Some(Home::Global) }
            else { self.interest.bubble_of(id).map(Home::Bubble) }
        };
        self.membership.pubs.insert(token, Publication { ready: None, published: None });
        self.membership.aim(token, aimed, true);
        if let Some(clients) = self.membership.egos.get(&id) {
            self.membership.dirty_bindings.extend(clients.iter().copied());
        }
    }
    pub(in crate::space) fn commit_cold_scene(&mut self) { self.membership.births_queued = true; }
    /// A ball's settled position folded back into topology and the ready
    /// projection. Current placement always updates; ready moves only for
    /// born tokens, and only a real bubble change marks rosters or ego
    /// bindings dirty — a same-bubble box hop is not a roster change.
    pub(super) fn refresh_scene_location(&mut self, id: i64) {
        let Some(ball) = self.balls.get(&id) else { return };
        let (pos, cloak) = (ball.pos, ball.cloak);
        let global = self.membership.globals.contains(&id);
        let old = self.interest.bubble_of(id);
        self.interest.place(id, pos);
        let new = self.interest.bubble_of(id);
        let Some(token) = self.ball_token(id) else { return };
        if self.membership.pubs.contains_key(&token) {
            let aimed = if cloak != 0 { None }
                else if global { Some(Home::Global) }
                else { new.map(Home::Bubble) };
            self.membership.aim(token, aimed, true);
        }
        if old != new {
            if let Some(clients) = self.membership.egos.get(&id) {
                self.membership.dirty_bindings.extend(clients.iter().copied());
            }
        }
    }
    /// Whether the birth entry already applied for this exact token.
    pub(super) fn scene_ready(&self, token: BallToken) -> bool {
        self.membership.pubs.contains_key(&token)
    }
    /// Unsettled projection work: dirty rosters, moves and pending initials.
    pub(super) fn scene_work_pending(&self) -> bool {
        let m = &self.membership;
        !(m.dirty_bubbles.is_empty() && !m.dirty_global
            && m.dirty_bindings.is_empty() && m.pending_initials.is_empty())
    }
    /// Register a new pending binding's indices and publication need.
    pub(super) fn scene_binding_added(&mut self, observer: ObserverToken, ego: i64) {
        self.membership.pending_initials.insert(observer.client);
        self.membership.binding_ego.insert(observer.client, ego);
        self.membership.egos.entry(ego).or_default().insert(observer.client);
        if let Some(b) = self.bubble_of(ego) { self.membership.dirty_bubbles.insert(b); }
    }
    /// Drop one binding from every index; the last informed binding of a
    /// bubble releases its committed roster once.
    pub(super) fn scene_binding_removed(&mut self, observer: ObserverToken, ego: i64, bubble: Option<BubbleId>) {
        let m = &mut self.membership;
        m.pending_initials.remove(&observer.client);
        m.dirty_bindings.remove(&observer.client);
        m.relocated.remove(&observer.client);
        if m.binding_ego.remove(&observer.client).is_some() {
            if let Some(set) = m.egos.get_mut(&ego) {
                set.remove(&observer.client);
                if set.is_empty() { m.egos.remove(&ego); }
            }
        }
        if let Some(b) = bubble {
            if let Some(set) = m.bubble_bindings.get_mut(&b) {
                set.remove(&observer.client);
                if set.is_empty() {
                    m.bubble_bindings.remove(&b);
                    m.release_bubble(b);
                }
            }
        }
    }
    /// An ego handoff inside one live binding; the informed scene rejoins
    /// through the ordinary dirty evaluation.
    pub(super) fn scene_ego_changed(&mut self, observer: ObserverToken, old_ego: i64, new_ego: i64) {
        let m = &mut self.membership;
        if m.binding_ego.get(&observer.client) == Some(&old_ego) {
            m.binding_ego.insert(observer.client, new_ego);
            if let Some(set) = m.egos.get_mut(&old_ego) {
                set.remove(&observer.client);
                if set.is_empty() { m.egos.remove(&old_ego); }
            }
            m.egos.entry(new_ego).or_default().insert(observer.client);
        }
        m.dirty_bindings.insert(observer.client);
    }
    /// The binding was actually delivered `bubble`/`ego`: write `informed`
    /// and move every index in the same place. Stale tickets are refused.
    pub(super) fn scene_binding_informed(&mut self, observer: ObserverToken, bubble: Option<BubbleId>, ego: Option<BallToken>) {
        let Some(entry) = self.observers.get_mut(&observer.client)
            .filter(|o| o.epoch == observer.epoch) else { return };
        let old = entry.informed.replace(observers::Informed { bubble, ego_token: ego });
        let old_bubble = old.and_then(|i| i.bubble);
        let m = &mut self.membership;
        m.pending_initials.remove(&observer.client);
        if old_bubble == bubble { return; }
        if let Some(old_bubble) = old_bubble {
            if let Some(set) = m.bubble_bindings.get_mut(&old_bubble) {
                set.remove(&observer.client);
                if set.is_empty() {
                    m.bubble_bindings.remove(&old_bubble);
                    m.release_bubble(old_bubble);
                }
            }
        }
        if let Some(new_bubble) = bubble {
            m.bubble_bindings.entry(new_bubble).or_default().insert(observer.client);
        }
    }
    /// Record a native cloak/uncloak result that already went out directly:
    /// hide drops every non-ego layer but keeps the birth-ready identity,
    /// show restores the given layer. A real departure retires through
    /// `scene_token_gone`, never through here. This records what was
    /// published; the reveal audience itself is the native caller's.
    pub(super) fn commit_scene_visibility(&mut self, token: BallToken, home: Option<BubbleId>, global: bool, visible: bool) {
        if !self.membership.pubs.contains_key(&token) { return; }
        let layer = if global { Some(Home::Global) } else { home.map(Home::Bubble) };
        let m = &mut self.membership;
        if visible {
            m.aim(token, layer, false);
            match layer {
                Some(Home::Bubble(b)) => { m.committed.entry(b).or_default().insert(token); }
                Some(Home::Global) => { m.global_committed.insert(token); }
                None => {}
            }
            m.publish(token, layer);
        } else {
            match layer {
                Some(Home::Bubble(b)) => {
                    if let Some(set) = m.committed.get_mut(&b) { set.remove(&token); }
                }
                Some(Home::Global) => { m.global_committed.remove(&token); }
                None => {}
            }
            m.publish(token, None);
            m.aim(token, None, false);
        }
    }
    /// The committed home a token was last delivered through: a bubble
    /// roster, the always-present layer, or nowhere.
    pub(super) fn scene_home(&self, token: BallToken) -> (Option<BubbleId>, bool) {
        match self.membership.pubs.get(&token).and_then(|p| p.published) {
            Some(Home::Bubble(b)) => (Some(b), false),
            Some(Home::Global) => (None, true),
            None => (None, false),
        }
    }
    /// Retire a token whose queued exit already delivered its remove to the
    /// old holders, so the shared delta never removes it a second time. The
    /// birth identity, aimed layer, delivered roster and reverse index all
    /// leave together.
    pub(super) fn scene_token_gone(&mut self, token: BallToken) {
        if let Some(publication) = self.membership.pubs.remove(&token) {
            if let Some(home) = publication.ready { self.membership.remove_ready(token, home); }
            match publication.published {
                Some(Home::Bubble(b)) => {
                    if let Some(set) = self.membership.committed.get_mut(&b) { set.remove(&token); }
                }
                Some(Home::Global) => { self.membership.global_committed.remove(&token); }
                None => {}
            }
        }
        if let Some(clients) = self.membership.egos.get(&token.id()) {
            self.membership.dirty_bindings.extend(clients.iter().copied());
        }
    }
    pub(super) fn scene_ctx(&self) -> SceneCtx<'_> {
        SceneCtx::new(&self.balls, &self.ball_tokens, &self.membership)
    }
    /// `SceneCtx::covers` resolved through a live observer ticket.
    pub(in crate::space) fn scene_ball_covered(&self, observer: ObserverToken, id: i64) -> bool {
        self.observers.get(&observer.client).filter(|o| o.epoch == observer.epoch)
            .is_some_and(|o| self.scene_ctx().covers(o, id))
    }
    /// Whether `observer`'s delivered prefix already lists `ball`: the
    /// committed membership, the always-present layer or its own ego.
    pub(in crate::space) fn scene_has(&self, observer: ObserverToken, ball: BallToken) -> bool {
        self.observers.get(&observer.client).filter(|o| o.epoch == observer.epoch)
            .is_some_and(|o| {
                let Some(informed) = &o.informed else { return false; };
                informed.ego_token == Some(ball) || self.membership.global_committed.contains(&ball)
                    || informed.bubble.is_some_and(|b|
                        self.membership.committed.get(&b).is_some_and(|set| set.contains(&ball)))
            })
    }
    /// Run the captured action prefix, then settle this prefix's publication
    /// and pending initials. The captured count fences the prefix; work
    /// queued meanwhile lands in a later prefix, never drained unbounded.
    pub(in crate::space) fn apply_scene_prefix(&mut self, collisions: bool) {
        let previous = self.membership.active_collisions.replace(collisions);
        let count = self.actions.len();
        let mut last_stamp = None;
        for _ in 0..count {
            let Some((stamp, action)) = self.actions.pop_front() else { break };
            if let Some(last) = last_stamp.filter(|&last| last != stamp) {
                self.settle_scene(last, collisions, false);
            }
            action(self, stamp);
            last_stamp = Some(stamp);
        }
        self.settle_scene(last_stamp.unwrap_or(self.current_time), collisions, true);
        self.membership.active_collisions = previous;
    }

    /// The synchronous in-action barrier: settles only the already-applied
    /// ready work at this action's stamp so the action's own native events
    /// land after its Adds. Pending initials wait for the prefix tail.
    pub(in crate::space) fn publish_ready_prefix(&mut self, stamp: i64) {
        let collisions = self.membership.active_collisions.expect("scene action phase");
        self.settle_scene(stamp, collisions, false);
    }

    pub(super) fn scene_action_collisions(&self) -> bool {
        self.membership.active_collisions.expect("scene action phase")
    }

    /// The real boundary projector at a completed destiny stamp, with the
    /// phase's own collision mode.
    pub(in crate::space) fn publish_scene_boundary(&mut self, stamp: i64, collisions: bool) {
        self.settle_scene(stamp, collisions, true);
    }

}

#[cfg(all(test, feature = "tests-space"))]
mod tests;
