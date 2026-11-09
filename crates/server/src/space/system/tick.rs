//! Space simulation tick, serving destiny updates consumed by
//! `eve/client/script/remote/michelle.py`. Celestials come from the SDE;
//! dynamic state is in memory, with positions checkpointed to `game.items`.
//! Legacy sphere collisions do not emit bumps.

use super::*;

impl System {
    pub fn tick(&mut self, ms: i64) {
        self.tick_with_collisions(ms, collision::enabled());
    }
    pub(in crate::space) fn tick_with_collisions(&mut self, ms: i64, collisions: bool) {
        self.observers.retain(|_, (_, tx)| !tx.is_closed());
        self.ms = ms;
        sovereignty::links::tick(self);
        if let Err(error) = self.observe_combat_utc() {
            tracing::error!(?error, "combat UTC boundary failed");
        }
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
            self.warp_corrections.clear();
            self.exited_ids.clear();
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
                    let aligning = self.balls.get(&id).is_some_and(|b| b.mode == MODE_WARP && !b.is_warping());
                    if aligning { self.prepare_warp_activation(id, self.current_time, logical_ms); }
                    let b = self.balls.get_mut(&id).unwrap();
                    b.evolve_behavior_for_ball(logical_ms, anchor_ms, &self.target_states, self.current_time);
                    let activated = aligning && b.is_warping();
                    self.target_states.get_mut(&id).unwrap().vel = b.vel;
                    if activated { self.stop_all_followers(id); self.drone_controller_lost(id); }
                }
                collision::freeze(&mut self.collision_scratch, &self.balls, collisions);
                collision::apply(&self.collision_scratch, &mut self.balls);
                for snapshot in self.collision_scratch.iter().filter(|snapshot| snapshot.free) {
                    let id = &snapshot.id;
                    let b = self.balls.get_mut(id).unwrap();
                    let was_warping = b.is_warping();
                    b.evolve_position(logical_ms, snapshot.time_factor);
                    if was_warping && b.mode == MODE_STOP {
                        if let Some(char_id) = b.char_id {
                            crate::progress::publish_live(&self.progress_registry, &crate::lua::events::GameEvent::WarpArrived {
                                char_id, system_id: self.id,
                            });
                        }
                        if !collisions || self.warp_exits.contains_key(id) {
                            self.exited_ids.push(*id);
                        }
                    }
                }
            }
            self.current_time += 1;
            self.broadcast_warp_corrections(self.current_time, collisions);
            for index in 0..self.exited_ids.len() {
                let id = self.exited_ids[index];
                if let Some(update) = collision::reset_event(id, self.current_time, collisions) {
                    self.broadcast(id, vec![update]);
                }
                if let Some(intent) = self.warp_exits.remove(&id) {
                    if let Some(arrival) = intent.arrival {
                        if intent.full && intent.generation == (self.persistent_generation(id), self.combat_membership_generation(id))
                            && self.actor_combat.get(&id).is_none_or(|record| record.phase == combat::CombatPhase::Live) {
                            self.complete_warp_arrival(arrival);
                        } else {
                            self.release_warp_arrival(arrival);
                        }
                    }
                }
            }
        }
        let pending_combat = self.actor_combat.values().any(|record| record.pending.is_some());
        if self.snapshot_positions.is_empty() && !pending_combat
            && !self.combat_facts.as_ref().is_some_and(|facts| facts.is_dirty() || facts.batch_count() != 0) {
            self.snapshot_due = 0;
        } else {
            let interval = crate::config::game().space.position_snapshot_secs.saturating_mul(MILLISECONDS_PER_SECOND);
            if self.snapshot_due == 0 { self.snapshot_due = ms.saturating_add(interval); }
            if ms >= self.snapshot_due {
                self.snapshot_due = ms.saturating_add(interval);
                snapshot::schedule(self);
                self.wake_pending_combat();
            }
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
        self.admit_effect_jobs();
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
}
