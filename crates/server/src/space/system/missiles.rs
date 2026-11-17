//! Actor-owned missile flights: queued registration, native launch at the
//! destiny boundary, per-segment target-only contact sweeps and one terminal
//! resolver shared by the contact and expiry timers. Steering and wire shape
//! stay native; the finish callback is the gameplay terminal and runs at most
//! once. A flight cancelled before contact keeps flying straight until its
//! frozen lifetime expires, preserving client visuals.
//! Stamped LaunchMissile and removal actions are consumed by
//! eve/client/script/remote/michelle.py; inputs are actor-owned balls and
//! launch-frozen lifetime values, with no database tables. Contact uses
//! one-second sphere chords, not general collision or restart recovery.
//! Queued launches and terminal timers are fenced to the original missile.
//! Live-SDE launches also register an actor-local damageable profile; it ends
//! with the flight's terminal and never enters ship combat.

use super::*;
use crate::space::system::observers::{BallToken, DestinyAudience};

/// Server policy for targeted missiles: destiny/src/Thunkers.cpp LaunchMissile
/// uses aimed launches to initialize direction toward an uncloaked target.
const MISSILE_AIMED_LAUNCH: bool = true;
/// Server policy for ordinary missiles; destiny/src/Thunkers.cpp sets ball massiveness from this flag.
const MISSILE_MASSIVE: bool = false;

/// A raw geometric contact recorded against the launch-applied clock, awaiting
/// its integer deadline. It is not committed damage until the resolver fires.
#[derive(Clone, Copy)]
struct PendingContact {
    /// Exact contact offset from applied launch in fractional milliseconds.
    elapsed_ms: f64,
    /// Target velocity at the exact contact fraction of its segment.
    target_velocity: V3,
    /// Absolute wall deadline `applied + ceil(elapsed_ms)`.
    deadline: i64,
}

/// One registered flight: queued, tracking, pending a contact or dead.
/// Dead flights keep only their expiry timer and visual ball.
pub(super) struct Flight {
    target: i64,
    launcher: i64,
    /// Frozen effective lifetime in fractional milliseconds from application.
    flight_time_ms: f64,
    /// Actor wall millisecond at which the launch action applied the flight.
    applied_at_ms: Option<i64>,
    /// Reserved target incarnation captured at Defender admission.
    target_token: Option<BallToken>,
    /// False permanently: never regains a target or produces a contact.
    eligible: bool,
    /// Defender flights resolve at the source-backed five-metre surface gap.
    defender: bool,
    pending: Option<PendingContact>,
    finish: Option<Box<dyn FnOnce(&mut System, Option<MissileImpact>) + Send>>,
}

/// The one damage-carrying terminal of a flight: target identity and the
/// exact target velocity at the contact fraction.
#[derive(Clone, Copy)]
pub(crate) struct MissileImpact {
    pub target: i64,
    pub target_velocity: crate::space::V3,
}

impl System {
    /// Insert a neutral missile now, register its flight, then launch it at the
    /// next destiny boundary. `finish` runs at most once: `Some` on the single
    /// contact terminal, `None` for invalidation, expiry, removal or a launch
    /// that cannot apply. An invalid lifetime consumes `finish` as a miss
    /// without inserting the ball. In a live-SDE actor the charge type must
    /// yield a valid damageable profile: a refusal consumes `finish` as a miss
    /// without inserting. Actors without an SDE keep the neutral motion
    /// behaviour and register no capability.
    pub(crate) fn launch_missile(&mut self, ball: Ball, target: i64, launcher: i64, flight_time_ms: f64,
        finish: impl FnOnce(&mut System, Option<MissileImpact>) + Send + 'static) {
        self.register_missile(ball, target, launcher, flight_time_ms, false, None, finish);
    }

