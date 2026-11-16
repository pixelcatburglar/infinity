//! The NPC half of the shared precursor-turret spool: the brain-owned ramp a
//! disintegrator (SDE 6995 targetDisintegratorAttack) NPC binds once per
//! target generation, plus the sustained beam effect published for it. The
//! `current`/`advance` pair is called only inside the shared turret shot's
//! single actor turn — the worker never re-advances a finished shot and needs
//! no in-flight or retry state. The generation ends the ramp outright: target
//! loss, a bound shot that cannot fire, and out-of-optimal all return the next
//! admitted shot to base.
use super::*;
use crate::services::effects::spool::{graphic_info, SpoolBinding, SpoolParameters};
use crate::space::{special_fx, System};
use infinity_core::consts::dogma::EFFECT_TARGET_DISINTEGRATOR_ATTACK;
use marshal::Value;
/// Server long-run repeat count for a sustained NPC beam, matching the
/// client's `sys.maxint` sustained-effect shape
/// (eve/client/script/ui/inflight/spaceObject/sovereigntyInfrastructueHub.py:38,43).
/// The beam is stopped explicitly on target loss or out-of-optimal, so the
/// count is never meant to run out; a nonpositive repeat would encode a
/// single cycle and must not be used here.
const NPC_BEAM_REPEAT: i64 = i64::MAX;

/// The beam effect currently published for one bound ramp: the binding it was
/// started for plus the arguments its stop event mirrors.
#[derive(Clone)]
pub(crate) struct NpcBeam {
    binding: SpoolBinding,
    guid: String,
    target: i64,
    parameters: SpoolParameters,
}

/// The OnSpecialFX event for one beam edge; the stop mirrors the start's
/// duration, repeat and `graphicInfo` payload.
fn beam_fx(stamp: i64, source: i64, beam: &NpcBeam, duration_ms: f64, start: bool) -> Value {
    special_fx(stamp, source, None, Some(beam.target), None, &beam.guid, true,
        i64::from(start), i64::from(start), Value::Float(duration_ms), NPC_BEAM_REPEAT, None, 0,
        graphic_info(beam.parameters))
}

impl System {
    /// The ramp bonus the next admitted shot applies. The captured identity is
    /// the only ramp authority: the brain's target generation and both combat
    /// memberships are compared against the live record, never rebound, and a
    /// mismatched or stale binding is refused. The first valid shot of a new
    /// generation binds here; `SpoolState::bind` makes a same-identity call a
    /// no-op.
    pub(crate) fn npc_spool_current(&mut self, source: i64, binding: SpoolBinding, parameters: SpoolParameters) -> Option<f64> {
        let Some(brain) = self.brains.get(&source) else { return None };
        if brain.target_generation != binding.activation_generation
            || !brain.target.is_some_and(|(ship, _, _)| ship == binding.target)
            || self.combat_membership_generation(source) != Some(binding.source_membership)
            || self.combat_membership_generation(binding.target) != Some(binding.target_membership)
            || brain.turret_spool.binding().is_some_and(|bound| bound != binding) {
            return None;
        }
        let brain = self.brains.get_mut(&source)?;
        brain.turret_spool.bind(binding);
        Some(brain.turret_spool.current(parameters))
    }

    /// Advance the bound ramp once for an accepted shot, publishing the
    /// sustained beam on the first one: every later shot of the same binding
    /// only ramps, so the client never sees the beam strength reset.
    pub(crate) fn npc_spool_advance(&mut self, source: i64, binding: SpoolBinding, parameters: SpoolParameters) -> bool {
        let fx = self.sde()
            .and_then(|sde| sde.dogma.effect(EFFECT_TARGET_DISINTEGRATOR_ATTACK))
            .and_then(|effect| effect.guid.as_deref())
            .and_then(|guid| guid.split(',').next())
            .map(str::to_owned);
        let start = {
            let Some(brain) = self.brains.get_mut(&source) else { return false };
            if brain.turret_spool.binding() != Some(binding) { return false; }
            brain.turret_spool.advance(parameters);
            match (brain.beam.is_none(), fx) {
                (true, Some(guid)) => {
                    let beam = NpcBeam { binding, guid, target: binding.target, parameters };
                    let duration_ms = brain.config.rate_of_fire_ms;
                    brain.beam = Some(beam.clone());
                    Some((beam, duration_ms))
                }
                _ => None,
            }
        };
        if let Some((beam, duration_ms)) = start {
            self.publish_destiny(source, move |stamp| vec![beam_fx(stamp, source, &beam, duration_ms, true)]);
        }
        true
    }

    /// A bound shot that could not fire ends only its own identity: the ramp
    /// and the published beam die together, and a newer binding is never
    /// touched by an old worker's reset.
    pub(crate) fn npc_spool_reset(&mut self, source: i64, binding: SpoolBinding) {
        let stop = {
            let Some(brain) = self.brains.get_mut(&source) else { return };
            if brain.turret_spool.binding() == Some(binding) { brain.turret_spool.reset(); }
            if brain.beam.as_ref().is_some_and(|beam| beam.binding == binding) {
                brain.beam.take().map(|beam| (beam, brain.config.rate_of_fire_ms))
            } else { None }
        };
        if let Some((beam, duration_ms)) = stop {
            self.publish_destiny(source, move |stamp| vec![beam_fx(stamp, source, &beam, duration_ms, false)]);
        }
    }

    /// Target loss ends the ramp outright: the whole generation is over, so no
    /// binding check applies and any published beam stops.
    pub(super) fn npc_spool_stop(&mut self, source: i64, brain: &mut NpcBrain) {
        brain.turret_spool.reset();
        if let Some(beam) = brain.beam.take() {
            let duration_ms = brain.config.rate_of_fire_ms;
            self.publish_destiny(source, move |stamp| vec![beam_fx(stamp, source, &beam, duration_ms, false)]);
        }
    }
}
