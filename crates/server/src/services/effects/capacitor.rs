//! Capacitor boosters, neutralizers, nosferatu and remote transfers.
//! Mirrors dogma/const.py and client dogma activation: fitted attributes and SDE
//! effect metadata determine amounts and falloff. Capacitor state and recharge are
//! owned by combat; assistance Crimewatch propagation is not yet modelled.
use super::lifecycle::{Cycle, CycleOutcome, Module};
use super::registry::{Effect, EffectAttribute, EffectFlag};
use super::{range::range_multiplier, weapons::consume_charge, EffectKind};
use infinity_core::consts::{dogma::ATTR_CAPACITOR_BONUS, dogma::ATTR_ENERGY_NEUTRALIZER_AMOUNT, dogma::ATTR_NOS_OVERRIDE, dogma::ATTR_POWER_TRANSFER_AMOUNT, CATEGORY_DRONE, CATEGORY_ENTITY, CATEGORY_SHIP};
use infinity_core::messages;
use crate::services::combat::{Ctx, groups_health::{capacitor_snapshot, drain_exact_guarded, inject, inject_guarded, NpcCapacitorGuard}, targeting_charges::{loaded, targets_of}};
use crate::services::fit::{Fit, Key};
use crate::services::{user_error, CallError};
use crate::space::{dist, MODE_WARP};
use marshal::Value;

/// Client UserError type-ID substitution tag.
const UE_TYPEID: i64 = 4;

fn target_error() -> CallError {
    user_error(messages::MSG_DENIED_ACTIVATE_TARGET_NOT_PRESENT, vec![])
}

/// Sample both balls together so the falloff uses one actor-time position.
async fn target_distance(cx: &Ctx, target: Option<i64>) -> Result<(f64, bool), CallError> {
    let target = target.filter(|&id| id != cx.ship_id && targets_of(cx.char_id).contains(&id)).ok_or_else(target_error)?;
    let source = cx.ship_id;
    let Some((ship, victim, npc, drone)) = cx.world.with(cx.system_id, move |s| Some((s.sampled_ball(source)?, s.sampled_ball(target)?, s.is_npc(target), s.drone(target).is_some()))).await.flatten() else {
        return Err(target_error());
    };
    if ship.mode == MODE_WARP || victim.mode == MODE_WARP {
        return Err(user_error(messages::MSG_DENIED_ACTIVATE_IN_WARP, vec![]));
    }
    let category = cx.world.sde().types.get(victim.type_id).and_then(|t| t.category_id);
    if category != Some(CATEGORY_SHIP) && category != Some(CATEGORY_DRONE) && !(category == Some(CATEGORY_ENTITY) && npc) { return Err(target_error()); }
    Ok(((dist(ship.pos, victim.pos) - f64::from(ship.radius) - f64::from(victim.radius)).max(0.0), drone))
}

pub(super) async fn validate(cx: &Ctx, _fit: &Fit, module: &Module, effect: &Effect, target: Option<i64>) -> Result<(), CallError> {
    if effect.kind == EffectKind::CapBooster {
        let ship = cx.ship_id;
        let mode = cx.world.with(cx.system_id, move |s| s.ball(ship).map(|b| b.mode)).await.flatten().ok_or_else(target_error)?;
        if mode == MODE_WARP { return Err(user_error(messages::MSG_DENIED_ACTIVATE_IN_WARP, vec![])); }
        if loaded(&cx.db, cx.ship_id, module.flag).await?.is_none_or(|(_, quantity)| quantity <= 0) {
            return Err(user_error(messages::MSG_NO_CHARGES, vec![("launcher".into(), Value::tuple([Value::Int(UE_TYPEID), Value::Int(i64::from(module.type_id))]))]));
        }
    } else {
        target_distance(cx, target).await?;
    }
    Ok(())
}

pub(super) fn nos_allowed(source_level: f64, source_capacity: f64, target_level: f64, target_capacity: f64, override_enabled: bool) -> bool {
    override_enabled || (source_capacity > 0.0 && target_capacity > 0.0 && target_level / target_capacity > source_level / source_capacity)
}

