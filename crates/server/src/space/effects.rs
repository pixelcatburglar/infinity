//! Actor-owned tractor movement and straight-flight projectile actions.
//! Destiny FollowBall and GotoDirection are delivered at action boundaries;
//! ownership and projectile timers are transient and are not persisted.
//! Queued movement is fenced to the original ball instances.
//! Bombs register one token-fenced terminal: the delay resolver is the only
//! `Some(centre)` detonation, while removal or lethal damage complete the
//! same callback once as `None`. Live-SDE projectiles carry a damageable
//! structure profile from `space::damageable`; it never produces combat facts,
//! killmails or durable loss.
use super::*;
use crate::space::system::observers::{BallToken, DestinyAudience};
use std::sync::atomic::{AtomicBool, Ordering};

pub(super) struct Tractor {
    module: i64,
    generation: Arc<AtomicBool>,
    source: i64,
    saved: Option<(f32, f32, u8, V3, i64, f32)>,
}

/// One registered bomb flight: the ball incarnation token that fences its
/// launch action and delay timer, the launch-time owner identity and fitted
/// blast reach for Defender threat checks, the armed detonation deadline
/// once the launch applies, plus the callback completed exactly once by
/// whichever terminal consumes it first.
pub(super) struct BombFlight {
    token: BallToken,
    identity: Option<crate::services::effects::defenders::ProjectileIdentity>,
    blast_range: f64,
    detonate_at_ms: Option<i64>,
    impact: Box<dyn FnOnce(&mut System, Option<V3>) + Send>,
}

impl System {
    pub(crate) fn begin_tractor(&mut self, target: i64, module: i64, generation: Arc<AtomicBool>, velocity: f32, stop_range: f32, source: i64) -> bool {
        if target == source || !velocity.is_finite() || velocity <= 0.0 || !stop_range.is_finite() || stop_range < 0.0
            || generation.load(Ordering::Relaxed) || self.ball(target).is_none_or(|b| !b.free || b.mode == MODE_WARP)
            || self.ball(source).is_none_or(|b| b.cloak != 0 || b.mode == MODE_WARP) { return false; }
        if let Some(owner) = self.tractors.get(&target) {
            if !owner.generation.load(Ordering::Relaxed) { return false; }
            let (old_module, old_generation) = (owner.module, owner.generation.clone());
            self.end_tractor(target, old_module, &old_generation);
        }
        self.tractor_epochs.entry(target).or_insert_with(|| Arc::new(()));
        self.tractors.insert(target, Tractor { module, generation: generation.clone(), source, saved: None });
        let target_token = self.capture_ball(target).unwrap();
        let source_token = self.capture_ball(source).unwrap();
        self.actions.push_back((self.current_time, Box::new(move |s, stamp| {
            if !s.ball_token_current(target_token) || !s.ball_token_current(source_token) { return; }
            if generation.load(Ordering::Relaxed) || !s.tractors.get(&target).is_some_and(|t| t.module == module && Arc::ptr_eq(&t.generation, &generation)) { return; }
            let Some(mut ball) = s.balls.remove(&target) else { s.tractors.remove(&target); return };
            let Some(source_ball) = s.balls.get(&source) else { s.balls.insert(target, ball); s.tractors.remove(&target); return };
            if source_ball.cloak != 0 || source_ball.mode == MODE_WARP { s.balls.insert(target, ball); s.tractors.remove(&target); return; }
            let saved = (ball.max_vel, ball.speed_fraction, ball.mode, ball.goto, ball.follow_id, ball.follow_range);
            if !ball.follow_ball(source_ball, stop_range) { s.balls.insert(target, ball); s.tractors.remove(&target); return; }
            let speed_accepted = ball.set_max_speed(f64::from(velocity));
            ball.set_speed_fraction(1.0);
            ball.last_ms = s.current_time * MILLISECONDS_PER_SECOND;
            ball.interpolation_anchor_ms = s.ms;
            ball.old_pos = ball.pos;
            ball.old_vel = ball.vel;
            ball.segment_acceleration = None;
            let visibility = (target, ball.cloak);
            s.balls.insert(target, ball);
            s.tractors.get_mut(&target).unwrap().saved = Some(saved);
            let mut events = Vec::new();
            if speed_accepted {
                events.push(event(stamp, "SetMaxSpeed", vec![Value::Int(target), Value::Float(f64::from(velocity))]));
            }
            events.push(event(stamp, "FollowBall", vec![Value::Int(target), Value::Int(source), Value::Float(f64::from(stop_range))]));
            events.push(event(stamp, "SetSpeedFraction", vec![Value::Int(target), Value::Float(1.0)]));
            // A follow event names the leading ball too: recipients must
            // already cover both ends of the ordered pair.
            let mut recipients = Vec::new();
            for (token, _) in s.scene_observers(&DestinyAudience::Visible { id: visibility.0, cloak: visibility.1 }) {
                if s.scene_ball_covered(token, source) { recipients.push(token); }
            }
            let count = recipients.len();
            for (i, token) in recipients.into_iter().enumerate() {
                let batch = if i + 1 == count { std::mem::take(&mut events) } else { events.clone() };
                s.emit_destiny(DestinyAudience::Observer(token), batch);
            }
        })));
        true
    }

