//! Mining effect validation, animations and yield phase.
//! Client dogma mining effects use the cycle-start fit when extracting ore.
//! Rock storage and depletion are owned by services/mining; later families are unsupported.
use super::lifecycle::{Cycle, CycleOutcome, Module};
use super::notify::fire_fx;
use super::registry::EffectFlag;
use crate::consts::{dogma::ATTR_MAX_RANGE, dogma::EFFECT_MINING_CLOUDS};
use crate::messages;
use crate::services::{combat::{Ctx, targeting_charges::targets_of}, fit::{Fit, Key}, user_error, CallError};
use crate::space::{dist, metres_to_km, System};
use marshal::Value;
use tracing::warn;

fn guid(id: i64) -> &'static str {
    if id == EFFECT_MINING_CLOUDS as i64 { "effects.CloudMining" } else { "effects.Mining" }
}

pub(super) async fn validate(cx: &Ctx, fit: &Fit, module: &Module, target: Option<i64>) -> Result<(), CallError> {
    let Some(t) = target.filter(|t| targets_of(cx.char_id).contains(t)) else {
        return Err(user_error(messages::MSG_DENIED_ACTIVATE_TARGET_NOT_PRESENT, vec![]));
    };
    if !crate::services::mining::is_rock(t) {
        return Err(user_error(messages::MSG_CUSTOM_NOTIFY, vec![("notify".into(), Value::unicode(messages::MINING_MODULE_TARGET_INVALID))]));
    }
    let ship = cx.ship_id;
    let gap = cx.world.with(cx.system_id, move |s| {
        s.ball(ship).zip(s.ball(t)).map(|(a, b)| dist(a.position_at(s.now_ms()), b.position_at(s.now_ms())) - a.radius as f64 - b.radius as f64)
    }).await.flatten();
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
            let (ship, duration, repeat, guid, is_offensive) = (cx.ship_id, cycle.duration_ms, cycle.repeat, guid(cycle.effect.id), cycle.effect.has(EffectFlag::Offensive));
            cx.world.broadcast(cx.system_id, ship, move |stamp| {
                modules.iter().map(|m| fire_fx(stamp, ship, Some((m.id, m.type_id)), guid, Some(rock), None, is_offensive, true, duration, repeat)).collect()
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

pub(super) async fn end(cx: &Ctx, fit: &Fit, cycle: &Cycle) -> CycleOutcome {
    let Some(rock) = cycle.target else { return CycleOutcome::Continue };
    let master = cycle.modules[0].id;
    match crate::services::mining::extract(&cx.db, &cx.world, &cx.notify, cx.char_id, cx.ship_id, cx.system_id, fit, master, cycle.modules[0].type_id, cycle.modules[0].flag, rock).await {
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

pub(super) fn stop(s: &mut System, cx: &Ctx, cycle: &Cycle) {
    let events = cycle.modules.iter().map(|m| fire_fx(s.current_time(), cx.ship_id, Some((m.id, m.type_id)), guid(cycle.effect.id), cycle.target, None, cycle.effect.has(EffectFlag::Offensive), false, cycle.duration_ms, cycle.repeat)).collect();
    s.broadcast(cx.ship_id, events);
}

#[cfg(test)] mod tests;
