//! Runs drone control, orders, movement and action cycles in the system actor.
//! Client commands are in eve/client/script/ui/services/menuSvcExtras/droneFunctions.py.
//! Uses actor state, drone specs and shared Sde; no tables directly.
//! Orders and controller state are not restored after restart.

use super::*;
use crate::consts::{*, dogma::*};
use crate::space::targeting::next_generation as generation;

/// Server movement policy: converge inside the action/contact boundary, not at it.
const DRONE_APPROACH_RANGE_FACTOR: f64 = 0.5;

pub(in crate::space) struct Reservation { owner: i64, controller: i64, bandwidths: Vec<f64> }
pub(in crate::space) struct DroneBrain {
    record: DroneRecord,
    effects: DroneEffects,
    lock_at: i64,
    locked: bool,
    cycle_at: i64,
    end_at: Option<i64>,
    contact_queued: bool,
    contact_claimed: Option<u64>,
    kinematics: Option<crate::services::effects::propulsion::Kinematics>,
    announced: bool,
}

impl System {
    pub(crate) fn drone(&self, id: i64) -> Option<DroneRecord> { self.drones.get(&id).map(|b| b.record.clone()) }
    pub(crate) fn combat_owner(&self, id: i64) -> Option<i64> {
        let ball = self.ball(id)?;
        ball.char_id.or_else(|| self.drones.get(&id).and_then(|b| (b.record.owner != OWNER_UNOWNED).then_some(b.record.owner)))
    }
    pub(crate) fn drones(&self) -> impl Iterator<Item = &DroneRecord> { self.drones.values().map(|b| &b.record) }
    fn drone_ship_live(&self, owner: i64, ship: i64) -> bool {
        self.ball(ship).is_some_and(|b| b.char_id == Some(owner) && b.cloak == 0 && b.mode != MODE_WARP)
    }
    pub(crate) fn reserve_drone_launch(&mut self, owner: i64, controller: i64, bandwidths: &[f64], max_active: f64, total_bandwidth: f64) -> Option<u64> {
        if !self.drone_ship_live(owner, controller) || !max_active.is_finite() || max_active < 0.0 || !total_bandwidth.is_finite() || total_bandwidth < 0.0
            || bandwidths.is_empty() || bandwidths.iter().any(|v| !v.is_finite() || *v < 0.0) { return None; }
        let controlled = self.drones().filter(|d| d.controller == Some(controller));
        let (mut count, mut used) = controlled.fold((0usize, 0.0), |(n, sum), d| (n + 1, sum + d.spec.bandwidth));
        for r in self.drone_reservations.values().filter(|r| r.controller == controller) {
            count += r.bandwidths.len();
            used += r.bandwidths.iter().sum::<f64>();
        }
        used += bandwidths.iter().sum::<f64>();
        if (count + bandwidths.len()) as f64 > max_active || !used.is_finite() || used > total_bandwidth { return None; }
        let token = generation();
        self.drone_reservations.insert(token, Reservation { owner, controller, bandwidths: bandwidths.to_vec() });
        Some(token)
    }
    pub(crate) fn cancel_drone_launch(&mut self, token: u64) { self.drone_reservations.remove(&token); }
    pub(crate) fn complete_drone_launch(&mut self, token: u64, owner: i64, controller: i64, entries: Vec<(Ball, DroneSpec)>, effects: DroneEffects) -> tokio::sync::oneshot::Receiver<()> {
        let valid = self.drone_reservations.remove(&token).is_some_and(|r| r.owner == owner && r.controller == controller)
            && self.drone_ship_live(owner, controller);
        for (ball, spec) in entries {
            if self.drones.contains_key(&ball.id) { continue; }
            let id = ball.id;
            self.add_ship(ball);
            self.register_drone(id, spec, effects.clone(), valid.then_some(controller), valid.then_some(owner), false);
            self.queue_drone_announcement(id);
        }
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.queue_drone_launch_ack(tx);
        rx
    }
    pub(crate) fn restore_drone(&mut self, ball: Ball, spec: DroneSpec, effects: DroneEffects) {
        let id = ball.id;
        if self.drones.contains_key(&id) { return; }
        self.insert(ball);
        self.register_drone(id, spec, effects, None, None, true);
    }
    fn register_drone(&mut self, id: i64, spec: DroneSpec, effects: DroneEffects, controller: Option<i64>, controller_owner: Option<i64>, announced: bool) {
        let Some((owner, pos)) = self.ball(id).map(|b| (b.owner_id, b.pos)) else { return };
        let record = DroneRecord { id, owner, controller, controller_owner, generation: generation(), order: DroneOrder::Idle, target: None, state: DRONE_STATE_IDLE, spec, pending: None };
        self.snapshot_generation = self.snapshot_generation.checked_add(1).expect("snapshot generation exhausted");
        self.snapshot_positions.insert(id, (self.snapshot_generation, pos));
        if self.snapshot_due == 0 { self.snapshot_due = self.ms.saturating_add(crate::config::game().space.position_snapshot_secs.saturating_mul(MILLISECONDS_PER_SECOND)); }
        self.drones.insert(id, DroneBrain { record, effects, lock_at: 0, locked: false, cycle_at: 0, end_at: None, contact_queued: false, contact_claimed: None, kinematics: None, announced });
    }
    fn notify_drone(&self, d: &DroneRecord) {
        if self.drones.get(&d.id).is_none_or(|b| !b.announced) { return; }
        self.notify_pilot(d.owner, "OnDroneStateChange", crate::services::effects::notify::drone_state(d.id,
            (d.owner != OWNER_UNOWNED).then_some(d.owner), d.controller, d.state, d.spec.type_id, d.controller_owner, d.target));
    }
    pub(in crate::space) fn announce_drone(&mut self, id: i64) {
        let Some(brain) = self.drones.get_mut(&id) else { return };
        brain.announced = true;
        let record = brain.record.clone();
        self.notify_drone(&record);
    }
    pub(in crate::space) fn drone_state_rows(&self, ego: i64) -> Vec<Value> {
        let owner = self.ball(ego).and_then(|b| b.char_id);
        self.drones().filter(|d| d.owner != OWNER_UNOWNED && (owner == Some(d.owner) || d.controller == Some(ego))).map(|d| crate::services::keyval([
            ("droneID", Value::Int(d.id)), ("ownerID", Value::Int(d.owner)),
            ("controllerID", d.controller.map_or(Value::None, Value::Int)), ("activityState", Value::Int(i64::from(d.state))),
            ("typeID", Value::Int(i64::from(d.spec.type_id))), ("controllerOwnerID", d.controller_owner.map_or(Value::None, Value::Int)),
            ("targetID", d.target.map_or(Value::None, Value::Int)),
        ])).collect()
    }
    pub(crate) fn drone_current(&self, id: i64, generation: u64) -> bool {
        self.drones.get(&id).is_some_and(|b| b.record.generation == generation) && self.contains(id)
    }
    pub(crate) fn drone_valid(&self, id: i64, target: i64, generation: u64) -> bool {
        let Some(b) = self.drones.get(&id) else { return false };
        let d = &b.record;
        let Some(controller) = d.controller else { return false };
        if !self.drone_current(id, generation) || !b.locked || d.pending.is_some() || d.target != Some(target) || !self.drone_ship_live(d.owner, controller)
            || crate::services::combat::jamming::is_jammed(id, self.ms) || crate::services::combat::jamming::is_jammed(controller, self.ms) { return false; }
        self.ball(id).zip(self.ball(controller)).zip(self.ball(target)).is_some_and(|((drone, ship), victim)|
            targeting::target_visible(victim)
            && dist(ship.position_at(self.ms), victim.position_at(self.ms)) <= d.spec.control_range
            && (d.spec.max_target_range == 0.0 || dist(drone.position_at(self.ms), victim.position_at(self.ms)) <= d.spec.max_target_range))
    }
    pub(crate) fn drone_order(&mut self, id: i64, owner: i64, controller: i64, order: DroneOrder) -> bool {
        let Some(d) = self.drone(id) else { return false };
        if self.drones[&id].contact_claimed.is_some() || d.owner != owner || !self.drone_ship_live(owner, controller) || (d.controller != Some(controller) && !(order == DroneOrder::ReturnBay && d.controller.is_none())) { return false; }
        if let DroneOrder::Engage(target) | DroneOrder::Mine(target) = order {
            if crate::services::combat::jamming::is_jammed(id, self.ms) || crate::services::combat::jamming::is_jammed(controller, self.ms) { return false; }
            let Some((ship, victim)) = self.ball(controller).zip(self.ball(target)) else { return false };
            if !targeting::target_visible(victim) || !d.spec.control_range.is_finite()
                || dist(ship.position_at(self.ms), victim.position_at(self.ms)) > d.spec.control_range { return false; }
        }
        self.remove_projections_from(id);
        let b = self.drones.get_mut(&id).unwrap();
        let old_order = b.record.order;
        b.record.generation = generation();
        b.record.order = order;
        b.record.target = match order { DroneOrder::Engage(t) | DroneOrder::Mine(t) => Some(t), DroneOrder::ReturnBay => Some(controller), _ => None };
        if order != DroneOrder::ReturnBay { b.record.pending = None; }
        b.record.state = match order { DroneOrder::ReturnBay | DroneOrder::ReturnHome => DRONE_STATE_DEPARTING, DroneOrder::Engage(_) | DroneOrder::Mine(_) => DRONE_STATE_APPROACHING, _ => DRONE_STATE_IDLE };
        b.locked = false; b.lock_at = 0; b.cycle_at = 0; b.end_at = None; b.contact_queued = false;
        let record = b.record.clone();
        if matches!(old_order, DroneOrder::Assist(_) | DroneOrder::Guard(_)) {
            self.notify_pilot(owner, "OnDroneActivityChange", crate::services::effects::notify::drone_activity(id, None, None));
        }
        if let DroneOrder::Assist(delegate) | DroneOrder::Guard(delegate) = order {
            self.notify_pilot(owner, "OnDroneActivityChange", crate::services::effects::notify::drone_activity(id, Some(delegate), Some(if matches!(order, DroneOrder::Assist(_)) { "assist" } else { "guard" })));
        }
        self.notify_drone(&record);
        match order {
            DroneOrder::ReturnBay | DroneOrder::ReturnHome => self.drone_follow(id, controller, DRONE_SCOOP_RANGE),
            DroneOrder::Engage(target) | DroneOrder::Mine(target) => self.drone_follow(id, target, record.spec.optimal_range.max(0.0)),
            _ => self.drone_stop(id),
        }
        true
    }
    fn drone_follow(&mut self, id: i64, target: i64, range: f64) {
        let Some(generation) = self.drones.get(&id).map(|b| b.record.generation) else { return };
        // Follow aims at its requested separation and can settle just outside it;
        // aim inside the action/contact boundary so the drone can actually enter it.
        let range = if range.is_finite() { (range * DRONE_APPROACH_RANGE_FACTOR).clamp(0.0, f32::MAX as f64) } else { 0.0 };
        self.command(id, move |ball, stamp, s| {
            if s.drones.get(&id).is_none_or(|b| b.record.generation != generation) { return Vec::new(); }
            let Some(target_ball) = s.ball(target) else { return Vec::new() };
            if !ball.follow_ball(target_ball, range as f32) { return Vec::new(); }
            ball.set_speed_fraction(1.0);
            vec![event(stamp, "FollowBall", vec![Value::Int(id), Value::Int(target), Value::Float(range)]), event(stamp, "SetSpeedFraction", vec![Value::Int(id), Value::Float(1.0)])]
        });
    }
    fn drone_stop(&mut self, id: i64) {
        let Some(generation) = self.drones.get(&id).map(|b| b.record.generation) else { return };
        self.command(id, move |b, stamp, s| {
            if s.drones.get(&id).is_none_or(|brain| brain.record.generation != generation) { return Vec::new(); }
            b.stop(); vec![event(stamp, "Stop", vec![Value::Int(id)])]
        });
    }
    pub(crate) fn drone_refresh(&mut self, id: i64, generation: u64, spec: DroneSpec, kinematics: crate::services::effects::propulsion::Kinematics, owner_projection: Option<(i64, u64)>) -> bool {
        if !self.drone_current(id, generation) { return false; }
        let brain = self.drones.get_mut(&id).unwrap();
        let changed = brain.kinematics != Some(kinematics);
        brain.kinematics = Some(kinematics);
        brain.record.spec = spec;
        if changed { self.queue_drone_kinematics(id, generation, self.projection_revision(id), owner_projection, kinematics); }
        true
    }
    pub(in crate::space) fn invalidate_drone_kinematics(&mut self, id: i64, generation: u64) {
        if let Some(brain) = self.drones.get_mut(&id).filter(|b| b.record.generation == generation) {
            brain.kinematics = None;
        }
    }
    pub(crate) fn drone_idle(&mut self, id: i64, generation: u64) {
        if !self.drone_current(id, generation) { return; }
        self.remove_projections_from(id);
        let b = self.drones.get_mut(&id).unwrap();
        b.record.generation = self::generation(); b.record.order = DroneOrder::Idle; b.record.target = None; b.record.state = DRONE_STATE_IDLE; b.record.pending = None;
        b.locked = false; b.end_at = None; b.contact_queued = false;
        let d = b.record.clone(); self.notify_drone(&d); self.drone_stop(id);
    }
    pub(crate) fn drone_pending(&mut self, id: i64, generation: u64, pending: PendingMining) -> bool {
        if !self.drone_valid(id, pending.rock, generation)
            || !matches!(self.drones[&id].record.order, DroneOrder::Mine(target) if target == pending.rock) { return false; }
        let Some(controller) = self.drones[&id].record.controller else { return false };
        let b = self.drones.get_mut(&id).unwrap(); b.record.pending = Some(pending); b.contact_queued = false; b.end_at = None; b.locked = false; b.lock_at = 0;
        self.drone_follow(id, controller, DRONE_SCOOP_RANGE); true
    }
    pub(crate) fn drone_claim_contact(&mut self, id: i64, generation: u64) -> Option<DroneRecord> {
        let d = self.drone(id)?;
        let b = self.drones.get(&id)?;
        if !self.drone_current(id, generation) || b.contact_claimed.is_some() || !b.contact_queued
            || (d.pending.is_none() && d.order != DroneOrder::ReturnBay) { return None; }
        let destination = if d.order == DroneOrder::ReturnBay { d.target? } else { d.controller? };
        if !self.drone_ship_live(d.owner, destination) { return None; }
        let (source, ship) = self.sampled_ball(id).zip(self.sampled_ball(destination))?;
        if dist(source.pos, ship.pos) - f64::from(source.radius) - f64::from(ship.radius) > DRONE_SCOOP_RANGE { return None; }
        self.drones.get_mut(&id)?.contact_claimed = Some(generation);
        Some(d)
    }
    pub(crate) fn drone_release_contact(&mut self, id: i64, generation: u64) {
        if let Some(b) = self.drones.get_mut(&id).filter(|b| b.contact_claimed == Some(generation)) {
            b.contact_claimed = None;
            if b.record.controller.is_none() {
                b.record.pending = None;
            }
        }
    }
    pub(crate) fn drone_delivered(&mut self, id: i64, generation: u64, repeat: bool) {
        let current = self.drone_current(id, generation);
        let Some(b) = self.drones.get_mut(&id) else { return };
        if !current && b.contact_claimed != Some(generation) { return; }
        if b.record.pending.take().is_none() { return; }
        b.contact_queued = false;
        if !current { return; }
        let d = b.record.clone();
        if repeat && matches!(d.order, DroneOrder::Mine(_)) {
            if let DroneOrder::Mine(target) = d.order { self.drone_follow(id, target, d.spec.optimal_range.max(0.0)); }
        } else if d.order != DroneOrder::ReturnBay { self.drone_idle(id, generation); }
    }
    pub(crate) fn drone_abandoned(&mut self, id: i64) {
        let Some(d) = self.drone(id) else { return };
        self.remove_projections_from(id);
        crate::services::combat::jamming::clear(self, id);
        self.set_ball_owner(id, OWNER_UNOWNED);
        let b = self.drones.get_mut(&id).unwrap();
        b.record.owner = OWNER_UNOWNED; b.record.controller = None; b.record.controller_owner = None; b.record.generation = generation(); b.record.order = DroneOrder::Idle; b.record.target = None; b.record.pending = None; b.record.state = DRONE_STATE_IDLE; b.locked = false;
        self.notify_pilot(d.owner, "OnDroneStateChange", crate::services::effects::notify::drone_state(id, None, None, DRONE_STATE_IDLE, d.spec.type_id, None, None));
        self.notify_pilot(d.owner, "OnDroneActivityChange", crate::services::effects::notify::drone_activity(id, None, None));
        self.drone_stop(id);
    }
    pub(crate) fn drone_controller_lost(&mut self, controller: i64) {
        self.clear_drone_aggressors(controller);
        let ids: Vec<_> = self.drones().filter(|d| d.controller == Some(controller) || (d.order == DroneOrder::ReturnBay && d.target == Some(controller))).map(|d| d.id).collect();
        for id in ids {
            self.remove_projections_from(id);
            let was_controlled = self.drones[&id].record.controller == Some(controller) && self.drones[&id].announced;
            let b = self.drones.get_mut(&id).unwrap();
            b.record.controller = None; b.record.controller_owner = None; b.record.generation = generation(); b.record.order = DroneOrder::Idle; b.record.target = None; b.record.state = DRONE_STATE_IDLE; b.locked = false; b.end_at = None; b.contact_queued = false;
            if b.contact_claimed.is_none() {
                b.record.pending = None;
            }
            let d = b.record.clone(); self.notify_drone(&d);
            if was_controlled { self.notify_pilot(d.owner, "OnDroneControlLost", crate::services::effects::notify::drone_control_lost(id)); }
            self.notify_pilot(d.owner, "OnDroneActivityChange", crate::services::effects::notify::drone_activity(id, None, None));
            self.drone_stop(id);
        }
    }
    pub(crate) fn drone_jammed(&mut self, source: i64) {
        let ids: Vec<_> = self.drones().filter(|d| d.id == source || d.controller == Some(source)).map(|d| d.id).collect();
        for id in ids {
            self.remove_projections_from(id);
            let b = self.drones.get_mut(&id).unwrap();
            if b.record.pending.is_none() && b.record.target.is_some() && (b.locked || b.lock_at != 0) {
                b.record.generation = generation();
                b.locked = false;
                b.lock_at = 0;
                b.end_at = None;
            }
        }
    }
    pub(in crate::space) fn drone_removed(&mut self, id: i64) {
        self.remove_drone_aggressor(id);
        if let Some(b) = self.drones.remove(&id) {
            let d = b.record;
            if d.owner != OWNER_UNOWNED {
                self.notify_pilot(d.owner, "OnDroneStateChange", crate::services::effects::notify::drone_state(id, None, None, DRONE_STATE_IDLE, d.spec.type_id, None, None));
            }
        }
    }
    pub(crate) fn drone_attack(&mut self, attacker: i64, target: i64) {
        self.record_drone_aggressor(attacker, target);
        let delegated: Vec<_> = self.drones().filter_map(|d| {
            let delegate = match d.order { DroneOrder::Assist(c) if self.pilot_ball(c).is_some_and(|b| b.id == attacker) => Some((c, target)), DroneOrder::Guard(c) if self.pilot_ball(c).is_some_and(|b| b.id == target) => Some((c, attacker)), _ => None }?;
            Some((d.id, delegate.1))
        }).collect();
        for (id, victim) in delegated {
            let Some(d) = self.drone(id) else { continue };
            let Some(ship) = d.controller else { continue };
            if !self.drone_ship_live(d.owner, ship) { continue; }
            let Some((owner_ball, target_ball)) = self.ball(ship).zip(self.ball(victim)) else { continue };
            if !targeting::target_visible(target_ball) || dist(owner_ball.position_at(self.ms), target_ball.position_at(self.ms)) > d.spec.control_range { continue; }
            self.remove_projections_from(id);
            let b = self.drones.get_mut(&id).unwrap(); b.record.generation = generation(); b.record.target = Some(victim); b.record.state = DRONE_STATE_APPROACHING; b.locked = false; b.lock_at = 0; b.end_at = None;
            let record = b.record.clone(); self.notify_drone(&record); self.drone_follow(id, victim, record.spec.optimal_range.max(0.0));
        }
    }
    pub(in crate::space) fn think_drones(&mut self) {
        self.think_automatic_drones();
        let mut ids = std::mem::take(&mut self.due_drones);
        ids.extend(self.drones.keys().copied());
        for &id in &ids { self.think_drone(id); }
        ids.clear();
        self.due_drones = ids;
    }
    fn think_drone(&mut self, id: i64) {
        let Some(d) = self.drone(id) else { return };
        let Some(controller) = d.controller.or_else(|| (d.order == DroneOrder::ReturnBay).then_some(d.target).flatten()) else { return };
        if self.ball(controller).is_none_or(|b| b.char_id != Some(d.owner) || b.mode == MODE_WARP) {
            self.drone_controller_lost(controller); return;
        }
        if !self.drone_ship_live(d.owner, controller) { self.drone_jammed(controller); return; }
        if d.pending.is_some() || matches!(d.order, DroneOrder::ReturnBay | DroneOrder::ReturnHome) {
            let Some((me, ship)) = self.sampled_ball(id).zip(self.sampled_ball(controller)) else { return };
            if dist(me.pos, ship.pos) - f64::from(me.radius) - f64::from(ship.radius) > DRONE_SCOOP_RANGE { return; }
            if d.order == DroneOrder::ReturnHome && d.pending.is_none() {
                self.drone_idle(id, d.generation);
                let orbit = d.spec.orbit_range.max(0.0);
                let Some(generation) = self.drones.get(&id).map(|b| b.record.generation) else { return };
                self.command(id, move |ball, stamp, s| {
                    if s.drones.get(&id).is_none_or(|b| b.record.generation != generation) { return Vec::new(); }
                    let Some(ship) = s.ball(controller) else { return Vec::new() };
                    if !ball.orbit(ship, orbit as f32) { return Vec::new(); }
                    ball.set_speed_fraction(1.0);
                    vec![event(stamp, "Orbit", vec![Value::Int(id), Value::Int(controller), Value::Float(orbit)]),
                        event(stamp, "SetSpeedFraction", vec![Value::Int(id), Value::Float(1.0)])]
                });
                return;
            }
            let brain = self.drones.get_mut(&id).unwrap();
            if !brain.contact_queued {
                brain.contact_queued = true;
                if let Some(tx) = &self.effect_jobs { let _ = tx.send(npc::EffectJob::Drone { effects: brain.effects.clone(), action: DroneAction::Contact { drone: d, return_to_bay: brain.record.order == DroneOrder::ReturnBay } }); }
            }
            return;
        }
        let Some(target) = d.target else { return };
        let jammed = crate::services::combat::jamming::is_jammed(id, self.ms) || crate::services::combat::jamming::is_jammed(controller, self.ms);
        let Some((me, victim, ship)) = self.sampled_ball(id).zip(self.sampled_ball(target)).zip(self.sampled_ball(controller)).map(|((a,b),c)| (a,b,c)) else { self.drone_idle(id, d.generation); return };
        if jammed || !targeting::target_visible(&victim) || dist(ship.pos, victim.pos) > d.spec.control_range || (d.spec.max_target_range != 0.0 && dist(me.pos, victim.pos) > d.spec.max_target_range) {
            self.remove_projections_from(id);
            let b = self.drones.get_mut(&id).unwrap();
            if b.locked || b.lock_at != 0 { b.record.generation = generation(); }
            b.locked = false; b.lock_at = 0; b.end_at = None; return;
        }
        if dist(me.pos, victim.pos) - f64::from(me.radius) - f64::from(victim.radius) > d.spec.optimal_range.max(0.0) { return; }
        let signature = victim.signature_radius.or_else(|| self.projection_sde.as_ref()
            .and_then(|sde| sde.types.get(victim.type_id))
            .and_then(|data| data.attribute(ATTR_SIGNATURE_RADIUS))).unwrap_or(f64::from(victim.radius));
        let b = self.drones.get_mut(&id).unwrap();
        if !b.locked {
            if b.lock_at == 0 { b.lock_at = self.ms.saturating_add(if d.spec.scan_resolution == 0.0 { 0 } else { lock_time_ms(d.spec.scan_resolution, signature) as i64 }); }
            if self.ms < b.lock_at { return; }
            b.locked = true; b.cycle_at = self.ms;
        }
        if !d.spec.duration_ms.is_finite() || d.spec.duration_ms < 1.0 { return; }
        if let Some(end) = b.end_at {
            if self.ms >= end {
                b.end_at = None;
                if let Some(tx) = &self.effect_jobs { let _ = tx.send(npc::EffectJob::Drone { effects: b.effects.clone(), action: DroneAction::Cycle { drone: d.clone(), target, phase: DronePhase::End } }); }
            }
        }
        let mut notice = None;
        if self.ms >= b.cycle_at {
            let state = if matches!(d.order, DroneOrder::Mine(_)) { DRONE_STATE_MINING } else { DRONE_STATE_COMBAT };
            if b.record.state != state { b.record.state = state; notice = Some(b.record.clone()); }
            let deadline = self.ms.saturating_add(d.spec.duration_ms.min(i64::MAX as f64) as i64);
            b.cycle_at = deadline; b.end_at = Some(deadline);
            if let Some(tx) = &self.effect_jobs { let _ = tx.send(npc::EffectJob::Drone { effects: b.effects.clone(), action: DroneAction::Cycle { drone: b.record.clone(), target, phase: DronePhase::Begin } }); }
        }
        if let Some(record) = notice { self.notify_drone(&record); }
    }
}
