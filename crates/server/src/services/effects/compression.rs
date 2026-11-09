//! Industrial compression facility markers advertised on the ship slim item.
//! The in-space compression client reads compressibleItemsTypeList and maxRange;
//! both are fitted dogma attributes. Facility markers are transient actor state.
use std::sync::Arc;
use super::lifecycle::{Cycle, CycleOutcome, Module};
use super::registry::Effect;
use infinity_core::consts::{dogma::ATTR_COMPRESSIBLE_ITEMS_TYPE_LIST, dogma::ATTR_MAX_RANGE};
use crate::services::{combat::{self, Ctx}, fit::{Fit, Key}, CallError};
use crate::space::System;

fn values(fit: &Fit, module: i64) -> Result<(i64, f64), CallError> {
    let list = fit.value(Key::Module(module), ATTR_COMPRESSIBLE_ITEMS_TYPE_LIST);
    let range = fit.value(Key::Module(module), ATTR_MAX_RANGE);
    if !list.is_finite() || list <= 0.0 || list.fract() != 0.0 || list > i64::MAX as f64 || !range.is_finite() || range <= 0.0 {
        return Err(CallError::Internal(anyhow::anyhow!("invalid compression facility attributes for module {module}")));
    }
    Ok((list as i64, range))
}

pub(super) async fn validate(_cx: &Ctx, fit: &Fit, module: &Module, _effect: &Effect, _target: Option<i64>) -> Result<(), CallError> {
    values(fit, module.id)?;
    Ok(())
}

pub(super) async fn start(cx: &Ctx, fit: &Fit, cycle: &mut Cycle) -> Result<CycleOutcome, CallError> {
    let module = cycle.modules[0].id;
    let (list, range) = values(fit, module)?;
    let ship = cx.ship_id;
    let generation = cycle.stop.clone();
    let active = cx.world.with(cx.system_id, move |s| {
        let state = combat::state();
        if !state.active.get(&module).is_some_and(|active| Arc::ptr_eq(&active.stop, &generation)) || !s.contains(ship) { return false; }
        s.set_compressor(ship, module, generation.clone(), list, range);
        true
    }).await.unwrap_or(false);
    Ok(if active { CycleOutcome::Continue } else { CycleOutcome::Stop("source lost") })
}

pub(super) fn stop(s: &mut System, cx: &Ctx, cycle: &Cycle) {
    s.remove_compressor(cx.ship_id, cycle.modules[0].id, &cycle.stop);
}