    fn register_missile(&mut self, ball: Ball, target: i64, launcher: i64, flight_time_ms: f64,
        defender: bool, target_token: Option<BallToken>,
        finish: impl FnOnce(&mut System, Option<MissileImpact>) + Send + 'static) {
        if !flight_time_ms.is_finite() || flight_time_ms <= 0.0 || flight_time_ms.ceil() >= i64::MAX as f64 {
            finish(self, None);
            return;
        }
        let profile = match self.sde() {
            Some(sde) => match crate::space::damageable::DamageableProjectile::from_sde(sde, ball.type_id) {
                Some(profile) => Some(profile),
                None => { finish(self, None); return; }
            },
            None => None,
        };
        let id = ball.id;
        self.add_ship(ball);
        if let Some(profile) = profile {
            self.damageable_projectiles.insert(id, profile);
        }
        let token = self.capture_ball(id).expect("inserted missile");
        self.missile_flights.insert(id, Flight {
            target, launcher, flight_time_ms,
            applied_at_ms: None, target_token, eligible: true, defender, pending: None,
            finish: Some(Box::new(finish)),
        });
        self.actions.push_back((self.current_time, Box::new(move |s, stamp| {
            s.apply_missile_launch(token, stamp);
        })));
    }
    /// Register a Defender flight atomically with its reserved target token.
    /// The wrapped finish is the one reservation release point: on a contact
    /// it ends the still-current, still-reserved target through the shared
    /// projectile terminal (a cancel, never a detonation), then always
    /// releases the reservation before reporting.
    pub(crate) fn launch_defender(&mut self, ball: Ball, target: i64, target_token: BallToken, launcher: i64, flight_time_ms: f64,
        finish: impl FnOnce(&mut System, Option<MissileImpact>) + Send + 'static) {
        self.register_missile(ball, target, launcher, flight_time_ms, true, Some(target_token), move |s, contact| {
            if contact.is_some() && s.ball_token_current(target_token) && s.defender_reservations.contains(&target) {
                s.end_projectile(target);
            }
            s.defender_reservations.remove(&target);
            finish(s, contact);
        });
    }

    /// The one launch-time identity capture in the crate: the ball owner's
    /// online session (its pilot session first, then the owner id). `None`
    /// when the ball, the session or the owning corporation is unknown; a
    /// projectile with no identity is never a Defender target.
    pub(crate) fn projectile_identity(&self, ball: i64)
        -> Option<crate::services::effects::defenders::ProjectileIdentity> {
        let source = self.ball(ball)?;
        let session = source.char_id.and_then(|character| self.registry.online_session(character))
            .or_else(|| self.registry.online_session(source.owner_id))?;
        if source.owner_id <= 0 { return None; }
        Some(crate::services::effects::defenders::ProjectileIdentity {
            owner: source.owner_id,
            corporation: session.corporation_id?,
            alliance: session.alliance_id,
            war_faction: session.faction_id,
            fleet: session.fleet_id,
        })
    }

    /// Permanently cancel every pending hit on `target` (dock/jump acceptance,
    /// true warp, cloak and removal all route here). Dead flights keep flying.
    pub(crate) fn invalidate_missile_target(&mut self, target: i64) {
        self.invalidate_flights_where(|flight| flight.target == target);
    }

    /// Permanently cancel every flight launched by `launcher`.
    pub(super) fn invalidate_missile_launcher(&mut self, launcher: i64) {
        self.invalidate_flights_where(|flight| flight.launcher == launcher
            || flight.launcher.checked_abs() == Some(launcher));
    }

    /// Drop the missile's own flight record as its ball leaves, consuming the
    /// callback as a miss. The record is taken before the callback runs, so a
    /// re-entrant removal cannot consume it twice.
    pub(super) fn remove_missile_flight(&mut self, missile: i64) {
        let Some(flight) = self.missile_flights.remove(&missile) else { return };
        if let Some(finish) = flight.finish { finish(self, None); }
    }

    /// Take a live flight's finish callback without running it: the record is
    /// removed so later removal hooks cannot consume it again. The shared
    /// projectile terminal destroys the ball first, then completes the miss.
    pub(super) fn take_missile_finish(&mut self, missile: i64)
        -> Option<Box<dyn FnOnce(&mut System, Option<MissileImpact>) + Send>> {
        self.missile_flights.remove(&missile).and_then(|flight| flight.finish)
    }

    /// Kill matching flights and report each callback once as a miss. The ID
    /// scratch is drained through a local so callbacks may re-enter safely.
    fn invalidate_flights_where(&mut self, matches: impl Fn(&Flight) -> bool) {
        let mut ids = std::mem::take(&mut self.missile_scratch);
        ids.extend(self.missile_flights.iter().filter(|(_, flight)| flight.eligible && matches(flight))
            .map(|(&id, _)| id));
        for id in ids.drain(..) { self.invalidate_flight(id); }
        self.missile_scratch = ids;
    }

    /// Permanently disqualify one flight: consume its callback as a miss,
    /// clear its damageable profile and queue straight untracked flight so the
    /// ball stays visible until expiry. The callback is taken before it runs;
    /// re-entrant invalidation is safe.
    fn invalidate_flight(&mut self, id: i64) {
        let Some(flight) = self.missile_flights.get_mut(&id) else { return };
        if !flight.eligible { return; }
        flight.eligible = false;
        let finish = flight.finish.take();
        self.damageable_projectiles.remove(&id);
        self.command(id, |ball, stamp, _| {
            ball.massive = false;
            if !ball.goto_direction(ball.vel) { return Vec::new() }
            vec![
                event(stamp, "GotoDirection", vec![Value::Int(ball.id),
                    Value::Float(ball.vel[0]), Value::Float(ball.vel[1]), Value::Float(ball.vel[2])]),
                event(stamp, "SetBallMassive", vec![Value::Int(ball.id), Value::Bool(false)]),
            ]
        });
        if let Some(finish) = finish { finish(self, None); }
    }