pub(super) async fn start(cx: &Ctx, fit: &Fit, cycle: &mut Cycle) -> Result<CycleOutcome, CallError> {
    let module = &cycle.modules[0];
    if cycle.effect.kind == EffectKind::CapBooster {
        let ship = cx.ship_id;
        if cx.world.with(cx.system_id, move |s| s.ball(ship).is_some_and(|b| b.mode != MODE_WARP)).await != Some(true) {
            return Ok(CycleOutcome::Stop("source lost or in warp"));
        }
        let amount = fit.value(Key::Charge(module.id), ATTR_CAPACITOR_BONUS).max(0.0);
        let Some(charge) = consume_charge(cx, module, cycle.charge_type).await? else { return Ok(CycleOutcome::Stop("out of charges")); };
        cycle.charge_type = Some(charge);
        inject(&cx.db, &cx.world, cx.system_id, cx.ship_id, amount).await?;
        return Ok(CycleOutcome::Continue);
    }
    let (distance, target_drone) = match target_distance(cx, cycle.target).await {
        Ok(distance) => distance,
        Err(CallError::User { .. }) => return Ok(super::lifecycle::target_loss(cycle).await),
        Err(error) => return Err(error),
    };
    let target = cycle.target.expect("validated target");
    let key = Key::Module(module.id);
    let optimal = cycle.effect.attribute(EffectAttribute::Range).map(|attr| fit.value(key, attr)).unwrap_or(0.0);
    let falloff = cycle.effect.attribute(EffectAttribute::Falloff).map(|attr| fit.value(key, attr)).unwrap_or(0.0);
    let scale = range_multiplier(distance, optimal, falloff);
    let guard = NpcCapacitorGuard::Attack {
        source: cx.ship_id, target, source_guard: None,
        player: Some((cx.char_id, module.id, cycle.stop.clone())), optimal, falloff,
        offensive: cycle.effect.has(EffectFlag::Offensive),
    };
    let mut victim_owner = None;
    match cycle.effect.kind {
        EffectKind::Neutralizer => {
            let amount = fit.value(key, ATTR_ENERGY_NEUTRALIZER_AMOUNT).max(0.0) * scale;
            victim_owner = drain_exact_guarded(&cx.db, &cx.world, cx.system_id, target, amount, guard).await?.and_then(|(_, owner)| owner);
        }
        EffectKind::Nosferatu => {
            let Some(source) = capacitor_snapshot(&cx.db, &cx.world, cx.system_id, cx.ship_id).await? else { return Ok(CycleOutcome::Stop("source lost")); };
            let Some(victim) = capacitor_snapshot(&cx.db, &cx.world, cx.system_id, target).await? else { return Ok(super::lifecycle::target_loss(cycle).await); };
            if nos_allowed(source.level, source.capacity, victim.level, victim.capacity, fit.value(key, ATTR_NOS_OVERRIDE) > 0.0) {
                let amount = fit.value(key, ATTR_POWER_TRANSFER_AMOUNT).max(0.0) * scale;
                if let Some((drained, owner)) = drain_exact_guarded(&cx.db, &cx.world, cx.system_id, target, amount, guard).await? {
                    victim_owner = owner;
                    inject(&cx.db, &cx.world, cx.system_id, cx.ship_id, drained).await?;
                }
            }
        }
        EffectKind::CapTransfer => {
            let amount = fit.value(key, ATTR_POWER_TRANSFER_AMOUNT).max(0.0) * scale;
            inject_guarded(&cx.db, &cx.world, cx.system_id, target, amount, guard).await?;
        }
        _ => unreachable!("not a capacitor effect"),
    }
    if target_drone && cycle.effect.has(EffectFlag::Offensive) {
        if let (Some(victim), Some(_)) = (victim_owner, fit.source_type(Key::Ship)) {
            crate::services::crimewatch::aggress(&cx.db, &cx.world, cx.system_id, cx.char_id, victim).await?;
        }
    }
    Ok(CycleOutcome::Continue)
}

#[cfg(test)] mod tests;