    pub(crate) fn end_tractor(&mut self, target: i64, module: i64, generation: &Arc<AtomicBool>) {
        if !self.tractors.get(&target).is_some_and(|t| t.module == module && Arc::ptr_eq(&t.generation, generation)) { return; }
        let owner = self.tractors.remove(&target).unwrap();
        let Some((speed, fraction, mode, goto, follow, range)) = owner.saved else { return };
        let Some(epoch) = self.tractor_epochs.get(&target).cloned() else { return };
        self.command(target, move |ball, stamp, system| {
            if !system.tractor_epochs.get(&target).is_some_and(|current| Arc::ptr_eq(current, &epoch)) { return Vec::new(); }
            let mut events = Vec::new();
            if ball.set_max_speed(f64::from(speed)) {
                events.push(event(stamp, "SetMaxSpeed", vec![Value::Int(target), Value::Float(f64::from(speed))]));
            }
            let movement = if mode == MODE_GOTO && ball.goto_point(goto) {
                event(stamp, "GotoPoint", vec![Value::Int(target), Value::Float(goto[0]), Value::Float(goto[1]), Value::Float(goto[2])])
            } else if matches!(mode, MODE_FOLLOW | MODE_ORBIT) && system.ball(follow).is_some_and(|other|
                if mode == MODE_FOLLOW { ball.follow_ball(other, range) } else { ball.orbit(other, range) }) {
                event(stamp, if mode == MODE_FOLLOW { "FollowBall" } else { "Orbit" }, vec![Value::Int(target), Value::Int(follow), Value::Float(f64::from(range))])
            } else {
                ball.stop();
                event(stamp, "Stop", vec![Value::Int(target)])
            };
            events.push(movement);
            ball.set_speed_fraction(fraction);
            events.push(event(stamp, "SetSpeedFraction", vec![Value::Int(target), Value::Float(f64::from(fraction))]));
            events
        });
    }

    pub(super) fn cleanup_tractors(&mut self, id: i64) {
        self.tractor_epochs.remove(&id);
        self.tractors.remove(&id);
        let affected: Vec<_> = self.tractors.iter().filter(|(_, t)| t.source == id)
            .map(|(&target, t)| (target, t.module, t.generation.clone())).collect();
        for (target, module, generation) in affected { self.end_tractor(target, module, &generation); }
    }

    /// Launch a straight-flight bomb and register its one token-fenced
    /// terminal. `impact` runs at most once: `Some(centre)` only from the
    /// delay resolver's real detonation, `None` for removal, lethal damage or
    /// a refused launch. In a live-SDE actor the charge type must yield a
    /// valid damageable profile: a refusal completes `impact` as `None`
    /// without inserting. Actors without an SDE keep the neutral motion
    /// behaviour and register no capability.
    pub(crate) fn launch_bomb(&mut self, ball: Ball, heading: V3, delay_ms: u64, blast_range: f64,
        impact: impl FnOnce(&mut System, Option<V3>) + Send + 'static) {
        let profile = match self.sde() {
            Some(sde) => match crate::space::damageable::DamageableProjectile::from_sde(sde, ball.type_id) {
                Some(profile) => Some(profile),
                None => { impact(self, None); return; }
            },
            None => None,
        };
        let id = ball.id;
        self.add_ship(ball);
        let identity = self.projectile_identity(id);
        if let Some(profile) = profile {
            self.damageable_projectiles.insert(id, profile);
        }
        let token = self.capture_ball(id).expect("inserted bomb");
        self.bomb_flights.insert(id, BombFlight {
            token, identity, blast_range, detonate_at_ms: None, impact: Box::new(impact),
        });
        self.actions.push_back((self.current_time, Box::new(move |s, stamp| {
            if !s.ball_token_current(token) { return; }
            let Some(ball) = s.balls.get_mut(&id) else { return };
            if !ball.goto_direction(heading) { return; }
            ball.set_speed_fraction(1.0);
            ball.last_ms = s.current_time * MILLISECONDS_PER_SECOND;
            ball.interpolation_anchor_ms = s.ms;
            ball.old_pos = ball.pos;
            ball.old_vel = ball.vel;
            ball.segment_acceleration = None;
            let visibility = (id, ball.cloak);
            s.publish_ready_prefix(stamp);
            s.emit_destiny(DestinyAudience::Visible { id: visibility.0, cloak: visibility.1 }, vec![
                event(stamp, "GotoDirection", vec![Value::Int(id), Value::Float(heading[0]), Value::Float(heading[1]), Value::Float(heading[2])]),
                event(stamp, "SetSpeedFraction", vec![Value::Int(id), Value::Float(1.0)]),
            ]);
            if let Some(flight) = s.bomb_flights.get_mut(&id) {
                flight.detonate_at_ms = Some(s.ms.saturating_add(i64::try_from(delay_ms).unwrap_or(i64::MAX)));
            }
            s.schedule(delay_ms, move |s| s.resolve_bomb(token));
        })));
    }

