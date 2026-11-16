//! Micro jump drive spool completion using destiny SetBallPosition.
//! Client dogma's microJumpDrive.py supplies the signature modifier. A type with
//! no jump-range row uses the client distance; otherwise range and blocking
//! values come from fitted dogma. Actor state and the spool are transient.
use std::sync::{Arc, atomic::Ordering};
use super::{lifecycle::{Cycle, CycleOutcome, Module}, registry::Effect};
use infinity_core::consts::{dogma::ATTR_ACTIVATION_BLOCKED, dogma::ATTR_WARP_SCRAMBLE_STATUS};
use infinity_core::messages;
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

fn jump_range(fit: &Fit, has_row: bool, module: i64) -> Option<f64> {
    resolve_range(has_row.then_some(0.0), || fit.value(Key::Module(module), ATTR_MJD_JUMP_RANGE))
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
    if jump_range(fit, cx.world.sde().types.attribute(module.type_id, ATTR_MJD_JUMP_RANGE).is_some(), module.id).is_none() { return Err(CallError::Internal(anyhow::anyhow!("invalid micro jump range for module {}", module.id))); }
    Ok(())
}

pub(super) async fn start(cx: &Ctx, cycle: &mut Cycle) -> Result<CycleOutcome, CallError> {
    let module = cycle.modules[0].id;
    let ship = cx.ship_id;
    let generation = cycle.stop.clone();
    let registered = cx.world.with(cx.system_id, move |s| {
        let current = combat::state().active.get(&module).is_some_and(|a| Arc::ptr_eq(&a.stop, &generation));
        current && !generation.load(Ordering::Relaxed) && s.ball(ship).is_some_and(|b| b.mode != MODE_WARP)
    }).await.unwrap_or(false);
    Ok(if registered { CycleOutcome::Continue } else { CycleOutcome::Stop("source lost") })
}

pub(super) async fn end(cx: &Ctx, cycle: &mut Cycle) -> Result<CycleOutcome, CallError> {
    let module = &cycle.modules[0];
    let ship = cx.ship_id;
    // The type row is immutable SDE; the fitted range and the actual blocker
    // come from the resident fit through the one prepared mutable gate.
    let has_row = cx.world.sde().types.attribute(module.type_id, ATTR_MJD_JUMP_RANGE).is_some();
    let module = module.id;
    let admitted = cycle.source_membership;
    let generation = cycle.stop.clone();
    let (tx, rx) = tokio::sync::oneshot::channel();
    cx.world.with(cx.system_id, move |s| {
        let current = combat::state().active.get(&module).is_some_and(|a| Arc::ptr_eq(&a.stop, &generation));
        if !current || generation.load(Ordering::Relaxed) { return; }
        let Some(range) = s.with_combat_fit(ship, move |fit, _key, stamp| {
            if Some(stamp.membership) != admitted || blocked(fit, module) { return None; }
            jump_range(fit, has_row, module)
        }).flatten() else { return };
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

#[cfg(all(test, feature = "tests-effects"))]
mod tests;
