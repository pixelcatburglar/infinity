//! Mining effect validation, animations and yield phase.
//! Client dogma mining effects use the cycle-start fit when extracting ore.
//! Rock identity is system-owned; storage and depletion are owned by services/mining.
//! Effect animations are published through the system's ordered scene channel.
use super::lifecycle::{Cycle, CycleOutcome, Module};
use super::parameters::MasterPacket;
use super::registry::EffectFlag;
use infinity_core::consts::{dogma::ATTR_MAX_RANGE, dogma::EFFECT_MINING_CLOUDS};
use infinity_core::messages;
use crate::services::{combat::{Ctx, targeting_charges::targets_of}, fit::{Fit, Key}, user_error, CallError};
use crate::space::{dist, metres_to_km, FxModule, System, system::presentation::{FxRetention, ModuleFx}};
use marshal::Value;
use tracing::warn;

fn guid(id: i64) -> &'static str {
    if id == EFFECT_MINING_CLOUDS as i64 { "effects.CloudMining" } else { "effects.Mining" }
}

pub(super) async fn validate(cx: &Ctx, fit: &Fit, module: &Module, target: Option<i64>) -> Result<(), CallError> {
    let Some(t) = target.filter(|t| targets_of(cx.char_id).contains(t)) else {
        return Err(user_error(messages::MSG_DENIED_ACTIVATE_TARGET_NOT_PRESENT, vec![]));
    };
    let ship = cx.ship_id;
    let (valid, gap) = cx.world.with(cx.system_id, move |s| {
        let valid = s.rock_type(t).is_some();
        let gap = s.ball(ship).zip(s.ball(t)).map(|(a, b)| dist(a.position_at(s.now_ms()), b.position_at(s.now_ms())) - a.radius as f64 - b.radius as f64);
        (valid, gap)
    }).await.unwrap_or((false, None));
    if !valid {
        return Err(user_error(messages::MSG_CUSTOM_NOTIFY, vec![("notify".into(), Value::unicode(messages::MINING_MODULE_TARGET_INVALID))]));
    }
    let range = fit.value(Key::Module(module.id), ATTR_MAX_RANGE);
    if gap.is_none_or(|g| g > range) {
        return Err(user_error(messages::MSG_CUSTOM_NOTIFY, vec![("notify".into(), Value::unicode(messages::mining_range(metres_to_km(range))))]));
    }
    Ok(())
}

pub(super) fn start(cx: &Ctx, cycle: &Cycle) -> CycleOutcome {
    if let Some(rock) = cycle.target {
        if !targets_of(cx.char_id).contains(&rock) { return CycleOutcome::Stop("target lost"); }
        if cycle.cycles == 0 {
            let modules = cycle.modules.clone();
            let (master, generation, duration, repeat, guid, is_offensive) = (cycle.modules[0].id, cycle.stop.clone(), cycle.duration_ms, cycle.repeat, guid(cycle.effect.id).to_owned(), cycle.effect.has(EffectFlag::Offensive));
            cx.world.post(cx.system_id, move |s| {
                for m in &modules {
                    s.publish_module_fx(master, &generation, ModuleFx {
                        module: FxModule { item: Some(m.id), type_id: m.type_id },
                        guid: guid.clone(),
                        charge: None,
                        offensive: is_offensive,
                        duration_ms: duration,
                        repeat,
                        graphic_info: Value::None,
                    }, true, FxRetention::Current);
                }
            });
        }
    }
    CycleOutcome::Continue
}

fn extraction_outcome(extraction: crate::services::mining::Extraction) -> CycleOutcome {
    let reason = if extraction.error.is_some() { "mining failed" } else { "mining finished" };
    if extraction.repeat { CycleOutcome::Continue }
    else if let Some(outcome) = extraction.outcome { CycleOutcome::Ended { reason, outcome } }
    else { CycleOutcome::Stop(reason) }
}

pub(super) async fn end(cx: &Ctx, cycle: &mut Cycle) -> CycleOutcome {
    let Some(rock) = cycle.target else { return CycleOutcome::Continue };
    let master = cycle.modules[0].id;
    let packet = cycle.inputs.frame.as_mut().map(|frame| std::mem::replace(&mut frame.packet, MasterPacket::None));
    let Some(MasterPacket::Mining(inputs)) = packet else {
        warn!("mining {master}: end without captured mining inputs");
        return CycleOutcome::Stop("mining failed");
    };
    match crate::services::mining::extract(&cx.db, &cx.world, &cx.notify, cx.char_id, cx.ship_id, cx.system_id, inputs, master, cycle.modules[0].type_id, cycle.modules[0].flag, rock).await {
        Ok(extraction) => {
            if let Some(e) = &extraction.error {
                warn!("mining {master}: {}", match e { CallError::Internal(e) => format!("{e:#}"), CallError::User { msg, .. } => msg.clone() });
            }
            extraction_outcome(extraction)
        }
        Err(e) => {
            warn!("mining {master}: {}", match e { CallError::Internal(e) => format!("{e:#}"), CallError::User { msg, .. } => msg });
            CycleOutcome::Stop("mining failed")
        }
    }
}

pub(super) fn stop(s: &mut System, _cx: &Ctx, cycle: &Cycle) {
    s.retire_fx_activation(cycle.modules[0].id, &cycle.stop);
}

#[cfg(all(test, feature = "tests-effects"))] mod tests;