    /// The sole real detonation terminal: a current incarnation token and a
    /// still-registered flight take the callback, the live ball's sampled
    /// position becomes the centre, the capability is cleared and the ball
    /// destroyed, and only then does the callback report `Some(centre)`. A
    /// stale timer or an already-consumed flight is a no-op.
    fn resolve_bomb(&mut self, token: BallToken) {
        if !self.ball_token_current(token) { return; }
        let id = token.id();
        if !self.bomb_flights.get(&id).is_some_and(|flight| flight.token == token) { return; }
        let flight = self.bomb_flights.remove(&id).expect("bomb flight verified");
        let centre = self.sample_position(id);
        self.damageable_projectiles.remove(&id);
        self.destroy_ball(id);
        (flight.impact)(self, centre);
    }

    /// Drop the bomb's terminal registration as its ball leaves, completing
    /// the callback once as a cancel. The entry is taken before the callback
    /// runs, so a re-entrant removal cannot consume it twice.
    pub(super) fn remove_bomb_flight(&mut self, id: i64) {
        let Some(flight) = self.bomb_flights.remove(&id) else { return };
        (flight.impact)(self, None);
    }

    /// Whether the ball still carries a live damageable projectile profile.
    pub(crate) fn projectile_damageable(&self, id: i64) -> bool {
        self.damageable_projectiles.contains_key(&id)
    }
    /// Select and reserve one hostile bomb whose remaining blast path still
    /// covers the source, inside `reach_m` surface distance. The source must
    /// be a known, present, non-warping pilot ball: an unknown source fails
    /// closed, exactly like an unknown projectile identity.
    pub(crate) fn reserve_defender_target(&mut self, source_id: i64, reach_m: f64)
        -> Option<(i64, crate::space::system::observers::BallToken)> {
        if !reach_m.is_finite() || reach_m <= 0.0 { return None; }
        let source_identity = self.projectile_identity(source_id)?;
        let source_ball = self.ball(source_id)?;
        if source_ball.is_warping() { return None; }
        let (source_pos, source_radius) = (source_ball.position_at(self.ms), f64::from(source_ball.radius));
        let mut candidates = Vec::new();
        for (&id, flight) in &self.bomb_flights {
            if id == source_id || self.defender_reservations.contains(&id) { continue; }
            let (Some(identity), Some(detonate_at_ms)) = (flight.identity, flight.detonate_at_ms) else { continue; };
            let Some(bomb) = self.ball(id) else { continue };
            let (bomb_pos, bomb_vel) = bomb.state_at(self.ms);
            let surface_distance = (crate::space::dist(source_pos, bomb_pos)
                - source_radius - f64::from(bomb.radius)).max(0.0);
            if !surface_distance.is_finite() || surface_distance > reach_m { continue; }
            let endpoint = crate::space::add(bomb_pos, crate::space::scale(bomb_vel,
                crate::space::ms_to_seconds(detonate_at_ms.saturating_sub(self.ms).max(0) as f64)));
            if !crate::services::effects::defenders::bomb_threatens(source_pos, source_radius,
                bomb_pos, f64::from(bomb.radius), endpoint, flight.blast_range) { continue; }
            candidates.push(crate::services::effects::defenders::Candidate {
                id, surface_distance, expiry_at_ms: detonate_at_ms, identity,
            });
        }
        let selected = crate::services::effects::defenders::choose_candidate(source_identity, candidates,
            &self.defender_reservations)?;
        let token = self.capture_ball(selected.id)?;
        self.defender_reservations.insert(selected.id);
        Some((selected.id, token))
    }


    /// Apply one typed raw vector to a live damageable projectile. `None`
    /// means there is no profile or the vector was refused. A lethal outcome
    /// runs the shared terminal and still reports itself; a non-lethal one
    /// keeps the reduced structure in flight.
    pub(crate) fn apply_projectile_damage(&mut self, id: i64, raw: [f64; 4])
        -> Option<crate::space::damageable::ProjectileDamageOutcome> {
        let outcome = self.damageable_projectiles.get_mut(&id)?.apply(raw)?;
        if outcome.destroyed { self.end_projectile(id); }
        Some(outcome)
    }

    /// Shared lethal terminal: take every projectile entry for this
    /// incarnation, destroy the ball, then run each taken callback once as a
    /// non-impact terminal. The ball's removal hooks then observe empty
    /// registries and cannot consume anything twice.
    pub(super) fn end_projectile(&mut self, id: i64) {
        self.damageable_projectiles.remove(&id);
        let finish = self.take_missile_finish(id);
        let impact = self.bomb_flights.remove(&id).map(|flight| flight.impact);
        self.destroy_ball(id);
        if let Some(finish) = finish { finish(self, None); }
        if let Some(impact) = impact { impact(self, None); }
    }

    pub(crate) fn hostiles_targeting(&self, ship: i64) -> Vec<i64> {
        let mut result: Vec<_> = self.brains.iter().filter_map(|(&id, brain)|
            (brain.announced && brain.target.is_some_and(|(target, _, _)| target == ship)
                && self.contains(id) && self.contains(ship)).then_some(id)).collect();
        result.sort_unstable();
        result
    }
}
