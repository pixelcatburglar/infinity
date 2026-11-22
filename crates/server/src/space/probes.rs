//! Actor-owned interdiction probes: stationary, damageable entities born by
//! a bubble-probe launcher's module cycle. Each probe owns one ball, one
//! damageable-projectile profile, one lifetime generation and one expiry
//! timer; a warp-disruption probe raises a field on its own ball through the
//! shared interdiction authority, and a stasis probe fires one delayed pulse
//! onto the shared timed runtime inputs. Probes are transient: the firing
//! ship's stop, departure or logout never retires one, but a shutdown or
//! restart removes it without restoring state or refunding the consumed
//! charge. They are not ships or NPCs: no combat record, killmail, insurance
//! or loot, and no missile flight or impact.
//! Destiny AddBalls births are consumed by `eve/client/script/remote/michelle.py`;
//! probe data comes from the in-memory SDE and the launch-frozen payload.

use super::*;
use crate::services::effects::probes::ProbePayload;
use crate::services::effects::projected::scaled;
use crate::services::fit::runtime::{RuntimeReceiverKey, RuntimeSourceKey, STASIS_PROBE_NAMESPACE};
use crate::services::fit::Key;
use crate::space::combat::inputs::ChargeToken;
use crate::space::combat::loaded::LoadedPermit;
use crate::space::dbuff::TimedRuntimeInput;
use crate::space::system::observers::BallToken;
use infinity_core::consts::CATEGORY_SHIP;
use marshal::Value;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
/// Stable actor timer name for probe lifetime expiry.
const PROBE_EXPIRE_TIMER: &str = "space::probes::expire";
/// Stable actor timer name for a stasis probe's delayed pulse.
const STASIS_PULSE_TIMER: &str = "space::probes::pulse";

/// A nonnegative finite millisecond duration whose cast due fits the actor clock.
fn checked_deadline(now_ms: i64, duration: f64) -> Option<i64> {
    (duration.is_finite() && duration >= 0.0 && duration < i64::MAX as f64)
        .then_some(duration as i64)
        .and_then(|delay| now_ms.checked_add(delay))
}

/// One live probe registered to its ball incarnation: the independent
/// lifetime flag (never the launcher's stop flag), the runtime-source
/// incarnation for landed stasis pulses and the frozen payload captured at
/// admission. Timers carry the ball token captured at birth; the registration
/// itself is keyed by the ball id the removal hook already has.
pub(super) struct Probe {
    generation: Arc<AtomicBool>,
    incarnation: u64,
    payload: ProbePayload,
}

/// The probe ball: stationary like a launched charge entity, overview
/// interactive like a celestial, carrying the projectile slim convention.
/// A warp probe publishes `warpDisruptionStartTime` as the birth FILETIME so
/// the client plays the field; a stasis probe publishes none. Source facts
/// are copied scalars, never a live ball.
fn probe_ball(type_id: i32, radius: f32, mass: f64, corp_id: i64, alliance_id: i64, launcher: i64, owner: i64, module: i64, pos: V3, id: i64, warp: bool) -> Ball {
    let slim = vec![
        ("sourceShipID", Value::Int(launcher)),
        ("launchModules", Value::list([Value::Int(module)])),
        ("warpDisruptionStartTime", if warp { Value::Int(crate::client::filetime_now()) } else { Value::None }),
    ];
    Ball {
        id, type_id, owner_id: owner, corp_id, alliance_id, char_id: None, signature_radius: None,
        mode: MODE_STOP, radius, mass, max_vel: 0.0, agility: 1.0, speed_fraction: 0.0,
        pos, vel: [0.0; 3], old_pos: pos, old_vel: [0.0; 3], segment_acceleration: None,
        goto: pos, follow_id: 0, follow_range: 0.0, free: true, global: false, massive: false,
        interactive: true, warp: WarpState::default(), last_ms: 0, interpolation_anchor_ms: 0,
        effect_stamp: 0, cloak: 0, cloak_until: 0, health: [1.0; DAMAGE_LAYER_COUNT], launcher, slim,
    }
}

