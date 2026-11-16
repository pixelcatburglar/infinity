//! Tractor beam cycles for inventory-backed wrecks and cargo containers.
//! Destiny FOLLOW and movement restoration are owned by the system actor; target
//! existence and positioning come from game.items. Loot-right restrictions are not
//! modelled, matching the inventory service's existing access policy.
//! Tractor beams follow to surface contact; the client supplies no stand-off distance.
//! Effect animations are published through the system's ordered scene channel.
use super::{lifecycle::{cycle_subject_valid, source_subject_current, Cycle, CycleOutcome, Module}, parameters::MasterPacket, registry::{Effect, EffectAttribute, EffectFlag}};
use infinity_core::consts::{dogma::ATTR_MAX_RANGE, CARGO_CONTAINER_GROUPS, GROUP_WRECK};
use crate::dao::items::loading::ItemQuery;
use infinity_core::messages;
use crate::services::{combat::{Ctx, targeting_charges::targets_of}, fit::{Fit, Key}, items::load_items, user_error, db_err, CallError};
use crate::space::{dist, FxModule, System, MODE_WARP, system::presentation::{FxRetention, ModuleFx}};
use marshal::Value;


fn container_group(group: i64) -> bool {
    group == i64::from(GROUP_WRECK) || i32::try_from(group).ok().is_some_and(|id| CARGO_CONTAINER_GROUPS.contains(&id))
}

fn in_range(s: &System, source: i64, target: i64, range: f64) -> bool {
    if !range.is_finite() || range < 0.0 || source == target { return false; }
    let Some((a, b)) = s.sampled_ball(source).zip(s.sampled_ball(target)) else { return false; };
    a.mode != MODE_WARP && b.mode != MODE_WARP && (dist(a.pos, b.pos) - f64::from(a.radius) - f64::from(b.radius)).max(0.0) <= range
}

async fn target_exists(cx: &Ctx, target: i64) -> Result<bool, CallError> {
    let item = load_items(&cx.db, cx.world.sde(), ItemQuery::Id(target)).await.map_err(db_err)?.into_iter().next();
    if !item.is_some_and(|item| container_group(item.group_id) && item.location_id == cx.system_id) { return Ok(false); }
    Ok(crate::dao::items::social::position(&cx.db, target).await.map_err(db_err)?.is_some_and(|p| p.location_id == cx.system_id))
}

pub(super) fn range(fit: &Fit, module: &Module, effect: &Effect) -> f64 {
    fit.value(Key::Module(module.id), effect.attribute(EffectAttribute::Range).unwrap_or(ATTR_MAX_RANGE))
}

pub(super) async fn validate(cx: &Ctx, fit: &Fit, module: &Module, effect: &Effect, target: Option<i64>) -> Result<(), CallError> {
    let refuse = || user_error(messages::MSG_DENIED_ACTIVATE_TARGET_NOT_PRESENT, vec![]);
    let target = target.filter(|id| targets_of(cx.char_id).contains(id)).ok_or_else(refuse)?;
    if !target_exists(cx, target).await? { return Err(refuse()); }
    let (source, range) = (cx.ship_id, range(fit, module, effect));
    if !cx.world.with(cx.system_id, move |s| in_range(s, source, target, range)).await.unwrap_or(false) { return Err(refuse()); }
    Ok(())
}

pub(super) async fn start(cx: &Ctx, cycle: &mut Cycle) -> Result<CycleOutcome, CallError> {
    if !cycle_subject_valid(cycle).await { return Ok(CycleOutcome::Stop("target lost")); }
    let Some(MasterPacket::Tractor(inputs)) = cycle.inputs.frame.as_ref().map(|frame| &frame.packet) else {
        return Err(CallError::Internal(anyhow::anyhow!("tractor cycle captured without tractor inputs")));
    };
    let (range, velocity) = (inputs.range, inputs.velocity);
    let Some(target) = cycle.target.filter(|id| targets_of(cx.char_id).contains(id)) else {
        return Ok(super::lifecycle::target_loss(cycle).await);
    };
    if !target_exists(cx, target).await? { return Ok(super::lifecycle::target_loss(cycle).await); }
    let source = cx.ship_id;
    if !cx.world.with(cx.system_id, move |s| in_range(s, source, target, range)).await.unwrap_or(false) {
        return Ok(super::lifecycle::target_loss(cycle).await);
    }
    if !velocity.is_finite() || velocity <= 0.0 { return Ok(CycleOutcome::Stop("invalid tractor velocity")); }
    if cycle.cycles == 0 {
        let Some(membership) = cycle.source_membership else { return Ok(CycleOutcome::Stop("target lost")) };
        let (master, generation, module, effect_id, character) =
            (cycle.modules[0].id, cycle.stop.clone(), cycle.modules[0].clone(), cycle.effect.id, cx.char_id);
        let modules = cycle.modules.clone();
        let guid = cycle.effect.guid.as_deref().and_then(|g| g.split(',').next()).map(str::to_owned);
        let (duration, repeat, offensive) = (cycle.duration_ms, cycle.repeat, cycle.effect.has(EffectFlag::Offensive));
        let accepted = cx.world.with(cx.system_id, move |s| {
            if !source_subject_current(s, source, character, membership, &module, effect_id, &generation)
                || !targets_of(character).contains(&target) || !in_range(s, source, target, range) { return false; }
            if !s.begin_tractor(target, master, generation.clone(), velocity, 0.0, source) { return false; }
            if let Some(guid) = guid {
                for m in &modules {
                    s.publish_module_fx(master, &generation, ModuleFx {
                        module: FxModule { item: Some(m.id), type_id: m.type_id },
                        guid: guid.clone(), charge: None, offensive,
                        duration_ms: duration, repeat, graphic_info: Value::None,
                    }, true, FxRetention::Current);
                }
            }
            true
        }).await.unwrap_or(false);
        if !accepted { return Ok(CycleOutcome::Stop("tractor unavailable")); }
    }
    Ok(CycleOutcome::Continue)
}

pub(super) fn stop(s: &mut System, _cx: &Ctx, cycle: &Cycle) {
    if let Some(target) = cycle.target { s.end_tractor(target, cycle.modules[0].id, &cycle.stop); }
    s.retire_fx_activation(cycle.modules[0].id, &cycle.stop);
}

#[cfg(all(test, feature = "tests-effects"))] mod tests;
