//! Micro jump drive spool completion using destiny SetBallPosition.
//! Client dogma's microJumpDrive.py supplies the signature modifier. A type with
//! no jump-range row uses the client distance; otherwise range and blocking
//! values come from fitted dogma. Actor state and the spool are transient.
use std::sync::{Arc, atomic::Ordering};
use super::{lifecycle::{Cycle, CycleOutcome, Module}, registry::Effect};
use crate::consts::{dogma::ATTR_ACTIVATION_BLOCKED, dogma::ATTR_WARP_SCRAMBLE_STATUS};
use crate::messages;
use crate::services::{combat::{self, Ctx}, fit::{Fit, Key}, notify_error, CallError};
use crate::space::{add, scale, MODE_WARP};
use crate::lua::events::ModuleCycleOutcome;

/// dogma/const.py: attributeMjdJumpRange, metres.
const ATTR_MJD_JUMP_RANGE: i32 = 2066;

/// appConst.py: microJumpDriveDistance, metres.
const CLIENT_MICRO_JUMP_DISTANCE: f64 = 100_000.0;

fn resolve_range(type_row: Option<f64>, fitted: impl FnOnce() -> f64) -> Option<f64> {
    let range = if type_row.is_some() { fitted() } else { CLIENT_MICRO_JUMP_DISTANCE };
    (range.is_finite() && range > 0.0).then_some(range)
}

fn jump_range(cx: &Ctx, fit: &Fit, module: &Module) -> Option<f64> {
    let row = cx.world.sde().types.attribute(module.type_id, ATTR_MJD_JUMP_RANGE);
    resolve_range(row, || fit.value(Key::Module(module.id), ATTR_MJD_JUMP_RANGE))
}

fn blocked(fit: &Fit, module: i64) -> bool {
    fit.value(Key::Ship, ATTR_WARP_SCRAMBLE_STATUS) > 0.0 || fit.value(Key::Module(module), ATTR_ACTIVATION_BLOCKED) > 0.0
}

pub(super) async fn validate(cx: &Ctx, fit: &Fit, module: &Module, _effect: &Effect, _target: Option<i64>) -> Result<(), CallError> {
    if blocked(fit, module.id) { return Err(notify_error(messages::effects::MICRO_JUMP_BLOCKED)); }
    let ship = cx.ship_id;
    if cx.world.with(cx.system_id, move |s| s.ball(ship).is_none_or(|b| b.mode == MODE_WARP)).await.unwrap_or(true) {
        return Err(notify_error(messages::effects::MICRO_JUMP_BLOCKED));
    }
    if jump_range(cx, fit, module).is_none() { return Err(CallError::Internal(anyhow::anyhow!("invalid micro jump range for module {}", module.id))); }
    Ok(())
}

pub(super) async fn start(cx: &Ctx, _fit: &Fit, cycle: &mut Cycle) -> Result<CycleOutcome, CallError> {
    let module = cycle.modules[0].id;
    let ship = cx.ship_id;
    let generation = cycle.stop.clone();
    let registered = cx.world.with(cx.system_id, move |s| {
        let current = combat::state().active.get(&module).is_some_and(|a| Arc::ptr_eq(&a.stop, &generation));
        current && !generation.load(Ordering::Relaxed) && s.ball(ship).is_some_and(|b| b.mode != MODE_WARP)
    }).await.unwrap_or(false);
    Ok(if registered { CycleOutcome::Continue } else { CycleOutcome::Stop("source lost") })
}

pub(super) async fn end(cx: &Ctx, _fit: &Fit, cycle: &mut Cycle) -> Result<CycleOutcome, CallError> {
    let module = &cycle.modules[0];
    let ship = cx.ship_id;
    let fit = Fit::load_in_space(&cx.db, &cx.world, cx.system_id, cx.char_id, ship).await?;
    let range = jump_range(cx, &fit, module);
    let module = module.id;
    let Some(revision) = fit.projection_revision() else { return Ok(CycleOutcome::Stop("fit unavailable")); };
    let Some(range) = range.filter(|_| !blocked(&fit, module)) else { return Ok(CycleOutcome::Stop("micro jump blocked")); };
    let generation = cycle.stop.clone();
    let (tx, rx) = tokio::sync::oneshot::channel();
    cx.world.with(cx.system_id, move |s| {
        let current = combat::state().active.get(&module).is_some_and(|a| Arc::ptr_eq(&a.stop, &generation));
        if !current || generation.load(Ordering::Relaxed) || s.projection_revision(ship) != revision { return; }
        let Some(ball) = s.sampled_ball(ship).filter(|b| b.mode != MODE_WARP) else { return };
        let Some(membership) = s.persistent_generation(ship) else { return };
        let Some(heading) = s.heading(ship) else { return };
        let destination = add(ball.pos, scale(heading, range));
        s.set_ball_position(ship, destination, move |ball, s| {
            let applied = !generation.load(Ordering::Relaxed) && ball.mode != MODE_WARP
                && s.persistent_generation(ship) == Some(membership);
            let _ = tx.send(applied);
            applied
        });
    }).await;
    Ok(if rx.await.unwrap_or(false) {
        CycleOutcome::Ended { reason: "micro jump completed", outcome: ModuleCycleOutcome::Completed }
    } else { CycleOutcome::Stop("micro jump completed") })
}

#[cfg(test)]
mod tests;