    /// Apply the queued launch at the destiny boundary: freeze the actor wall
    /// time, run native steering toward the target's current scalars, send the
    /// original LaunchMissile event and arm the expiry timer. A flight that was
    /// invalidated or lost its launcher/target while queued applies untargeted
    /// and can never hit.
    fn apply_missile_launch(&mut self, token: BallToken, stamp: i64) {
        if !self.ball_token_current(token) { return; }
        let id = token.id();
        let Some(flight) = self.missile_flights.get_mut(&id) else { return };
        if flight.applied_at_ms.is_some() { return; }
        flight.applied_at_ms = Some(self.ms);
        let (target, launcher, flight_time_ms, eligible) =
            (flight.target, flight.launcher, flight.flight_time_ms, flight.eligible);
        let Some(mut ball) = self.balls.remove(&id) else {
            // The ball left between registration and application; still owe a miss.
            self.damageable_projectiles.remove(&id);
            if let Some(flight) = self.missile_flights.remove(&id) {
                if let Some(finish) = flight.finish { finish(self, None); }
            }
            return;
        };
        ball.last_ms = self.current_time * MILLISECONDS_PER_SECOND;
        ball.interpolation_anchor_ms = self.ms;
        let owner = launcher.checked_abs().and_then(|owner_id| {
            if owner_id == id { Some(&ball) } else { self.balls.get(&owner_id) }
        }).map(|b| (b.pos, b.vel, b.max_vel));
        let target_ball = owner.filter(|_| target >= 0).and_then(|_| {
            if target == launcher { None } else if target == id {
                Some(&ball)
            } else { self.balls.get(&target) }
        }).map(|b| (b.id, b.pos, b.radius, b.cloak));
        // Mirrors launch_native_missile's aimed condition: only a present,
        // uncloaked, non-launcher target can ever produce a contact.
        let live = eligible && owner.is_some()
            && target_ball.is_some_and(|t| t.0 != launcher && t.3 == 0);
        if let Some(owner) = owner {
            ball.launch_native_missile(owner, target_ball.filter(|_| live), launcher, stamp);
        }
        let visibility = (id, ball.cloak);
        self.balls.insert(id, ball);
        self.refresh_scene_location(id);
        let update = event(stamp, "LaunchMissile", vec![
            Value::Int(id), Value::Int(target), Value::Int(launcher),
            Value::Bool(MISSILE_AIMED_LAUNCH), Value::Bool(MISSILE_MASSIVE),
        ]);
        self.publish_ready_prefix(stamp);
        self.emit_destiny(DestinyAudience::Visible { id: visibility.0, cloak: visibility.1 }, vec![update]);
        if !live {
            // Permanent at application: consume the callback, keep the visual.
            let finish = self.missile_flights.get_mut(&id)
                .and_then(|f| { f.eligible = false; f.finish.take() });
            self.damageable_projectiles.remove(&id);
            if let Some(finish) = finish { finish(self, None); }
        }
        match self.ms.checked_add(flight_time_ms.ceil() as i64) {
            Some(deadline) => self.schedule_at(deadline, move |s| s.resolve_missile(token)),
            // The deadline cannot be represented; end the flight as a miss.
            None => { self.take(id); }
        }
    }

