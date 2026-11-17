//! Space simulation tick, serving destiny updates consumed by
//! `eve/client/script/remote/michelle.py`. Celestials come from the SDE;
//! dynamic state is in memory, with positions checkpointed to `game.items`.
//! Observer batches flush before drone announcements and flush acknowledgements.
//! Legacy sphere collisions do not emit bumps.

use super::*;

/// One AddBalls event carrying every listed ball, encoded once for the whole
/// receiving scene: native state, slims and current three-layer damage.
/// `adds` arrives already id-sorted by the shared projector.
pub(super) fn add_balls_batch(stamp: i64, balls: &HashMap<i64, Ball>,
    adds: &[observers::BallToken], collisions: bool) -> Value {
    let mut state = vec![DESTINY_BALLS];
    state.extend_from_slice(&(stamp as i32).to_le_bytes());
    let mut slims = Vec::new();
    let mut damage = Vec::new();
    for &token in adds {
        let Some(ball) = balls.get(&token.id()) else { continue };
        ball.write(&mut state, collisions);
        slims.push(ball.slim_item());
        if ball.free { damage.push((Value::Int(ball.id), ball.damage_state())); }
    }
    event(stamp, "AddBalls", vec![Value::tuple([Value::str(state), Value::List(slims), Value::Dict(damage)])])
}

/// One RemoveBalls event naming every removed ball id; `removes` arrives
/// already id-sorted by the shared projector.
pub(super) fn remove_balls_event(stamp: i64, removes: &[i64]) -> Value {
    event(stamp, "RemoveBalls", vec![Value::list(removes.iter().map(|&id| Value::Int(id)))])
}

