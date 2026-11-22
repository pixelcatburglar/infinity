//! Ship-sourced warp-disruption field raised by an unscripted warp disruption
//! field generator (`warpDisruptSphere`). A loaded focused script never reaches
//! this family: the scripted activation resolves to the script's own projected
//! effect first. The first accepted cycle raises the actor's field at the
//! source ball once; later cycles run the ordinary lifecycle without rearming
//! it. Field geometry, alignment cancellation and landing interception live in
//! the system actor; the transient field is nowhere persisted. Launched
//! interdiction probes raise their own fields through the same actor
//! authority; mobile warp disruptors are not supported.
//! Effect animations are published through the system's ordered scene channel.
use super::behaviors::Activation;
use super::lifecycle::{source_subject_current, Cycle, CycleOutcome};
use super::parameters::MasterPacket;
use super::registry::EffectFlag;
use crate::services::combat::Ctx;
use crate::services::fit::Fit;
use crate::services::{keyval, user_error, CallError};
use crate::space::{FxModule, System};
use crate::space::system::presentation::{FxRetention, ModuleFx};
use infinity_core::messages;
use marshal::Value;

/// A sphere takes no target: a supplied one is a forged request for the
/// scripted projected path, not an unscripted field admission. The shared
/// security-class check has already run by this point.
pub(super) fn validate(_cx: &Ctx, _fit: &Fit, activation: &Activation) -> Result<(), CallError> {
    if activation.target.is_some() {
        return Err(user_error(messages::MSG_DENIED_ACTIVATE_TARGET_NOT_PRESENT, vec![]));
    }
    Ok(())
}

/// The first accepted cycle raises the field once under the admitted
/// membership, presented effect and activation generation. A lost source, a
/// refused field admission or a refused animation ends the activation instead
/// of leaving a field that never published its start.
pub(super) async fn start(cx: &Ctx, cycle: &mut Cycle) -> Result<CycleOutcome, CallError> {
    if cycle.cycles > 0 { return Ok(CycleOutcome::Continue); }
    let Some(&MasterPacket::Field { radius }) = cycle.inputs.frame.as_ref().map(|frame| &frame.packet) else {
        return Err(CallError::Internal(anyhow::anyhow!("interdiction cycle captured without field inputs")));
    };
    let Some(membership) = cycle.source_membership else { return Ok(CycleOutcome::Stop("activation lost")); };
    let Some(guid) = cycle.effect.guid.as_deref().and_then(|g| g.split(',').next()).map(str::to_owned) else {
        return Ok(CycleOutcome::Stop("no field animation"));
    };
    let (ship, master, owner) = (cx.ship_id, cycle.modules[0].id, cx.char_id);
    let (generation, module, effect_id) = (cycle.stop.clone(), cycle.modules[0].clone(), cycle.presented_effect);
    let (duration_ms, repeat, offensive) = (cycle.duration_ms, cycle.repeat, cycle.effect.has(EffectFlag::Offensive));
    let accepted = cx.world.with(cx.system_id, move |s| {
        if !source_subject_current(s, ship, owner, membership, &module, effect_id, &generation)
            || !s.begin_interdiction(ship, master, generation.clone(), radius) {
            return false;
        }
        let published = s.publish_module_fx(master, &generation, ModuleFx {
            module: FxModule { item: Some(master), type_id: module.type_id },
            guid, charge: None, offensive, duration_ms, repeat,
            graphic_info: keyval([("range", Value::Float(radius))]),
        }, true, FxRetention::Current);
        // A start that could not publish its animation leaves no field behind.
        if !published { s.end_interdiction(master, &generation); }
        published
    }).await.unwrap_or(false);
    if !accepted { return Ok(CycleOutcome::Stop("interdiction field unavailable")); }
    Ok(CycleOutcome::Continue)
}

/// Retire only this generation's field and replay records; a stale stop cannot
/// clear a replacement raised by the same module.
pub(super) fn stop(s: &mut System, _cx: &Ctx, cycle: &Cycle) {
    let (master, generation) = (cycle.modules[0].id, &cycle.stop);
    s.end_interdiction(master, generation);
    s.retire_fx_activation(master, generation);
}

#[cfg(all(test, feature = "tests-effects"))]
mod tests;
