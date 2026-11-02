//! Optional legacy sphere collision prediction and response. Mirrors
//! carbonengine destiny Ballpark::Gradient, Ballpark::Potential and
//! Collision.cpp as consumed by client destiny. Inputs are actor-owned ball
//! state; no database records are used. The restart-only policy defaults off
//! because detailed shape data is unavailable; enabled collisions use one
//! sphere per ball. Nonmassive balls do not occlude native
//! CheckVisibility queries (carbonengine destiny Ballpark.cpp:5299-5346).
//! No shipped client caller of CheckVisibility was found. Miniballs, force
//! fields, collision callbacks and bump events are absent.

use super::*;

pub(crate) fn enabled() -> bool { crate::config::game().space.collisions }

pub(super) fn massive(ball: &Ball, collisions: bool) -> bool {
    ball.massive && (ball.mode == MODE_MISSILE || collisions)
}

pub(super) fn reset_event(ship: i64, stamp: i64, collisions: bool) -> Option<Value> {
    (!collisions).then(|| event(stamp, "SetBallMassive", vec![Value::Int(ship), Value::Bool(false)]))
}

/// Ballpark::Potential collision acceleration multiplier.
const RESPONSE_SCALE: f64 = 0.85;
/// Ballpark::Potential additional overlap clearance in metres.
const OVERLAP_MARGIN: f64 = 1.0;
/// Ballpark::Potential light-body response amplification threshold.
const MASS_RATIO_LIMIT: f64 = 25.0;
/// Ballpark::Potential relative speed squared threshold for recursive response.
const RELATIVE_SPEED_SQUARED: f64 = 0.0001;
/// Ballpark::Potential maximum overlap recursion depth.
const MAX_RECURSION: usize = 2;

#[derive(Clone, Copy)]
pub(super) struct Snapshot {
    pub(super) id: i64, mode: u8, follow: i64, launcher: i64, pub(super) free: bool, massive: bool, cloak: u8,
    radius: f64, mass: f64, effective_mass: f64, pos: V3, vel: V3, acceleration: V3,
    endpoint: V3, pub(super) time_factor: f64,
}

impl Snapshot {
    fn new(b: &Ball, collisions: bool) -> Self {
        let mass = if b.free { b.mass * b.agility as f64 } else { CELESTIAL_STATIC_MASS };
        let acceleration = b.segment_acceleration.unwrap_or_default();
        let time_factor = integration_factor(mass, DESTINY_FRICTION);
        let endpoint = integrate(b.pos, b.vel, acceleration, mass, DESTINY_FRICTION, time_factor, 1.0).0;
        Self { id: b.id, mode: b.mode, follow: b.follow_id, launcher: b.launcher,
            free: b.free, massive: massive(b, collisions), cloak: b.cloak, radius: b.radius as f64,
            mass: b.mass, effective_mass: mass, pos: b.pos, vel: b.vel, acceleration,
            endpoint, time_factor }
    }
}

fn eligible(me: &Snapshot, other: &Snapshot) -> bool {
    if !me.free || !me.massive || me.id == other.id { return false; }
    if me.mode == MODE_MISSILE && me.launcher >= 0 {
        return other.id == me.follow && other.id != me.launcher;
    }
    other.massive && other.cloak == 0 && other.mode != MODE_MISSILE
        && !(me.mode == MODE_MISSILE && other.id == me.launcher)
}

/// Shared cold/evolution gate: native overlap includes exact tangency.
pub(super) fn stationary_contact(balls: &HashMap<i64, Ball>, collisions: bool) -> bool {
    if !collisions { return false; }
    balls.values().filter(|b| b.free && massive(b, collisions) && b.mode == MODE_STOP && b.vel == [0.0; 3])
        .any(|me| balls.values().any(|other| {
            if me.id == other.id || !massive(other, collisions) || other.cloak != 0 || other.mode == MODE_MISSILE { return false; }
            let delta = sub(me.pos, other.pos);
            let radius = me.radius as f64 + other.radius as f64;
            dot(delta, delta) <= radius * radius
        }))
}

pub(super) fn freeze(scratch: &mut Vec<Snapshot>, balls: &HashMap<i64, Ball>, collisions: bool) {
    scratch.clear();
    scratch.extend(balls.values().map(|ball| Snapshot::new(ball, collisions)));
    scratch.sort_unstable_by_key(|b| b.id);
}

pub(super) fn gradient(me: &Snapshot, all: &[Snapshot]) -> V3 {
    let mut state = Fold { collision: -1.0, acceleration: [0.0; 3] };
    for other in all {
        if eligible(me, other) && swept_aabb(me.pos, me.endpoint, other.pos, other.endpoint, me.radius + other.radius) {
            potential(*me, *other, 0, &mut state);
        }
    }
    state.acceleration
}

pub(super) fn apply(scratch: &[Snapshot], balls: &mut HashMap<i64, Ball>) {
    for me in scratch.iter().filter(|b| b.free && b.massive) {
        let acceleration = gradient(me, scratch);
        if let Some(ball) = balls.get_mut(&me.id) {
            if let Some(intrinsic) = ball.segment_acceleration.as_mut() {
                *intrinsic = add(*intrinsic, acceleration);
            }
        }
    }
}

