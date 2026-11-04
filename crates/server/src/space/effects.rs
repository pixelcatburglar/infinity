//! Actor-owned tractor movement and straight-flight projectile actions.
//! Destiny FollowBall and GotoDirection are delivered at action boundaries;
//! ownership and projectile timers are transient and are not persisted.
use super::*;
use std::sync::atomic::{AtomicBool, Ordering};

pub(super) struct Tractor {
    module: i64,
    generation: Arc<AtomicBool>,
    source: i64,
    saved: Option<(f32, f32, u8, V3, i64, f32)>,
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
        self.actions.push_back((self.current_time, Box::new(move |s, stamp| {
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
            s.deliver(Some(visibility), events);
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

    pub(crate) fn launch_bomb(&mut self, ball: Ball, heading: V3, delay_ms: u64, impact: impl FnOnce(&mut System) + Send + 'static) {
        let id = ball.id;
        self.add_ship(ball);
        self.actions.push_back((self.current_time, Box::new(move |s, stamp| {
            let Some(ball) = s.balls.get_mut(&id) else { return };
            if !ball.goto_direction(heading) { return; }
            ball.set_speed_fraction(1.0);
            ball.last_ms = s.current_time * MILLISECONDS_PER_SECOND;
            ball.interpolation_anchor_ms = s.ms;
            ball.old_pos = ball.pos;
            ball.old_vel = ball.vel;
            ball.segment_acceleration = None;
            let visibility = (id, ball.cloak);
            s.deliver(Some(visibility), vec![
                event(stamp, "GotoDirection", vec![Value::Int(id), Value::Float(heading[0]), Value::Float(heading[1]), Value::Float(heading[2])]),
                event(stamp, "SetSpeedFraction", vec![Value::Int(id), Value::Float(1.0)]),
            ]);
            s.schedule(delay_ms, impact);
        })));
    }

    pub(crate) fn hostiles_targeting(&self, ship: i64) -> Vec<i64> {
        let mut result: Vec<_> = self.brains.iter().filter_map(|(&id, brain)|
            (brain.announced && brain.target.is_some_and(|(target, _, _)| target == ship)
                && self.contains(id) && self.contains(ship)).then_some(id)).collect();
        result.sort_unstable();
        result
    }
}
