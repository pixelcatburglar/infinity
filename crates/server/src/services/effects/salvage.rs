//! Salvager cycles and the shared wreck recovery settlement in `recovery`.
//! The client dogma salvaging effect uses fitted accessDifficultyBonus and the
//! wreck's SDE accessDifficulty. A cycle's per-phase wreck check runs on the
//! system ball and SDE data only; the durable root claim happens once, inside
//! the shared settlement. Effect animations are published through the system's
//! ordered scene channel.
use super::{lifecycle::{Cycle, CycleOutcome, Module}, parameters::MasterPacket, registry::{Effect, EffectAttribute, EffectFlag}};
use infinity_core::consts::{dogma::ATTR_MAX_RANGE, GROUP_WRECK};
use infinity_core::messages;
use crate::services::{combat::{Ctx, targeting_charges::targets_of}, fit::{Fit, Key}, rand01, user_error, CallError};
use crate::space::{dist, Ball, FxModule, System, MODE_WARP, system::presentation::{FxRetention, ModuleFx}};
use marshal::Value;
use crate::lua::events::ModuleCycleOutcome;
pub(in crate::services) mod recovery;

fn range(fit: &Fit, module: &Module, effect: &Effect) -> f64 {
    fit.value(Key::Module(module.id), effect.attribute(EffectAttribute::Range).unwrap_or(ATTR_MAX_RANGE))
}

fn target_ball(s: &System, source: i64, target: i64, range: f64) -> Option<Ball> {
    if source == target || !range.is_finite() || range < 0.0 { return None; }
    let (a, b) = s.sampled_ball(source).zip(s.sampled_ball(target))?;
    if a.mode == MODE_WARP || b.mode == MODE_WARP || (dist(a.pos, b.pos) - f64::from(a.radius) - f64::from(b.radius)).max(0.0) > range { return None; }
    Some(b)
}

pub(super) async fn validate(cx: &Ctx, fit: &Fit, module: &Module, effect: &Effect, target: Option<i64>) -> Result<(), CallError> {
    check(cx, range(fit, module, effect), target).await
}

/// The per-phase wreck gate: a held target lock on a visible wreck ball of the
/// right group inside action range, all read from actor state and the SDE.
async fn check(cx: &Ctx, range: f64, target: Option<i64>) -> Result<(), CallError> {
    let refuse = || user_error(messages::MSG_DENIED_ACTIVATE_TARGET_NOT_PRESENT, vec![]);
    let target = target.filter(|id| targets_of(cx.char_id).contains(id)).ok_or_else(refuse)?;
    let sde = cx.world.sde().clone();
    let source = cx.ship_id;
    let valid = cx.world.with(cx.system_id, move |s| {
        s.sampled_ball(target).is_some_and(|ball| sde.types.get(ball.type_id).and_then(|t| t.group_id) == Some(GROUP_WRECK))
            && target_ball(s, source, target, range).is_some()
    }).await.unwrap_or(false);
    if !valid { return Err(refuse()); }
    Ok(())
}

pub(super) async fn start(cx: &Ctx, cycle: &mut Cycle) -> Result<CycleOutcome, CallError> {
    if !super::lifecycle::cycle_subject_valid(cycle).await { return Ok(CycleOutcome::Stop("target lost")); }
    let Some(range) = cycle.inputs.frame.as_ref().and_then(|frame| match &frame.packet {
        MasterPacket::Salvage(inputs) => Some(inputs.range),
        _ => None,
    }) else {
        return Err(CallError::Internal(anyhow::anyhow!("salvage cycle captured without salvage inputs")));
    };
    if check(cx, range, cycle.target).await.is_err() { return Ok(super::lifecycle::target_loss(cycle).await); }
    if cycle.cycles == 0 {
        if let Some(guid) = cycle.effect.guid.as_deref().and_then(|g| g.split(',').next()).map(str::to_owned) {
            let (modules, master, generation, duration, repeat, offensive) = (cycle.modules.clone(), cycle.modules[0].id, cycle.stop.clone(), cycle.duration_ms, cycle.repeat, cycle.effect.has(EffectFlag::Offensive));
            cx.world.post(cx.system_id, move |s| {
                for m in &modules {
                    s.publish_module_fx(master, &generation, ModuleFx {
                        module: FxModule { item: Some(m.id), type_id: m.type_id },
                        guid: guid.clone(), charge: None, offensive,
                        duration_ms: duration, repeat, graphic_info: Value::None,
                    }, true, FxRetention::Current);
                }
            });
        }
    }
    Ok(CycleOutcome::Continue)
}

