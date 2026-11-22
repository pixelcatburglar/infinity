//! Micro jump drive spool completion using destiny SetBallPosition.
//! Client dogma's microJumpDrive.py supplies the signature modifier. A type with
//! no jump-range row uses the client distance; otherwise range and blocking
//! values come from fitted dogma. The micro jump field generator reuses this
//! relocation and guard for its source and passengers. Actor state and the
//! spool are transient.
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
use super::{lifecycle::{Cycle, CycleOutcome, Module}, registry::Effect};
use infinity_core::consts::{dogma::ATTR_ACTIVATION_BLOCKED, dogma::ATTR_WARP_SCRAMBLE_STATUS};
use infinity_core::messages;
use crate::services::{combat::{self, Ctx}, fit::{Fit, Key}, notify_error, CallError};
use crate::space::{add, scale, Ball, System, V3, MODE_WARP};
use crate::lua::events::ModuleCycleOutcome;

/// dogma/const.py: attributeMjdJumpRange, metres.
const ATTR_MJD_JUMP_RANGE: i32 = 2066;

/// appConst.py: microJumpDriveDistance, metres.
const CLIENT_MICRO_JUMP_DISTANCE: f64 = 100_000.0;

/// Client dogma/const.py: attributeMJDBlocked (microJumpBlocked); a positive
/// ship value blocks every micro jump movement.
const ATTR_MICRO_JUMP_BLOCKED: i32 = 5694;

fn resolve_range(type_row: Option<f64>, fitted: impl FnOnce() -> f64) -> Option<f64> {
    let range = if type_row.is_some() { fitted() } else { CLIENT_MICRO_JUMP_DISTANCE };
    (range.is_finite() && range > 0.0).then_some(range)
}

pub(super) fn jump_range(fit: &Fit, has_row: bool, module: i64) -> Option<f64> {
    resolve_range(has_row.then_some(0.0), || fit.value(Key::Module(module), ATTR_MJD_JUMP_RANGE))
}

/// Whether the module type carries an SDE jump-range row; the client distance
/// fallback applies only without one.
pub(super) fn has_range_row(sde: &crate::sde::Sde, type_id: i32) -> bool {
    sde.types.attribute(type_id, ATTR_MJD_JUMP_RANGE).is_some()
}

/// Ship-level micro jump blockers: warp scramble status and the microJumpBlocked
/// dbuff value.
pub(super) fn ship_blocked(fit: &Fit) -> bool {
    fit.value(Key::Ship, ATTR_WARP_SCRAMBLE_STATUS) > 0.0 || fit.value(Key::Ship, ATTR_MICRO_JUMP_BLOCKED) > 0.0
}

pub(super) fn blocked(fit: &Fit, module: i64) -> bool {
    ship_blocked(fit) || fit.value(Key::Module(module), ATTR_ACTIVATION_BLOCKED) > 0.0
}

/// The activation generation still owns the module's active record and has not
/// been stopped.
pub(super) fn activation_live(module: i64, generation: &Arc<AtomicBool>) -> bool {
    combat::state().active.get(&module).is_some_and(|a| Arc::ptr_eq(&a.stop, generation))
        && !generation.load(Ordering::Relaxed)
}

/// Queue one micro jump displacement through the single SetBallPosition
/// authority. The guard applies only when the activation is still live at the
/// destiny boundary, the ball is not warping, its persistent membership is
/// unchanged and `admit` accepts; `report` observes the same verdict.
pub(super) fn queue_jump(s: &mut System, ship: i64, destination: V3, generation: Arc<AtomicBool>, membership: u64,
    admit: impl FnOnce(&Ball, &System) -> bool + Send + 'static, report: impl FnOnce(bool) + Send + 'static,
) {
    s.set_ball_position(ship, destination, move |ball, s| {
        let applied = !generation.load(Ordering::Relaxed) && ball.mode != MODE_WARP
            && s.persistent_generation(ship) == Some(membership)
            && !super::active_cycle::mode_locked(s, ship) && admit(ball, s);
        report(applied);
        applied
    });
}

pub(super) async fn validate(cx: &Ctx, fit: &Fit, module: &Module, _effect: &Effect, _target: Option<i64>) -> Result<(), CallError> {
    if blocked(fit, module.id) { return Err(notify_error(messages::effects::MICRO_JUMP_BLOCKED)); }
    let ship = cx.ship_id;
    if cx.world.with(cx.system_id, move |s| s.ball(ship).is_none_or(|b| b.mode == MODE_WARP)).await.unwrap_or(true) {
        return Err(notify_error(messages::effects::MICRO_JUMP_BLOCKED));
    }
    if jump_range(fit, has_range_row(&cx.world.sde(), module.type_id), module.id).is_none() { return Err(CallError::Internal(anyhow::anyhow!("invalid micro jump range for module {}", module.id))); }
    Ok(())
}

pub(super) async fn start(cx: &Ctx, cycle: &mut Cycle) -> Result<CycleOutcome, CallError> {
    let module = cycle.modules[0].id;
    let ship = cx.ship_id;
    let generation = cycle.stop.clone();
    let registered = cx.world.with(cx.system_id, move |s| {
        activation_live(module, &generation) && s.ball(ship).is_some_and(|b| b.mode != MODE_WARP)
    }).await.unwrap_or(false);
    Ok(if registered { CycleOutcome::Continue } else { CycleOutcome::Stop("source lost") })
}

pub(super) async fn end(cx: &Ctx, cycle: &mut Cycle) -> Result<CycleOutcome, CallError> {
    let module = &cycle.modules[0];
    let ship = cx.ship_id;
    // The type row is immutable SDE; the fitted range and the actual blocker
    // come from the resident fit through the one prepared mutable gate.
    let has_row = has_range_row(&cx.world.sde(), module.type_id);
    let module = module.id;
    let admitted = cycle.source_membership;
    let generation = cycle.stop.clone();
    let (tx, rx) = tokio::sync::oneshot::channel();
    cx.world.with(cx.system_id, move |s| {
        if !activation_live(module, &generation) { return; }
        let Some(range) = s.with_combat_fit(ship, move |fit, _key, stamp| {
            if Some(stamp.membership) != admitted || blocked(fit, module) { return None; }
            jump_range(fit, has_row, module)
        }).flatten() else { return };
        let Some(ball) = s.sampled_ball(ship).filter(|b| b.mode != MODE_WARP) else { return };
        let Some(membership) = s.persistent_generation(ship) else { return };
        let Some(heading) = s.heading(ship) else { return };
        let destination = add(ball.pos, scale(heading, range));
        queue_jump(s, ship, destination, generation, membership, |_, _| true, move |ok| { let _ = tx.send(ok); });
    }).await;
    Ok(if rx.await.unwrap_or(false) {
        CycleOutcome::Ended { reason: "micro jump completed", outcome: ModuleCycleOutcome::Completed }
    } else { CycleOutcome::Stop("micro jump completed") })
}

#[cfg(all(test, feature = "tests-effects"))]
mod tests;
