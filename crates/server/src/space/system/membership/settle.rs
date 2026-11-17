use super::*;

/// One binding's wire work for a settle: the directed effect stop/replay
/// pairs evaluated from borrowed coverage. A stayer carries only the
/// bubble whose shared delta applies to it; a true transfer additionally
/// owns its full coverage diff.
struct BindingWork {
    token: ObserverToken,
    /// For a stayer: the bubble whose shared delta this binding consumes
    /// (`None` when only the always-present layer changed).
    local: Option<BubbleId>,
    transfer: Option<TransferWork>,
    stops: Vec<Value>,
    replays: Vec<Value>,
}

/// The full old/new coverage diff owned only by a true transfer.
struct TransferWork {
    new_bubble: Option<BubbleId>,
    new_ego: Option<BallToken>,
    lost: Vec<BallToken>,
    gained: Vec<BallToken>,
    /// The transfer's own Add content, with tokens the shared adds already
    /// deliver removed and stale tokens filtered.
    add_emit: Vec<BallToken>,
    /// lost ∪ gained, materialized once for the endpoint-index scan.
    changed: Vec<BallToken>,
}

impl System {
    /// The counted delivery of the ready work accumulated since the last
    /// settle: shared remove-then-add deltas once per dirty observed bubble
    /// and the global layer, then per-binding transfers. Effect stops land
    /// before every remove, adds and replays after every remove, and the
    /// last observer's departure releases a bubble's publication once.
    pub(in crate::space::system) fn settle_scene(&mut self, stamp: i64, collisions: bool, initials: bool) {
        // The relocated mark lives only until the next settle; clearing it
        // before the fast return keeps a stable scene's Retained holders.
        self.membership.relocated.clear();
        if !self.scene_work_pending() { return; }
        // Pending initials need their bubble's roster materialized too.
        let mut pending_bubbles: HashSet<BubbleId> = HashSet::new();
        for client in &self.membership.pending_initials {
            if let Some(b) = self.observers.get(client).and_then(|o| self.bubble_of(o.ego)) {
                pending_bubbles.insert(b);
            }
        }
        self.membership.dirty_bubbles.extend(pending_bubbles.iter().copied());
        // Bindings whose informed scene no longer matches their ego become
        // true transfers; a target that still needs a future exit stays
        // pending for it instead of reading a same-id successor.
        let mut transfers: HashSet<i64> = HashSet::new();
        for client in std::mem::take(&mut self.membership.dirty_bindings) {
            let Some(o) = self.observers.get(&client).filter(|o| o.informed.is_some()) else { continue };
            let informed = o.informed.as_ref().unwrap();
            if informed.bubble != self.bubble_of(o.ego) || informed.ego_token != self.ball_token(o.ego) {
                transfers.insert(client);
            }
        }
        // One shared delta per dirty observed bubble against its committed
        // roster; an unobserved bubble releases its publication once.
        let mut deltas: HashMap<BubbleId, (Vec<BallToken>, Vec<BallToken>)> = HashMap::new();
        let mut released: Vec<BallToken> = Vec::new();
        for b in std::mem::take(&mut self.membership.dirty_bubbles) {
            let observed = self.membership.bubble_bindings.get(&b).is_some_and(|s| !s.is_empty())
                || pending_bubbles.contains(&b);
            if !observed {
                released.extend(self.membership.release_bubble(b));
                continue;
            }
            let (rem, add) = {
                let old = self.membership.committed.get(&b);
                let next = self.membership.ready.get(&b);
                let rem: Vec<BallToken> = old.into_iter().flatten()
                    .filter(|t| !next.is_some_and(|s| s.contains(*t))).copied().collect();
                let mut add: Vec<BallToken> = next.into_iter().flatten()
                    .filter(|t| !old.is_some_and(|s| s.contains(*t))).copied().collect();
                add.sort_unstable_by_key(|t| t.id());
                (rem, add)
            };
            if rem.is_empty() && add.is_empty() { continue; }
            deltas.insert(b, (rem, add));
        }
        // The always-present layer moves as one shared delta.
        let mut global_rem: Vec<BallToken> = Vec::new();
        let mut global_add: Vec<BallToken> = Vec::new();
        if self.membership.dirty_global {
            self.membership.dirty_global = false;
            global_rem = self.membership.global_committed
                .difference(&self.membership.ready_global).copied().collect();
            global_add = self.membership.ready_global
                .difference(&self.membership.global_committed).copied().collect();
            global_add.sort_unstable_by_key(|t| t.id());
        }
        let has_global_delta = !global_rem.is_empty() || !global_add.is_empty();
        // Shared cohort buffers materialize once per dirty bubble; stayers
        // read these slices and never rebuild them. Only true transfers own
        // their own diffs.
        let mut cohort_changed: HashMap<BubbleId, Vec<BallToken>> = HashMap::new();
        let mut cohort_lost: HashMap<BubbleId, Vec<BallToken>> = HashMap::new();
        let mut cohort_lost_ids: HashMap<BubbleId, Vec<i64>> = HashMap::new();
        for (b, (rem, add)) in &deltas {
            cohort_changed.insert(*b, rem.iter().chain(add.iter())
                .chain(global_rem.iter()).chain(global_add.iter()).copied().collect());
            cohort_lost.insert(*b, rem.iter().chain(global_rem.iter()).copied().collect());
            cohort_lost_ids.insert(*b, rem.iter().map(|t| t.id())
                .chain(global_rem.iter().map(|t| t.id())).collect());
        }
        let global_changed: Vec<BallToken> =
            global_rem.iter().chain(global_add.iter()).copied().collect();
        let global_lost: Vec<BallToken> = global_rem.clone();
        let global_lost_ids: Vec<i64> = global_rem.iter().map(|t| t.id()).collect();
        // Stayers on a dirty bubble consume the shared delta; the global
        // delta reaches every informed stayer. Only true transfers read
        // whole old and new lists, through borrowed views.
        let mut works: Vec<BindingWork> = Vec::new();
        let mut work_clients: HashSet<i64> = HashSet::new();
        for b in deltas.keys() {
            for &client in self.membership.bubble_bindings.get(b).into_iter().flatten() {
                if transfers.contains(&client) || !work_clients.insert(client) { continue; }
                let Some(o) = self.observers.get(&client).filter(|o| o.informed.is_some()) else { continue };
                works.push(BindingWork {
                    token: ObserverToken { client, epoch: o.epoch },
                    local: Some(*b), transfer: None,
                    stops: Vec::new(), replays: Vec::new() });
            }
        }
        if has_global_delta {
            for (&client, o) in &self.observers {
                if o.informed.is_none() || transfers.contains(&client)
                    || !work_clients.insert(client) { continue; }
                works.push(BindingWork {
                    token: ObserverToken { client, epoch: o.epoch },
                    local: None, transfer: None,
                    stops: Vec::new(), replays: Vec::new() });
            }
        }
        for client in &transfers {
            let Some(o) = self.observers.get(client).filter(|o| o.informed.is_some()) else { continue };
            let informed = o.informed.as_ref().unwrap();
            let new_bubble = self.bubble_of(o.ego);
            let new_ego = self.ball_token(o.ego);
            // A delivered roster token, a target token that still waits on a
            // future exit, or an ego token whose birth entry has not applied
            // yet keeps the whole move pending for it, rather than removing
            // or materializing around a stale or unborn incarnation.
            let stale = informed.bubble.and_then(|b| self.membership.committed.get(&b))
                .into_iter().flatten().chain(self.membership.global_committed.iter())
                .any(|&t| !self.ball_token_current(t))
                || new_bubble.and_then(|b| self.membership.ready.get(&b))
                .into_iter().flatten().chain(self.membership.ready_global.iter())
                .chain(new_ego.iter())
                .any(|&t| !self.ball_token_current(t))
                || new_ego.is_some_and(|t| !self.scene_ready(t));
            if stale {
                self.membership.dirty_bindings.insert(*client);
                continue;
            }
            let (mut lost, mut gained) = {
                let old_view = SceneView {
                    local: informed.bubble.and_then(|b| self.membership.committed.get(&b)),
                    global: &self.membership.global_committed, ego: informed.ego_token };
                let new_view = SceneView {
                    local: new_bubble.and_then(|b| self.membership.ready.get(&b)),
                    global: &self.membership.ready_global, ego: new_ego };
                let mut lost = Vec::new();
                let mut gained = Vec::new();
                // A superseded ego incarnation was already removed by its own
                // exit; the roster side holds no stale token here.
                for t in informed.bubble.and_then(|b| self.membership.committed.get(&b))
                    .into_iter().flatten()
                    .chain(self.membership.global_committed.iter())
                    .chain(informed.ego_token.iter().filter(|&&t| self.ball_token_current(t))) {
                    if !new_view.contains(*t) { lost.push(*t); }
                }
                for t in new_bubble.and_then(|b| self.membership.ready.get(&b))
                    .into_iter().flatten()
                    .chain(self.membership.ready_global.iter()).chain(new_ego.iter()) {
                    if !old_view.contains(*t) { gained.push(*t); }
                }
                (lost, gained)
            };
            // The chained rosters and ego layers are not sets: the same
            // token can enumerate twice (the new ego inside its ready roster
            // and again as the ego layer). The diff owns exactly-once by
            // full token — same-id incarnations stay distinct.
            lost.sort_unstable_by_key(|t| (t.id(), t.serial()));
            lost.dedup();
            gained.sort_unstable_by_key(|t| (t.id(), t.serial()));
            gained.dedup();
            // The shared adds of the new bubble and the global layer already
            // reach this binding once it is re-informed; they must not land
            // twice inside its own transfer Add.
            let shared_add: &[BallToken] = new_bubble.and_then(|b| deltas.get(&b))
                .map(|(_, add)| add.as_slice()).unwrap_or(&[]);
            let mut add_emit: Vec<BallToken> = gained.iter().copied()
                .filter(|t| !shared_add.contains(t) && !global_add.contains(t)
                    && self.ball_token_current(*t)).collect();
            add_emit.sort_unstable_by_key(|t| t.id());
            let mut changed = lost.clone();
            changed.extend(gained.iter().copied());
            works.push(BindingWork {
                token: ObserverToken { client: *client, epoch: o.epoch },
                local: None,
                transfer: Some(TransferWork { new_bubble, new_ego, lost, gained, add_emit, changed }),
                stops: Vec::new(), replays: Vec::new() });
        }
        // Borrowed old/new coverage evaluates each changed binding's
        // recorded effects once; the receiver is `Presentation` so the views
        // can borrow the membership sets.
        let now = self.ms;
        let ball_tokens = &self.ball_tokens;
        let balls = &self.balls;
        let current = move |t: BallToken|
            ball_tokens.get(&t.id()) == Some(&t.serial()) && balls.contains_key(&t.id());
        for w in &mut works {
            let Some(o) = self.observers.get(&w.token.client).filter(|o| o.epoch == w.token.epoch) else { continue };
            let Some(informed) = &o.informed else { continue };
            let changed: &[BallToken] = if let Some(t) = &w.transfer {
                &t.changed
            } else if let Some(b) = w.local {
                cohort_changed.get(&b).map(|v| v.as_slice()).unwrap_or(&[])
            } else {
                &global_changed
            };
            let new_bubble = w.transfer.as_ref().map_or(informed.bubble, |t| t.new_bubble);
            let new_ego = w.transfer.as_ref().map_or(informed.ego_token, |t| t.new_ego);
            let old_view = SceneView {
                local: informed.bubble.and_then(|b| self.membership.committed.get(&b)),
                global: &self.membership.global_committed, ego: informed.ego_token };
            let new_view = SceneView {
                local: new_bubble.and_then(|b| self.membership.ready.get(&b)),
                global: &self.membership.ready_global, ego: new_ego };
            let (stops, replays) = self.presentation
                .scene_fx_changes(old_view, new_view, changed, stamp, now, current);
            w.stops = stops;
            w.replays = replays;
        }
        // Re-inform every true transfer before any wire: the mover leaves
        // its old shared cohort (so the cohort remove never carries its
        // retained ego) and joins the new one in time for its shared adds.
        for w in &mut works {
            let Some(t) = &w.transfer else { continue };
            self.scene_binding_informed(w.token, t.new_bubble, t.new_ego);
            self.membership.relocated.insert(w.token.client);
        }
        // Directed effect stops land before every remove.
        for w in &mut works {
            if !w.stops.is_empty() {
                self.emit_destiny(DestinyAudience::Observer(w.token), std::mem::take(&mut w.stops));
            }
        }
        // Shared removes, then each transfer's own remove; every relevant
        // remove lands before any of the binding's adds.
        for (b, (rem, _)) in &deltas {
            if rem.is_empty() || !self.membership.bubble_bindings.get(b).is_some_and(|s| !s.is_empty()) { continue; }
            let mut ids: Vec<i64> = rem.iter().map(|t| t.id()).collect();
            ids.sort_unstable();
            self.emit_destiny(DestinyAudience::Scene { bubble: *b, except_ego: None },
                vec![remove_balls_event(stamp, &ids)]);
        }
        if !global_rem.is_empty() {
            let mut ids: Vec<i64> = global_rem.iter().map(|t| t.id()).collect();
            ids.sort_unstable();
            self.emit_destiny(DestinyAudience::System, vec![remove_balls_event(stamp, &ids)]);
        }
        for w in &works {
            let Some(t) = &w.transfer else { continue };
            if t.lost.is_empty() { continue; }
            let mut ids: Vec<i64> = t.lost.iter().map(|t| t.id()).collect();
            ids.sort_unstable();
            self.emit_destiny(DestinyAudience::Observer(w.token), vec![remove_balls_event(stamp, &ids)]);
        }
        // Shared adds, then the committed rosters take the aimed content in
        // place so the old buffers keep their capacity.
        for (b, (_, add)) in &deltas {
            let add: Vec<BallToken> = add.iter().copied()
                .filter(|&t| self.ball_token_current(t)).collect();
            if add.is_empty() || !self.membership.bubble_bindings.get(b).is_some_and(|s| !s.is_empty()) { continue; }
            self.emit_destiny(DestinyAudience::Scene { bubble: *b, except_ego: None },
                vec![add_balls_batch(stamp, &self.balls, &add, collisions)]);
        }
        // Committed rosters take the aimed content in place so the old
        // buffers keep their capacity; the delivered-home reverse index is
        // reconciled from the final rosters once, after every roster write.
        let mut reconcile: HashSet<BallToken> = released.into_iter().collect();
        for (b, (rem, add)) in &deltas {
            let committed = &mut self.membership.committed;
            let ready = &self.membership.ready;
            let entry = committed.entry(*b).or_default();
            entry.clear();
            entry.extend(ready.get(b).into_iter().flatten().copied());
            reconcile.extend(rem.iter().copied());
            reconcile.extend(add.iter().copied());
        }
        if has_global_delta {
            let add: Vec<BallToken> = global_add.iter().copied()
                .filter(|&t| self.ball_token_current(t)).collect();
            if !add.is_empty() {
                self.emit_destiny(DestinyAudience::System,
                    vec![add_balls_batch(stamp, &self.balls, &add, collisions)]);
            }
            self.membership.global_committed.clear();
            let ready_global = &self.membership.ready_global;
            self.membership.global_committed.extend(ready_global.iter().copied());
            reconcile.extend(global_rem.iter().copied());
            reconcile.extend(global_add.iter().copied());
        }
        // Each transfer's own Add carries what the shared adds did not; a
        // destination bubble no shared commit covers is materialized from
        // its aimed roster once, and its tokens join the reconcile set.
        for w in &works {
            let Some(t) = &w.transfer else { continue };
            if !t.add_emit.is_empty() {
                self.emit_destiny(DestinyAudience::Observer(w.token),
                    vec![add_balls_batch(stamp, &self.balls, &t.add_emit, collisions)]);
            }
            let Some(b) = t.new_bubble else { continue };
            if self.membership.committed.contains_key(&b) { continue; }
            let tokens: Vec<BallToken> = self.membership.ready.get(&b)
                .into_iter().flatten().copied().collect();
            self.membership.committed.insert(b, tokens.iter().copied().collect());
            reconcile.extend(tokens);
        }
        for t in reconcile { self.membership.resolve_published(t); }
        // Replays, fleet snapshots and SKIN entries follow the adds. Cleared
        // ids and lost tokens come from the cohort slice or the transfer's
        // own diff.
        for w in &mut works {
            if !w.replays.is_empty() {
                self.emit_destiny(DestinyAudience::Observer(w.token), std::mem::take(&mut w.replays));
            }
            let transfer_lost_ids: Vec<i64>;
            let lost_ids: &[i64] = if let Some(t) = &w.transfer {
                transfer_lost_ids = t.lost.iter().map(|x| x.id()).collect();
                &transfer_lost_ids
            } else if let Some(b) = w.local {
                cohort_lost_ids.get(&b).map(|v| v.as_slice()).unwrap_or(&[])
            } else {
                &global_lost_ids
            };
            if let Some(character) = self.observer_character(w.token) {
                let snapshot = crate::services::fleets::space_state::observer_snapshot(self, self.id, character, lost_ids);
                self.emit_scene_notify(w.token, "OnFleetStateChange", vec![snapshot]);
            }
            let lost: &[BallToken] = if let Some(t) = &w.transfer {
                &t.lost
            } else if let Some(b) = w.local {
                cohort_lost.get(&b).map(|v| v.as_slice()).unwrap_or(&[])
            } else {
                &global_lost
            };
            self.gm_effect_scene_lost(w.token, lost);
            if let Some(t) = &w.transfer {
                for token in &t.gained { self.present_skins(w.token, Some(*token)); }
            } else {
                let local_add: &[BallToken] = w.local.and_then(|b| deltas.get(&b))
                    .map(|(_, a)| a.as_slice()).unwrap_or(&[]);
                for token in local_add.iter().chain(global_add.iter()) {
                    self.present_skins(w.token, Some(*token));
                }
            }
        }
        // Pending initials materialize only on the fresh committed state and
        // only at a captured-prefix tail or a real boundary, so a SetState
        // never replays an action the shared deltas already delivered.
        if initials {
            let pending: Vec<i64> = self.membership.pending_initials.iter().copied().collect();
            for client in pending {
                let Some(token) = self.observers.get(&client)
                    .map(|o| ObserverToken { client, epoch: o.epoch }) else {
                    self.membership.pending_initials.remove(&client);
                    continue;
                };
                self.present_initial(token, collisions);
            }
        }
    }
}
