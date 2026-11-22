//! Interdiction probe launches through the shared module lifecycle.
//! The bubble-probe launcher presents `useMissiles`; module-aware activation
//! resolves the probe-launch family for it alone while notifications and
//! effect membership keep the presented effect. The typed payload the
//! compiled fields-deployables policy selects is captured once from the
//! resident fit and frozen at launch; a probe's own lifetime, field or
//! pending pulse outlives its launcher's stop, departure and logout.
//! Combat probes are transient actor state: they never enter `game.items`,
//! scanning lists, probe recovery or scoop, and a restart loses live probes
//! and landed stasis pulses without refunding the consumed charge.
//! Scanning `probeLaunching` stays refused here; scan probes keep the
//! exploration service path.
use super::behaviors::Activation;
use super::fx::weapon_fx;
use super::lifecycle::{source_subject_current, Cycle, CycleOutcome};
use super::parameters::MasterPacket;
use crate::lua::content::fields_deployables;
use crate::services::combat::{quantity_change, Ctx};
use crate::services::fit::{Fit, Key};
use crate::services::{user_error, CallError};
use crate::space::system::presentation::FxRetention;
use infinity_core::consts::dogma::{ATTR_DOOMSDAY_AOE_DURATION, ATTR_DOOMSDAY_AOE_RANGE, ATTR_DOOMSDAY_WARNING_DURATION,
    ATTR_EXPLOSION_DELAY, ATTR_REMOTE_RESISTANCE_ID, ATTR_SPEED_FACTOR, ATTR_WARP_SCRAMBLE_RANGE};
use infinity_core::messages;
use marshal::Value;

/// The frozen per-launch values captured once from the resident fit.
#[derive(Clone, Copy)]
pub(crate) struct ProbeInputs {
    /// The loaded charge the cycle pays and births.
    pub(crate) charge: i32,
    /// The probe entity's own lifetime in milliseconds (`explosionDelay`).
    pub(crate) lifetime_ms: f64,
    /// What the born probe does for that lifetime.
    pub(crate) payload: ProbePayload,
}

/// The probe entity's frozen payload.
#[derive(Clone, Copy)]
pub(crate) enum ProbePayload {
    /// A stationary warp-disruption field centred on the probe ball, metres.
    WarpField { radius: f64 },
    /// One delayed stasis-web pulse: warning delay, selection radius, landed
    /// web duration, the shared dbuff collection and its dogma operation, the
    /// captured `speedFactor` and the target resistance attribute selected by
    /// the charge's `remoteResistanceID`.
    StasisPulse {
        warning_ms: f64,
        radius: f64,
        duration_ms: f64,
        collection_id: i32,
        op: i32,
        speed_factor: f64,
        resistance_attribute: i32,
    },
}

/// The launcher's frozen payload for the loaded charge: the compiled policy's
/// family for the charge's group, with every dogma scalar read from the
/// resident fit's charge node and the live SDE. A non-finite value, an
/// unmapped group or a missing policy is a capture refusal, never a spent
/// charge or a partial probe.
pub(super) fn capture(fit: &Fit, cycle: &Cycle, deployables: Option<&fields_deployables::Definitions>) -> Result<ProbeInputs, CallError> {
    let master = cycle.modules[0].id;
    let charge = fit.source_type(Key::Charge(master))
        .ok_or_else(|| CallError::Internal(anyhow::anyhow!("probe launch captured without a charge type")))?;
    let group = fit.sde().types.get(charge).and_then(|data| data.group_id)
        .ok_or_else(|| CallError::Internal(anyhow::anyhow!("probe charge {charge} has no SDE group")))?;
    let definitions = deployables
        .ok_or_else(|| CallError::Internal(anyhow::anyhow!("probe launch captured without pinned definitions")))?;
    let value = |attr: i32| fit.value(Key::Charge(master), attr);
    let payload = match definitions.payload_family(group) {
        Some(fields_deployables::PayloadFamily::WarpField) => ProbePayload::WarpField { radius: value(ATTR_WARP_SCRAMBLE_RANGE) },
        Some(fields_deployables::PayloadFamily::StasisPulse) => {
            let collection_id = definitions.stasis_collection_id();
            let op = fit.sde().dbuffs.get(collection_id).and_then(|collection| collection.modifiers.first())
                .map(|row| row.modifier.op)
                .ok_or_else(|| CallError::Internal(anyhow::anyhow!("stasis dbuff collection {collection_id} has no modifiers")))?;
            ProbePayload::StasisPulse {
                warning_ms: value(ATTR_DOOMSDAY_WARNING_DURATION),
                radius: value(ATTR_DOOMSDAY_AOE_RANGE),
                duration_ms: value(ATTR_DOOMSDAY_AOE_DURATION),
                collection_id,
                op,
                speed_factor: value(ATTR_SPEED_FACTOR),
                resistance_attribute: value(ATTR_REMOTE_RESISTANCE_ID) as i32,
            }
        }
        None => return Err(CallError::Internal(anyhow::anyhow!("probe charge group {group} has no payload family"))),
    };
    let lifetime_ms = match &payload {
        ProbePayload::WarpField { .. } => value(ATTR_EXPLOSION_DELAY),
        ProbePayload::StasisPulse { .. } => 0.0,
    };
    let finite = match &payload {
        ProbePayload::WarpField { radius } => lifetime_ms.is_finite() && lifetime_ms > 0.0 && radius.is_finite() && *radius > 0.0,
        ProbePayload::StasisPulse { warning_ms, radius, duration_ms, speed_factor, resistance_attribute, .. } =>
            warning_ms.is_finite() && *warning_ms >= 0.0 && radius.is_finite() && *radius > 0.0
                && duration_ms.is_finite() && *duration_ms > 0.0 && speed_factor.is_finite() && *resistance_attribute > 0,
    };
    if !finite {
        return Err(CallError::Internal(anyhow::anyhow!("probe charge {charge} has invalid payload values")));
    }
    Ok(ProbeInputs { charge, lifetime_ms, payload })
}