    /// Sweep every eligible applied flight against its own target only, on the
    /// just-completed segment bounded by `logical_ms`/`anchor_ms`. With no
    /// evolved motion the current positions are swept as a zero-length chord,
    /// which still reports a standing overlap. The recorded raw contact offset
    /// is clipped to the post-launch part of the segment and must satisfy
    /// `0 <= elapsed < lifetime`; only then does it freeze a pending contact.
    pub(super) fn scan_missile_segment(&mut self, logical_ms: i64, anchor_ms: i64, evolved: bool) {
        // A ball owns the just-completed segment only when this step actually
        // evolved it; everything else contributes a zero-length current chord.
        let in_segment = |b: &Ball| evolved
            && b.last_ms == logical_ms && b.interpolation_anchor_ms == anchor_ms;
        let mut ids = std::mem::take(&mut self.missile_scratch);
        ids.extend(self.missile_flights.iter().filter(|(_, flight)| {
            flight.eligible && flight.applied_at_ms.is_some() && flight.pending.is_none()
        }).map(|(&id, _)| id));
        for id in ids.drain(..) {
            let Some(flight) = self.missile_flights.get(&id) else { continue };
            if !flight.eligible || flight.pending.is_some() { continue; }
            let (applied, target, launcher, lifetime, target_token) = (
                flight.applied_at_ms.unwrap(), flight.target, flight.launcher, flight.flight_time_ms, flight.target_token);
            let f_min = ((applied - anchor_ms) as f64 / MILLISECONDS_PER_SECOND as f64).max(0.0);
            let dead = self.balls.get(&id).is_none() || {
                let target_dead = self.balls.get(&target).is_none_or(|b| b.cloak != 0 || b.is_warping())
                    || target_token.is_some_and(|token| !self.ball_token_current(token));
                let source_id = launcher.checked_abs().unwrap_or(launcher);
                let source_dead = source_id != id
                    && self.balls.get(&source_id).is_none_or(|b| b.cloak != 0 || b.is_warping());
                target_dead || source_dead
            };
            if dead { self.invalidate_flight(id); continue; }
            if f_min > 1.0 { continue; }
            let (me, target_ball) = (self.balls.get(&id).unwrap(), self.balls.get(&target).unwrap());
            let (p0, p1) = if in_segment(me) { (me.old_pos, me.pos) } else { (me.pos, me.pos) };
            let (q0, q1) = if in_segment(target_ball) { (target_ball.old_pos, target_ball.pos) }
                else { (target_ball.pos, target_ball.pos) };
            let clip = |a: V3, b: V3| add(a, scale(sub(b, a), f_min));
            let radius = f64::from(me.radius) + f64::from(target_ball.radius)
                + if flight.defender { crate::services::effects::defenders::DEFENDER_INTERCEPT_SURFACE_METRES } else { 0.0 };
            let (t_old_vel, t_vel, t_accel, t_mass) = if in_segment(target_ball) {
                (target_ball.old_vel, target_ball.vel, target_ball.segment_acceleration,
                    target_ball.mass * target_ball.agility as f64)
            } else {
                (target_ball.vel, target_ball.vel, None, 0.0)
            };
            let Some(frac) = collision::collide_two_spheres(clip(p0, p1), p1, clip(q0, q1), q1, radius) else { continue };
            let fraction = f_min + (1.0 - f_min) * frac;
            let elapsed = (anchor_ms - applied) as f64 + seconds_to_ms_f64(fraction);
            if !(elapsed >= 0.0 && elapsed < lifetime) { continue; }
            let target_velocity = if fraction == 0.0 || t_accel.is_none() { t_old_vel }
                else if fraction == 1.0 { t_vel }
                else { integrate(q0, t_old_vel, t_accel.unwrap(), t_mass, DESTINY_FRICTION, 0.0, fraction).1 };
            let Some(deadline) = applied.checked_add(elapsed.ceil() as i64) else { continue };
            if let Some(flight) = self.missile_flights.get_mut(&id) {
                flight.pending = Some(PendingContact { elapsed_ms: elapsed, target_velocity, deadline });
            }
            let Some(token) = self.capture_ball(id) else { continue };
            self.schedule_at(deadline, move |s| s.resolve_missile(token));
        }
        self.missile_scratch = ids;
    }

    /// The shared terminal for contact and expiry timers. A valid pending
    /// contact wins even when `now` is past expiry or the expiry timer dequeued
    /// first; only an already-fired callback is final, never a raw-time replay.
    fn resolve_missile(&mut self, token: BallToken) {
        if !self.ball_token_current(token) { return; }
        let id = token.id();
        let now = self.ms;
        enum Outcome { Hit(MissileImpact), Miss, Wait }
        let outcome = {
            let Some(flight) = self.missile_flights.get(&id) else { return };
            match flight.pending {
                Some(pending) if flight.eligible && pending.elapsed_ms < flight.flight_time_ms
                    && pending.deadline <= now => Outcome::Hit(MissileImpact {
                        target: flight.target, target_velocity: pending.target_velocity }),
                _ if flight.applied_at_ms.and_then(|applied|
                    applied.checked_add(flight.flight_time_ms.ceil() as i64)).is_some_and(|d| d <= now) => Outcome::Miss,
                _ => Outcome::Wait,
            }
        };
        let result = match outcome {
            Outcome::Wait => return,
            Outcome::Hit(impact) => Some(impact),
            Outcome::Miss => None,
        };
        let Some(flight) = self.missile_flights.remove(&id) else { return };
        self.damageable_projectiles.remove(&id);
        self.destroy_ball(id);
        if let Some(finish) = flight.finish { finish(self, result); }
    }
}