impl System {
    /// The one probe admission: validate the source, representable deadlines
    /// and charge identity, pay the permit, then birth the stationary probe
    /// and its warp field in the same turn. `None` before payment releases
    /// the permit without a probe or a spent charge.
    pub(crate) fn admit_probe(&mut self, ship: i64, module: i64, owner: i64,
        permit: LoadedPermit, token: ChargeToken, inputs: crate::services::effects::probes::ProbeInputs) -> Option<(i64, i64)> {
        let crate::services::effects::probes::ProbeInputs { charge, lifetime_ms, payload } = inputs;
        // A deadline is representable only when it is a finite nonnegative
        // millisecond count whose absolute due fits in the actor clock.
        let now = self.ms;
        let launchable = match payload {
            ProbePayload::WarpField { radius } =>
                checked_deadline(now, lifetime_ms).is_some() && lifetime_ms > 0.0 && radius.is_finite() && radius > 0.0,
            ProbePayload::StasisPulse { warning_ms, radius, duration_ms, speed_factor, resistance_attribute, .. } =>
                checked_deadline(now, warning_ms).is_some() && radius.is_finite() && radius > 0.0
                    && checked_deadline(now, duration_ms).is_some() && duration_ms > 0.0
                    && checked_deadline(now, warning_ms + duration_ms).is_some()
                    && speed_factor.is_finite() && resistance_attribute > 0,
        };
        let refuse = |system: &mut System| { system.release_loaded_use(permit); None };
        if !launchable || inputs.charge != token.type_id { return refuse(self); }
        let Some(sde) = self.sde() else { return refuse(self); };
        let Some(source) = self.ball(ship) else { return refuse(self); };
        let (corp_id, alliance_id, launcher) = (source.corp_id, source.alliance_id, source.id);
        if source.mode == MODE_WARP || source.cloak != 0
            || !self.sample_position(ship).is_some_and(|pos| pos.iter().all(|v| v.is_finite())) {
            return refuse(self);
        }
        let (Some(data), Some(profile)) = (sde.types.get(charge), crate::space::damageable::DamageableProjectile::from_sde(sde, charge)) else {
            return refuse(self);
        };
        let (type_id, radius) = (data.type_id, data.radius.unwrap_or(1.0) as f32);
        let mass = data.mass.unwrap_or(1.0);
        let pos = self.sample_position(ship).expect("position sampled above");
        let id = crate::space::probe_ids::next_probe_id();
        let warp = matches!(payload, ProbePayload::WarpField { .. });
        let Some((_, remaining)) = self.consume_fitted_loaded(permit, token) else { return refuse(self); };
        let ball = probe_ball(type_id, radius, mass, corp_id, alliance_id, launcher, owner, module, pos, id, warp);
        self.add_ship(ball);
        let generation = Arc::new(AtomicBool::new(false));
        if let ProbePayload::WarpField { radius } = payload {
            // A fresh generation, a prevalidated positive finite radius and the
            // ball just inserted make a refusal an internal invariant violation.
            // The call runs in release: its Stop queue is the side effect.
            if !self.begin_interdiction(id, id, generation.clone(), radius) {
                unreachable!("probe field refused after a proven birth");
            }
        };
        self.damageable_projectiles.insert(id, profile);
        let probe_token = self.capture_ball(id).expect("inserted probe");
        let incarnation = crate::space::targeting::next_generation();
        let due = match payload {
            ProbePayload::WarpField { .. } => checked_deadline(now, lifetime_ms),
            ProbePayload::StasisPulse { warning_ms, .. } => checked_deadline(now, warning_ms),
        }.expect("deadline checked before birth");
        self.probes.insert(id, Probe { generation, incarnation, payload });
        match payload {
            ProbePayload::WarpField { .. } => {
                self.schedule_at_named(due, PROBE_EXPIRE_TIMER, move |s| s.probe_expire(probe_token));
            }
            ProbePayload::StasisPulse { .. } => {
                self.schedule_at_named(due, STASIS_PULSE_TIMER, move |s| s.stasis_pulse(probe_token));
            }
        }
        Some((id, remaining))
    }
    /// Retire this probe's field or pending pulse as its ball leaves by any
    /// path; removing the registration is the one exact-once cleanup shared
    /// by expiry, lethal damage and removal.
    pub(super) fn probe_departed(&mut self, id: i64) {
        let Some(probe) = self.probes.remove(&id) else { return };
        if matches!(probe.payload, ProbePayload::WarpField { .. }) {
            self.end_interdiction(id, &probe.generation);
        }
    }

