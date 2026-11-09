//! Vorton arcs using fitted dogma and the shared turret damage pipeline.
//! Mirrors turrets.py and brawlChainWeaponEffect.py; arc range and count come
//! from SDE dogma. Victims are sampled per cycle; effects are transient.
use super::lifecycle::{Cycle, CycleOutcome, Module};
use super::registry::{Effect, EffectFlag};
use super::{notify, weapons};
use infinity_core::consts::{CATEGORY_DRONE, CATEGORY_ENTITY, CATEGORY_SHIP};
use infinity_core::messages;
use crate::services::combat::{self, Ctx, targeting_charges::{loaded, targets_of}};
use crate::services::fit::{Fit, Key};
use crate::services::{user_error, CallError};
use crate::space::{dist, System, MODE_WARP};
use marshal::Value;
use std::sync::{Arc, atomic::AtomicBool};

/// SDE dogma VortonArcRange: distance from the original target.
const ATTR_VORTON_ARC_RANGE: i32 = 3036;
/// SDE dogma VortonArcTargets: maximum number of targets including the primary.
const ATTR_VORTON_ARC_TARGETS: i32 = 3037;
/// Client UserError type-ID substitution tag.
const UE_TYPEID: i64 = 4;

fn secondary_count(total: f64) -> usize {
    if total.is_finite() && total > 0.0 { (total as usize).saturating_sub(1) } else { 0 }
}

fn generation_valid(master: i64, generation: &Arc<AtomicBool>) -> bool {
    combat::state().active.get(&master).is_some_and(|active| Arc::ptr_eq(&active.stop, generation))
}

fn select_targets(s: &System, sde: &crate::sde::Sde, source: i64, primary: i64, range: f64, count: usize) -> Option<Vec<(i64, Option<i64>)>> {
    let center = s.sample_position(primary)?;
    if !range.is_finite() || range < 0.0 { return Some(Vec::new()); }
    let mut candidates: Vec<_> = s.balls().filter(|b| b.id != source && b.id != primary && b.cloak == 0)
        .filter(|b| sde.types.get(b.type_id).and_then(|t| t.category_id)
            .is_some_and(|c| c == CATEGORY_SHIP || c == CATEGORY_DRONE || c == CATEGORY_ENTITY))
        .filter_map(|b| {
            let distance = dist(center, s.sample_position(b.id)?);
            (distance.is_finite() && distance <= range).then_some((distance, b.id, b.char_id))
        }).collect();
    candidates.sort_unstable_by(|a, b| a.0.total_cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    Some(candidates.into_iter().take(count).map(|(_, id, pilot)| (id, pilot)).collect())
}

pub(super) async fn validate(cx: &Ctx, _fit: &Fit, module: &Module, _effect: &Effect, target: Option<i64>) -> Result<(), CallError> {
    let Some(target) = target.filter(|t| targets_of(cx.char_id).contains(t)) else {
        return Err(user_error(messages::MSG_DENIED_ACTIVATE_TARGET_NOT_PRESENT, vec![]));
    };
    let ship = cx.ship_id;
    let (in_warp, present) = cx.world.with(cx.system_id, move |s| {
        (s.ball(ship).is_some_and(|b| b.mode == MODE_WARP), s.ball(target).is_some())
    }).await.unwrap_or((false, false));
    if in_warp { return Err(user_error(messages::MSG_DENIED_ACTIVATE_IN_WARP, vec![])); }
    if !present { return Err(user_error(messages::MSG_DENIED_ACTIVATE_TARGET_NOT_PRESENT, vec![])); }
    if loaded(&cx.db, cx.ship_id, module.flag).await?.is_none_or(|(_, q)| q <= 0) {
        return Err(user_error(messages::MSG_NO_CHARGES, vec![("launcher".into(), Value::tuple([Value::Int(UE_TYPEID), Value::Int(module.type_id as i64)]))]));
    }
    Ok(())
}

pub(super) async fn start(cx: &Ctx, fit: &Fit, cycle: &mut Cycle) -> Result<CycleOutcome, CallError> {
    let Some(primary) = cycle.target.filter(|t| targets_of(cx.char_id).contains(t)) else { return Ok(super::lifecycle::target_loss(cycle).await); };
    let master = cycle.modules[0].id;
    let generation = cycle.stop.clone();
    let mut fired = false;
    for index in 0..cycle.modules.len() {
        let module = cycle.modules[index].clone();
        let Some(attempt) = super::lifecycle::prepare_combat_source(cycle, module.id, Some(module.type_id)).await? else {
            return Ok(CycleOutcome::Stop("source lost or activation replaced"));
        };
        if !generation_valid(master, &generation) { return Ok(CycleOutcome::Stop("activation replaced")); }
        let key = Key::Module(module.id);
        let range = fit.value(key, ATTR_VORTON_ARC_RANGE);
        let count = secondary_count(fit.value(key, ATTR_VORTON_ARC_TARGETS));
        let source = cx.ship_id;
        let sde = cx.world.sde().clone();
        let check = generation.clone();
        let Some((targets, victims)) = cx.world.with(cx.system_id, move |s| {
            if !generation_valid(master, &check) { return None; }
            let ship = s.sampled_ball(source)?;
            if ship.mode == MODE_WARP { return None; }
            let target = s.sampled_ball(primary)?;
            let targets = select_targets(s, &sde, source, primary, range, count)?;
            let victims = std::iter::once((primary, target.char_id)).chain(targets.iter().copied())
                .map(|(id, pilot)| (id, pilot, s.combat_membership_generation(id), false, None)).collect();
            Some((targets, victims))
        }).await.flatten() else { return Ok(CycleOutcome::Stop("source or target lost")); };
        let Some(charge) = weapons::consume_charge(cx, &module, fit.source_type(Key::Charge(module.id))).await? else { continue; };
        cycle.charge_type = Some(charge);
        fired = true;
        let (parameters, raw) = weapons::turret_parameters(fit, &module);
        super::aoe::apply_damage_guarded(&cx.db, &cx.world, cx.system_id, module.type_id, victims, raw,
            Some(super::aoe::DamageGuard::Player(master, generation.clone())), attempt, Some(parameters)).await?;
        if let Some(guid) = cycle.effect.guid.as_deref().and_then(|g| g.split(',').next()).map(str::to_owned) {
            let info = Value::dict([("targets", Value::list(targets.iter().map(|(id, _)| Value::Int(*id))))]);
            let (id, type_id, duration, repeat, offensive) = (module.id, module.type_id, cycle.duration_ms, cycle.repeat, cycle.effect.has(EffectFlag::Offensive));
            cx.world.broadcast(cx.system_id, source, move |stamp| vec![notify::fire_fx_with_info(stamp, source, Some((id, type_id)), &guid, Some(primary), Some(charge), offensive, true, duration, repeat, info.clone())]);
        }
    }
    if fired { Ok(CycleOutcome::Continue) } else { Ok(CycleOutcome::Stop("out of charges")) }
}

#[cfg(test)] mod tests;