fn dot(a: V3, b: V3) -> f64 { a[0] * b[0] + a[1] * b[1] + a[2] * b[2] }

fn swept_aabb(p0: V3, p1: V3, q0: V3, q1: V3, radius: f64) -> bool {
    if !radius.is_finite() || radius < 0.0
        || p0.iter().chain(p1.iter()).chain(q0.iter()).chain(q1.iter()).any(|v| !v.is_finite()) {
        return true;
    }
    for i in 0..3 {
        if p0[i].min(p1[i]) > q0[i].max(q1[i]) + radius
            || q0[i].min(q1[i]) > p0[i].max(p1[i]) + radius { return false; }
    }
    true
}

fn quadratic(a: f64, b: f64, c: f64) -> Option<f64> {
    if a == 0.0 && b == 0.0 { return None; }
    let det = b * b - 4.0 * a * c;
    if det < 0.0 { return None; }
    Some((-b - det.sqrt()) * 0.5 / a)
}

fn collide_two_spheres(p0: V3, p1: V3, q0: V3, q1: V3, radius: f64) -> Option<f64> {
    let offset = sub(p0, q0);
    let offset2 = dot(offset, offset);
    if offset2 <= radius * radius { return Some(0.0); }
    let delta = sub(sub(p1, p0), sub(q1, q0));
    quadratic(dot(delta, delta), 2.0 * dot(offset, delta), offset2 - radius * radius)
        .filter(|s| *s >= 0.0 && *s <= 1.0)
}

struct Fold { collision: f64, acceleration: V3 }

fn potential(mut me: Snapshot, mut other: Snapshot, depth: usize, state: &mut Fold) {
    let radius = me.radius + other.radius;
    let Some(s) = collide_two_spheres(me.pos, me.endpoint, other.pos, other.endpoint, radius) else { return };
    let m1 = me.effective_mass;
    let m2 = other.effective_mass;
    let a1 = if s > 0.0 {
        let (p, mut vp) = integrate(me.pos, me.vel, me.acceleration, m1, DESTINY_FRICTION, me.time_factor, s);
        let (q, vq) = integrate(other.pos, other.vel, other.acceleration, m2, DESTINY_FRICTION, other.time_factor, s);
        let normal = norm(sub(q, p));
        let v1 = dot(vp, normal);
        let v2 = dot(vq, normal);
        if !other.free {
            vp = sub(vp, scale(normal, 2.0 * v1));
        } else {
            let v1p = (me.mass * v1 - other.mass * v1 + 2.0 * other.mass * v2) / (me.mass + other.mass);
            vp = add(vp, scale(normal, v1p - v1));
        }
        let (_, final_vel) = integrate(p, vp, me.acceleration, m1, DESTINY_FRICTION, me.time_factor, 1.0 - s);
        let k = DESTINY_FRICTION;
        let numerator = add(sub(add(scale(me.acceleration, -m1),
            scale(me.acceleration, me.time_factor * m1)),
            scale(scale(me.vel, me.time_factor), k)), scale(final_vel, k));
        scale(scale(scale(numerator, -1.0), 1.0 / m1), 1.0 / (me.time_factor - 1.0))
    } else {
        let offset = sub(other.pos, me.pos);
        let distance2 = dot(offset, offset);
        let normal = if distance2 == 0.0 {
            if me.id > other.id { [1.0, 0.0, 0.0] } else { [-1.0, 0.0, 0.0] }
        } else { norm(offset) };
        let distance = radius - distance2.sqrt() + OVERLAP_MARGIN;
        let d1 = m2 / (m1 + m2) * distance;
        let d2 = m1 / (m1 + m2) * distance;
        if me.free && other.free && depth < MAX_RECURSION {
            me.pos = sub(me.pos, scale(normal, d1));
            other.pos = add(other.pos, scale(normal, d2));
            me.endpoint = integrate(me.pos, me.vel, me.acceleration, m1, DESTINY_FRICTION, me.time_factor, 1.0).0;
            other.endpoint = integrate(other.pos, other.vel, other.acceleration, m2, DESTINY_FRICTION, other.time_factor, 1.0).0;
            potential(me, other, depth + 1, state);
            if m2 / m1 > MASS_RATIO_LIMIT { state.acceleration = scale(norm(state.acceleration), distance); }
            let relative = sub(me.vel, other.vel);
            if dot(relative, relative) > RELATIVE_SPEED_SQUARED { return; }
        }
        let tmp = 1.0 / (m1 + DESTINY_FRICTION);
        sub(sub(scale(normal, -d1 / (tmp * m1)), me.vel), scale(normal, dot(me.acceleration, normal)))
    };
    if (me.mode != MODE_MISSILE || other.id != me.follow) && s >= state.collision {
        let candidate = scale(a1, RESPONSE_SCALE);
        if s == state.collision {
            if dot(candidate, candidate) > dot(state.acceleration, state.acceleration) { state.acceleration = candidate; }
        } else {
            state.acceleration = candidate;
            state.collision = s;
        }
    }
}

#[cfg(test)] mod tests;