impl System {
    pub fn tick(&mut self, ms: i64) {
        self.tick_with_collisions(ms, collision::enabled());
    }
    pub(in crate::space) fn tick_with_collisions(&mut self, ms: i64, collisions: bool) {
        self.attribution.begin_tick();
        let mut mark = tokio::time::Instant::now();
        macro_rules! phase { ($p:ident) => {{
            let ended = tokio::time::Instant::now();
            self.attribution.phase(attribution::Phase::$p, ended.duration_since(mark).as_nanos());
            ended
        }} }
        self.prune_observers();
        self.ms = ms;
        mark = phase!(PruneObservers);
        sovereignty::links::tick(self);
        mark = phase!(Sovereignty);
        if let Err(error) = self.observe_combat_utc() {
            tracing::error!(?error, "combat UTC boundary failed");
        }
        mark = phase!(CombatUtc);
        self.expire_projections();
        mark = phase!(Projections);
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
        self.attribution.steps = steps;
        if !self.is_cold(collisions) { self.begin_destiny_batch(); }
        for step in 0..steps {
            self.warp_corrections.clear();
            self.exited_ids.clear();
            self.apply_scene_prefix(collisions);
            let logical_ms = (self.current_time + 1) * MILLISECONDS_PER_SECOND;
            let anchor_ms = if due > MAX_CATCHUP_STEPS { ms }
                else { next_tick_ms + step * MILLISECONDS_PER_SECOND };
            mark = phase!(Actions);
            let mut evolved = false;
            if self.balls.values().any(|b| b.free && (b.mode != MODE_STOP || b.vel != [0.0; 3]))
                || collision::stationary_contact(&self.balls, collisions) {
                evolved = true;
                self.target_states.clear();
                self.target_states.extend(self.balls.iter().map(|(&id, b)| (id, TargetState {
                    pos: b.pos, vel: b.vel, radius: b.radius,
                })));
                self.free_ids.clear();
                self.free_ids.extend(self.balls.iter().filter_map(|(&id, b)| b.free.then_some(id)));
                self.free_ids.sort_unstable();
                for index in 0..self.free_ids.len() {
                    let id = self.free_ids[index];
                    let aligning = self.balls.get(&id).is_some_and(|b| b.mode == MODE_WARP && !b.is_warping());
                    if aligning { self.prepare_warp_activation(id, self.current_time, logical_ms, collisions); }
                    let b = self.balls.get_mut(&id).unwrap();
                    b.evolve_behavior_for_ball(logical_ms, anchor_ms, &self.target_states, self.current_time);
                    let activated = aligning && b.is_warping();
                    self.target_states.get_mut(&id).unwrap().vel = b.vel;
                    if activated {
                        self.stop_all_followers(id); self.drone_controller_lost(id);
                        self.invalidate_missile_target(id); self.invalidate_missile_launcher(id);
                    }
                }
                mark = phase!(Behavior);
                collision::freeze(&mut self.collision_scratch, &self.balls, collisions);
                collision::apply(&self.collision_scratch, &mut self.balls);
                mark = phase!(Collisions);
                for index in 0..self.collision_scratch.len() {
                    let snapshot = self.collision_scratch[index];
                    if !snapshot.free { continue; }
                    let id = snapshot.id;
                    let b = self.balls.get_mut(&id).unwrap();
                    let was_warping = b.is_warping();
                    b.evolve_position(logical_ms, snapshot.time_factor);
                    let stopped = b.mode == MODE_STOP;
                    let char_id = b.char_id;
                    self.refresh_scene_location(id);
                    if was_warping && stopped {
                        if let Some(char_id) = char_id {
                            crate::progress::publish_live(&self.progress_registry, &crate::lua::events::GameEvent::WarpArrived {
                                char_id, system_id: self.id,
                            });
                        }
                        if !collisions || self.warp_exits.contains_key(&id) {
                            self.exited_ids.push(id);
                        }
                    }
                }
                mark = phase!(Positions);
            } else { mark = phase!(Behavior); }
            self.scan_missile_segment(logical_ms, anchor_ms, evolved);
            self.current_time += 1;
            mark = phase!(Missiles);
            self.broadcast_warp_corrections(self.current_time, collisions);
            self.publish_scene_boundary(self.current_time, collisions);
            for index in 0..self.exited_ids.len() {
                let id = self.exited_ids[index];
                if let Some(update) = collision::reset_event(id, self.current_time, collisions) {
                    self.publish_destiny(id, |_| vec![update]);
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
            mark = phase!(WarpExits);
        }
        crate::services::combat::targeting_charges::locking::drop_invalid(self);

        let pending_combat = self.actor_combat.values().any(|record| record.pending.is_some());
        if self.snapshot_positions.is_empty() && !pending_combat && !self.loaded_outstanding()
            && !self.bounty_checkpoint_pending()
            && !self.combat_facts.as_ref().is_some_and(|facts| facts.is_dirty() || facts.batch_count() != 0) {
            self.snapshot_due = 0;
        } else {
            let interval = crate::config::game().space.position_snapshot_secs.saturating_mul(MILLISECONDS_PER_SECOND);
            if self.snapshot_due == 0 { self.snapshot_due = ms.saturating_add(interval); }
            if ms >= self.snapshot_due {
                self.snapshot_due = ms.saturating_add(interval);
                // Record this period before the gate is tried; busy, in-flight,
                // held and unknown obligations all count it as unfinished.
                self.record_loaded_period();
                snapshot::schedule(self);
                self.wake_pending_combat();
            }
        }
        mark = phase!(Snapshot);
        if self.is_cold(collisions) {
            self.finish_destiny_batch();
            phase!(DeliveryFlush);
            return;
        }
        let filetime = crate::client::filetime_from_unix_ms(ms);
        let expired: Vec<_> = self.balls.values().filter(|b|
            b.cloak != 0 && b.cloak_until > 0 && b.cloak_until <= filetime).map(|b| b.id).collect();
        for id in expired { self.uncloak(id); }
        let obstructed: Vec<_> = self.module_cloaks.keys().copied().filter(|&ship| self.cloak_obstructed(ship)).collect();
        for ship in obstructed { self.uncloak(ship); }
        mark = phase!(Cloak);
        while self.timers.first_key_value().is_some_and(|(&(due, _), _)| due <= ms) {
            let (_, timer) = self.timers.pop_first().unwrap();
            timer.0.run_as(self, attribution::Work::Timer);
        }
        mark = phase!(Timers);
        crate::services::effects::heat::tick(self);
        mark = phase!(Heat);
        self.think_npcs();
        mark = phase!(Npcs);
        self.think_drones();
        mark = phase!(Drones);
        self.admit_effect_jobs();
        mark = phase!(AdmitEffects);
        self.finish_destiny_batch();
        mark = phase!(DeliveryFlush);
        let mut announcements = std::mem::take(&mut self.drone_announcements);
        for id in announcements.iter().copied() { self.announce_drone(id); }
        announcements.clear();
        self.drone_announcements = announcements;
        mark = phase!(DroneAnnouncements);
        let mut acks = std::mem::take(&mut self.destiny_flush_acks);
        for ack in acks.drain(..) { let _ = ack.send(()); }
        self.destiny_flush_acks = acks;
        phase!(FlushAcks);
    }
}