/// A probe launch takes no target: a supplied one is a forged request, not a
/// launch. The shared warp, security-class and loaded-charge checks run at
/// the ordinary admission points.
pub(super) fn validate(_cx: &Ctx, _fit: &Fit, activation: &Activation) -> Result<(), CallError> {
    if activation.target.is_some() {
        return Err(user_error(messages::MSG_DENIED_ACTIVATE_TARGET_NOT_PRESENT, vec![]));
    }
    Ok(())
}

/// The one actor admission that pays exactly one loaded charge and births the
/// stationary probe: activation generation, admitted membership, source
/// presence, warp and cloak state, payload finiteness and loaded-slot
/// identity are rechecked inside a single system turn before the permit pays
/// and the ball enters. Any refusal releases the permit with neither a probe
/// nor a spent charge.
pub(super) async fn start(cx: &Ctx, cycle: &mut Cycle) -> Result<CycleOutcome, CallError> {
    let Some(&MasterPacket::Probe(inputs)) = cycle.inputs.frame.as_ref().map(|frame| &frame.packet) else {
        return Err(CallError::Internal(anyhow::anyhow!("probe cycle captured without probe inputs")));
    };
    let Some(membership) = cycle.source_membership else { return Ok(CycleOutcome::Stop("activation lost")); };
    let (ship, owner, module_id) = (cx.ship_id, cx.char_id, cycle.modules[0].id);
    let Some(member) = cycle.inputs.members.first_mut() else {
        return Err(CallError::Internal(anyhow::anyhow!("probe cycle captured without member inputs")));
    };
    let (permit, token) = member.take_charge();
    let Some((permit, token)) = permit.zip(token) else {
        if let Some(permit) = permit {
            cx.world.post(cx.system_id, move |s| s.release_loaded_use(permit));
        }
        return Ok(CycleOutcome::Stop("out of charges"));
    };
    let (module, presented, generation) = (cycle.modules[0].clone(), cycle.presented_effect, cycle.stop.clone());
    let flag = permit.flag;
    let admitted = cx.world.with(cx.system_id, move |s| {
        if !source_subject_current(s, ship, owner, membership, &module, presented, &generation) {
            s.release_loaded_use(permit);
            return None;
        }
        s.admit_probe(ship, module_id, owner, permit, token, inputs)
    }).await.flatten();
    let Some((_probe, remaining)) = admitted else {
        cx.notify("OnRemoteMessage", vec![Value::str(messages::MSG_CUSTOM_NOTIFY),
            Value::dict([("notify", Value::unicode(messages::deployables::PROBE_LAUNCH_REFUSED))])]);
        return Ok(CycleOutcome::Stop("probe launch refused"));
    };
    cx.notify("OnModuleAttributeChanges", vec![quantity_change(owner, ship, flag, inputs.charge, remaining, remaining + 1)]);
    if cycle.cycles == 0 {
        weapon_fx(cx, &cycle.modules, &cycle.effect, None, Some(inputs.charge), true,
            cycle.duration_ms, cycle.repeat, cycle.modules[0].id, &cycle.stop, FxRetention::Transient, Value::None);
    }
    Ok(CycleOutcome::Continue)
}

#[cfg(all(test, feature = "tests-effects"))]
#[path = "probes/tests.rs"]
mod tests;