pub(super) async fn end(cx: &Ctx, cycle: &mut Cycle) -> Result<CycleOutcome, CallError> {
    let Some(target) = cycle.target else { return Ok(CycleOutcome::Stop("target lost")); };
    if !super::lifecycle::cycle_subject_valid(cycle).await { return Ok(CycleOutcome::Stop("target lost")); }
    let Some((range, bonus)) = cycle.inputs.frame.as_ref().and_then(|frame| match &frame.packet {
        MasterPacket::Salvage(inputs) => Some((inputs.range, inputs.bonus)),
        _ => None,
    }) else {
        return Err(CallError::Internal(anyhow::anyhow!("salvage cycle captured without salvage inputs")));
    };
    if check(cx, range, Some(target)).await.is_err() { return Ok(super::lifecycle::target_loss(cycle).await); }
    let (source, character, master, generation) = (cx.ship_id, cx.char_id, cycle.modules[0].id, cycle.stop.clone());
    let Some(membership) = cycle.source_membership else { return Ok(CycleOutcome::Stop("target lost")); };
    let module = cycle.modules[0].clone();
    let effect = cycle.presented_effect;
    let generation_check = generation.clone();
    let ball = cx.world.with(cx.system_id, move |s| {
        let subject = super::lifecycle::source_subject_current(s, source, character, membership, &module, effect, &generation_check)
            && targets_of(character).contains(&target);
        if !subject { return None; }
        target_ball(s, source, target, range)
    }).await.flatten();
    let Some(ball) = ball else { return Ok(super::lifecycle::target_loss(cycle).await); };
    let sde = cx.world.sde().clone();
    let Some(difficulty) = recovery::difficulty(&sde, ball.type_id) else { return Ok(CycleOutcome::Stop("wreck difficulty missing")); };
    // A parked plan of this same activation already holds a frozen success
    // draw: resume it instead of rolling chance or yields again.
    let attempt = crate::space::system::npc_wrecks::RecoveryAttempt::Module { master, stop: generation.clone() };
    let yields = match recovery::parked_yields(&cx.world, cx.system_id, target, character, source, source, source, range, attempt).await {
        Some(yields) => yields,
        None => {
            if rand01() >= recovery::chance(bonus, difficulty) { return Ok(CycleOutcome::Continue); }
            recovery::select_yields(&sde)?
        }
    };
    let capacity = match cycle.inputs.frame.as_mut().and_then(|frame| match &mut frame.packet {
        MasterPacket::Salvage(inputs) => Some(&mut inputs.capacity),
        _ => None,
    }) {
        Some(Ok(capacity)) => *capacity,
        Some(Err(error)) => return Err(std::mem::replace(error, CallError::Internal(anyhow::anyhow!("salvage capacity already consumed")))),
        None => return Err(CallError::Internal(anyhow::anyhow!("salvage cycle captured without salvage inputs"))),
    };
    let request = recovery::RecoveryRequest {
        system: cx.system_id,
        character,
        ship: source,
        wreck: ball,
        range,
        authority: recovery::RecoveryAuthority::Module { master, generation, membership, presented_effect: cycle.presented_effect },
        destination: recovery::RewardDestination::Ship { ship: source, capacity },
        yields,
    };
    Ok(match recovery::settle(&cx.db, &cx.world, request, Some(cx.notify.clone())).await? {
        recovery::RecoveryOutcome::Recovered => CycleOutcome::Ended { reason: "salvage complete", outcome: ModuleCycleOutcome::Completed },
        recovery::RecoveryOutcome::Unavailable => CycleOutcome::Stop("salvage unavailable"),
        // A deferred attempt carries its own error: report it and stop this
        // activation loop without rolling another draw.
        recovery::RecoveryOutcome::Deferred { error, .. } => return Err(error),
    })
}

pub(super) fn stop(s: &mut System, _cx: &Ctx, cycle: &Cycle) {
    s.retire_fx_activation(cycle.modules[0].id, &cycle.stop);
}

#[cfg(all(test, feature = "tests-effects"))] mod tests;