    /// Expiry terminal: only a current incarnation with a live registration
    /// runs the shared projectile terminal, which destroys the ball and lets
    /// the ordinary removal hook retire the field exactly once.
    fn probe_expire(&mut self, token: BallToken) {
        if !self.ball_token_current(token) || !self.probes.contains_key(&token.id()) { return; }
        self.end_projectile(token.id());
    }

    /// The stasis pulse's inputs computed once at detonation: every live ship
    /// in the frozen selection radius (source and friends included, warp
    /// and cloak excluded), scaled by its resident web resistance through the
    /// shared neutral-scaling helper, keyed to the probe's own source id and
    /// incarnation with the frozen landed duration. A ship without a live fit,
    /// or whose resistance is not a finite nonnegative multiplier, is not a
    /// recipient; it does not cancel the ships that are.
    pub(super) fn stasis_inputs(&mut self, id: i64) -> Option<Vec<TimedRuntimeInput>> {
        let (radius, duration_ms, speed_factor, op, resistance_attribute, collection_id) = match self.probes.get(&id).map(|probe| probe.payload) {
            Some(ProbePayload::StasisPulse { radius, duration_ms, speed_factor, op, resistance_attribute, collection_id, .. }) =>
                (radius, duration_ms, speed_factor, op, resistance_attribute, collection_id),
            _ => return None,
        };
        let incarnation = self.probes.get(&id)?.incarnation;
        let center = self.sample_position(id)?;
        let Some(expires_at_ms) = checked_deadline(self.ms, duration_ms) else { return None };
        let ships: Vec<i64> = self.balls.values()
            .filter(|ball| ball.cloak == 0 && ball.mode != MODE_WARP)
            .filter(|ball| self.sde().and_then(|sde| sde.types.get(ball.type_id)).and_then(|data| data.category_id)
                .is_some_and(|category| category == i32::from(CATEGORY_SHIP)))
            .filter(|ball| dist(center, ball.position_at(self.ms)) <= radius)
            .map(|ball| ball.id)
            .collect();
        let mut inputs = Vec::with_capacity(ships.len());
        for ship in ships {
            // remoteResistanceID selects the target's strength multiplier.
            // The published stasisWebifierResistance default of 1 is full
            // strength; a missing resident fit is not a recipient.
            let Some(resistance) = self.with_combat_fit(ship, |fit, _, _| fit.value(Key::Ship, resistance_attribute)) else { continue };
            if !resistance.is_finite() || resistance < 0.0 { continue; }
            inputs.push(TimedRuntimeInput {
                receiver: RuntimeReceiverKey::Ship(ship),
                collection_id,
                source: RuntimeSourceKey { producer: STASIS_PROBE_NAMESPACE, source_id: id, incarnation },
                value: scaled(speed_factor, op, resistance),
                expires_at_ms,
            });
        }
        Some(inputs)
    }

    /// One detonation: sample eligible ships once, land every admitted web,
    /// then retire the probe through the shared projectile terminal. A ship
    /// that cannot supply a resident fit is skipped; the probe still ends
    /// exactly once. A stale token or a probe already destroyed lands nothing.
    fn stasis_pulse(&mut self, token: BallToken) {
        if !self.ball_token_current(token) { return; }
        let id = token.id();
        if let Some(inputs) = self.stasis_inputs(id) {
            self.land_runtime_inputs(&inputs);
        }
        if self.ball_token_current(token) { self.end_projectile(id); }
    }
}

#[cfg(all(test, feature = "tests-space"))]
#[path = "probes/tests.rs"]
mod tests;
