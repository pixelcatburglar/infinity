//! Cycle-family validation, execution and cleanup for dogma effects.
//! Mirrors clientDogmaLocation and godma using SDE effect metadata.
//! Unsupported effects are refused before reaching these dispatchers; unexpected
//! invalid dispatch returns internal errors, and unexpected cleanup logs a warning.
use super::{EffectKind, repair, mining, capacitor, projected, range, ecm, aoe, bombs, vorton, cloak, mjd, compression, tractor, scan, targeting, salvage};
use super::registry::{Effect, EffectAttribute, EffectFlag};
use super::lifecycle::{Module, Cycle, CycleOutcome, weapon_fx, charge_kind, UE_TYPEID};
use super::notify::fire_fx;
use super::propulsion::apply_propulsion;
use super::weapons::{consume_charge, empty_charge, fire_missile, fire_turret};
use crate::services::combat::{state, Ctx};
use crate::services::combat::targeting_charges::{loaded, targets_of};
use crate::services::fit::{Fit, Key};
use crate::services::{user_error, CallError};
use infinity_core::consts::{dogma::ATTR_ACTIVATION_BLOCKED, CATEGORY_SHIP, dogma::EFFECT_MICROWARPDRIVE};
use infinity_core::messages;
use crate::space::{System, MODE_WARP};
use marshal::Value;
use std::sync::Arc;
use tracing::warn;
fn repair_ranges(fit: &Fit, cycle: &Cycle) -> (f64, f64) {
    let module = Key::Module(cycle.modules[0].id);
    (cycle.effect.attribute(EffectAttribute::Range).map(|a| fit.value(module, a)).unwrap_or(0.0),
     cycle.effect.attribute(EffectAttribute::Falloff).map(|a| fit.value(module, a)).unwrap_or(0.0))
}

async fn remote_repair(cx: &Ctx, fit: &Fit, cycle: &mut Cycle, layer: usize) -> Result<CycleOutcome, CallError> {
    let Some(mut attempt) = cycle.owner.pending_combat.take() else {
        return Err(CallError::Internal(anyhow::anyhow!("missing remote repair attempt")));
    };
    let (optimal, falloff) = repair_ranges(fit, cycle);
    let Ok((target, pilot, multiplier)) = repair::target(cx, cycle.target, optimal, falloff).await else {
        return Ok(super::lifecycle::target_loss(cycle).await);
    };
    let master = cycle.modules[0].id;
    if !state().active.get(&master).is_some_and(|active| Arc::ptr_eq(&active.stop, &cycle.stop)) {
        return Ok(CycleOutcome::Stop("activation lost"));
    }
    let generation = cycle.stop.clone();
    let source = cx.ship_id;
    let character = cx.char_id;
    let sde = cx.world.sde().clone();
    let guard = move |s: &System| {
        if !state().active.get(&master).is_some_and(|active| Arc::ptr_eq(&active.stop, &generation))
            || !targets_of(character).contains(&target) { return false; }
        let Some((ship, victim)) = s.sampled_ball(source).zip(s.sampled_ball(target)) else { return false; };
        if ship.char_id != Some(character) || ship.mode == MODE_WARP || victim.mode == MODE_WARP || victim.char_id != pilot
            || crate::space::sovereignty::links::assistance_blocked(s, target) { return false; }
        let category = sde.types.get(victim.type_id).and_then(|t| t.category_id);
        if category != Some(CATEGORY_SHIP) && !(category == Some(infinity_core::consts::CATEGORY_ENTITY) && s.is_npc(target))
            && !(category == Some(infinity_core::consts::CATEGORY_DRONE) && s.drone(target).is_some()) { return false; }
        let gap = (crate::space::dist(ship.pos, victim.pos) - f64::from(ship.radius) - f64::from(victim.radius)).max(0.0);
        range::range_multiplier(gap, optimal, falloff) > 0.0
    };
    let amount = cycle.repair_amount * multiplier;
    let result = crate::services::combat::damage::repair_target(&cx.world, cx.system_id, target, layer, amount,
        crate::space::combat::RepairSource::Remote(attempt.facts()), guard).await;
    if result.is_ok() { attempt.disarm(); }
    result?;
    Ok(CycleOutcome::Continue)
}

pub(super) async fn on_cycle_start(cx: &Ctx, fit: &Fit, cycle: &mut Cycle, charged: bool) -> Result<CycleOutcome, CallError> {
    use EffectKind::*;
    let outcome = match cycle.effect.kind {
        Turret | Missile => {
            let target = cycle.target.unwrap_or(0);
            if !targets_of(cx.char_id).contains(&target) { return Ok(super::lifecycle::target_loss(cycle).await); }
            let mut fired = false;
            for index in 0..cycle.modules.len() {
                let charge = match consume_charge(cx, &cycle.modules[index], None).await { Ok(Some(t)) => t, _ => continue };
                cycle.charge_type = Some(charge);
                fired = true;
                let module_id = cycle.modules[index].id;
                let weapon_type = cycle.modules[index].type_id;
                let Some(attempt) = super::lifecycle::prepare_combat_source(cycle, module_id, Some(weapon_type)).await? else {
                    return Ok(CycleOutcome::Stop("activation lost"));
                };
                let module = &cycle.modules[index];
                let result = if cycle.effect.kind == Turret {
                    fire_turret(cx, fit, module, target, cycle.illegal, attempt).await
                } else {
                    fire_missile(cx, fit, module, charge, target, cycle.illegal, attempt).await
                };
                match result {
                    Err(error @ CallError::User { .. }) => return Err(error),
                    Err(CallError::Internal(error)) => warn!("weapon {} failed: {error:#}", module.id),
                    Ok(()) => {}
                }
            }
            if !fired { return Ok(CycleOutcome::Stop("out of charges")); }
            if cycle.cycles == 0 {
                weapon_fx(cx, &cycle.modules, &cycle.effect, Some(target), cycle.charge_type, true, cycle.duration_ms, cycle.repeat);
            }
            Ok(CycleOutcome::Continue)
        }
        LocalRepair { layer, amount_attr } => {
            cycle.repair_amount = repair::amount(fit, cycle.modules[0].id, amount_attr);
            repair::apply(cx, cycle, layer, cycle.repair_amount, true).await?;
            Ok(CycleOutcome::Continue)
        }
        FueledLocalRepair { layer, amount_attr, charge_bonus_attr } => {
            let module = &cycle.modules[0];
            if !charged && !empty_charge(cx, module).await? { return Ok(CycleOutcome::Stop("out of charges")); }
            cycle.repair_amount = repair::fueled(fit, module.id, amount_attr, charge_bonus_attr, charged);
            cycle.charge_type = if charged {
                match consume_charge(cx, module, cycle.charge_type).await? {
                    Some(kind) => Some(kind),
                    None => return Ok(CycleOutcome::Stop("out of charges")),
                }
            } else { None };
            repair::apply(cx, cycle, layer, cycle.repair_amount, true).await?;
            Ok(CycleOutcome::Continue)
        }
        RemoteRepair { layer } => {
            let (optimal, falloff) = repair_ranges(fit, cycle);
            if repair::target(cx, cycle.target, optimal, falloff).await.is_err() {
                return Ok(super::lifecycle::target_loss(cycle).await);
            }
            let module = &cycle.modules[0];
            cycle.repair_amount = if let FueledLocalRepair { amount_attr, charge_bonus_attr, .. } = charge_kind(cycle.effect.id, cycle.effect.kind) {
                if !charged && !empty_charge(cx, module).await? { return Ok(CycleOutcome::Stop("out of charges")); }
                let amount = repair::fueled(fit, module.id, amount_attr, charge_bonus_attr, charged);
                cycle.charge_type = if charged {
                    match consume_charge(cx, module, cycle.charge_type).await? {
                        Some(kind) => Some(kind),
                        None => return Ok(CycleOutcome::Stop("out of charges")),
                    }
                } else { None };
                amount
            } else { repair::amount(fit, module.id, repair::amount_attribute(layer)) };
            let module_id = cycle.modules[0].id;
            let Some(attempt) = super::lifecycle::prepare_combat_source(cycle, module_id, None).await? else {
                return Ok(CycleOutcome::Stop("activation lost"));
            };
            cycle.owner.pending_combat = Some(attempt);
            if repair::applies_at_start(layer) { remote_repair(cx, fit, cycle, layer).await } else { Ok(CycleOutcome::Continue) }
        }
        CapTransfer | Neutralizer | Nosferatu | CapBooster => capacitor::start(cx, fit, cycle).await,
        Mining => Ok(mining::start(cx, cycle)),
        Projected { .. } => {
            let target = cycle.target.unwrap_or(0);
            if !targets_of(cx.char_id).contains(&target) { return Ok(super::lifecycle::target_loss(cycle).await); }
            for index in 0..cycle.modules.len() {
                let module_id = cycle.modules[index].id;
                let attempt = if matches!(cycle.effect.kind, Projected { jamming: super::JammingType::Scramble }) {
                    let Some(attempt) = super::lifecycle::prepare_combat_source(cycle, module_id, None).await? else {
                        return Ok(CycleOutcome::Stop("activation lost"));
                    };
                    Some(attempt)
                } else { None };
                let module = &cycle.modules[index];
                let charge = fit.source_type(Key::Charge(module.id));
                projected::apply(cx, fit, module.id, module.type_id, &cycle.effect, target, cycle.duration_ms, charge, attempt).await?;
            }
            Ok(CycleOutcome::Continue)
        }
        Ecm | BurstJammer => ecm::start(cx, fit, cycle).await,
        Propulsion | Modifiers => Ok(CycleOutcome::Continue),
        Smartbomb => aoe::start(cx, fit, cycle).await,
        Bomb => bombs::start(cx, fit, cycle).await,
        Vorton => vorton::start(cx, fit, cycle).await,
        Cloak => cloak::start(cx, fit, cycle).await,
        MicroJumpDrive => mjd::start(cx, fit, cycle).await,
        Compression => compression::start(cx, fit, cycle).await,
        Tractor => tractor::start(cx, fit, cycle).await,
        CargoScan | ShipScan | SurveyScan => scan::start(cx, fit, cycle).await,
        AutoTarget | PassiveTarget => targeting::start(cx, fit, cycle).await,
        Salvage => salvage::start(cx, fit, cycle).await,
        kind @ (Entosis | ProbeLaunch | Hacking | Online) => Err(CallError::Internal(anyhow::anyhow!("unsupported cycle start kind: {kind:?}"))),
    }?;
    Ok(outcome)
}

pub(super) async fn on_cycle_end(cx: &Ctx, fit: &Fit, cycle: &mut Cycle) -> Result<CycleOutcome, CallError> {
    use EffectKind::*;
    match cycle.effect.kind {
        LocalRepair { layer, .. } | FueledLocalRepair { layer, .. } => {
            repair::apply(cx, cycle, layer, cycle.repair_amount, false).await?;
            Ok(CycleOutcome::Continue)
        }
        RemoteRepair { layer } => {
            if repair::applies_at_start(layer) { Ok(CycleOutcome::Continue) } else { remote_repair(cx, fit, cycle, layer).await }
        }
        Mining => Ok(mining::end(cx, fit, cycle).await),
        MicroJumpDrive => mjd::end(cx, fit, cycle).await,
        Salvage => salvage::end(cx, fit, cycle).await,
        CargoScan | ShipScan | SurveyScan => scan::end(cx, fit, cycle).await,
        Modifiers | Turret | Missile | Propulsion | Projected { .. } | CapTransfer | Neutralizer | Nosferatu | CapBooster | Ecm | BurstJammer | Smartbomb | Vorton | Cloak | Tractor | AutoTarget | PassiveTarget | Bomb | Compression | Entosis => Ok(CycleOutcome::Continue),
        kind @ (ProbeLaunch | Hacking | Online) => Err(CallError::Internal(anyhow::anyhow!("unsupported cycle end kind: {kind:?}"))),
    }
}

pub(super) fn on_stop(s: &mut System, cx: &Ctx, cycle: &Cycle) {
    use EffectKind::*;
    match cycle.effect.kind {
        Turret | Missile | FueledLocalRepair { .. } | RemoteRepair { .. } | CapTransfer | Neutralizer | Nosferatu | CapBooster | Ecm | BurstJammer | Smartbomb | Bomb | Vorton => {
            if let Some(guid) = cycle.effect.guid.as_deref().and_then(|g| g.split(',').next()) {
                let events = cycle.modules.iter().map(|m| fire_fx(s.current_time(), cx.ship_id, Some((m.id, m.type_id)), guid, cycle.target, cycle.charge_type, cycle.effect.has(EffectFlag::Offensive), false, cycle.duration_ms, cycle.repeat)).collect();
                s.broadcast(cx.ship_id, events);
            }
        }
        Mining => mining::stop(s, cx, cycle),
        Projected { .. } => {
            for module in &cycle.modules { s.remove_projection(module.id); }
        }
        Propulsion => {
            let cx = cx.clone();
            let master = cycle.modules[0].id;
            let generation = cycle.stop.clone();
            s.defer(async move { apply_propulsion(&cx, master, generation, false).await; });
        }
        Cloak => cloak::stop(s, cx, cycle),
        Compression => compression::stop(s, cx, cycle),
        Tractor => tractor::stop(s, cx, cycle),
        CargoScan | ShipScan | SurveyScan => scan::stop(s, cx, cycle),
        Entosis => {
            if cycle.cycles > 0 {
                if let Some(guid) = cycle.effect.guid.as_deref().and_then(|g| g.split(',').next()) {
                    let events = cycle.modules.iter().map(|m| fire_fx(s.current_time(), cx.ship_id, Some((m.id, m.type_id)), guid, cycle.target, cycle.charge_type, cycle.effect.has(EffectFlag::Offensive), false, cycle.duration_ms, cycle.repeat)).collect();
                    s.broadcast(cx.ship_id, events);
                }
            }
        }
        Salvage => salvage::stop(s, cx, cycle),
        Modifiers | LocalRepair { .. } | MicroJumpDrive | AutoTarget | PassiveTarget => {}
        kind @ (ProbeLaunch | Hacking | Online) => warn!(?kind, "unexpected unsupported effect cleanup"),
    }
}

pub(super) struct Activation {
    pub(super) effect: Effect,
    pub(super) module: Module,
    pub(super) target: Option<i64>,
}

pub(super) async fn on_activate(cx: &Ctx, fit: &Fit, activation: &Activation) -> Result<(), CallError> {
    use EffectKind::*;
    match activation.effect.kind {
        Turret | Missile => {
            let Some(target) = activation.target.filter(|t| targets_of(cx.char_id).contains(t)) else {
                return Err(user_error(messages::MSG_DENIED_ACTIVATE_TARGET_NOT_PRESENT, vec![]));
            };
            let ship = cx.ship_id;
            let (in_warp, present) = cx.world.with(cx.system_id, move |s| {
                (s.ball(ship).is_some_and(|b| b.mode == MODE_WARP), s.ball(target).is_some())
            }).await.unwrap_or((false, false));
            if in_warp { return Err(user_error(messages::MSG_DENIED_ACTIVATE_IN_WARP, vec![])); }
            if !present { return Err(user_error(messages::MSG_DENIED_ACTIVATE_TARGET_NOT_PRESENT, vec![])); }
            if loaded(&cx.db, cx.ship_id, activation.module.flag).await?.is_none_or(|(_, q)| q <= 0) {
                return Err(user_error(messages::MSG_NO_CHARGES, vec![("launcher".into(), Value::tuple([Value::Int(UE_TYPEID), Value::Int(activation.module.type_id as i64)]))]));
            }
            Ok(())
        }
        Projected { .. } => {
            let Some(target) = activation.target.filter(|t| targets_of(cx.char_id).contains(t)) else {
                return Err(user_error(messages::MSG_DENIED_ACTIVATE_TARGET_NOT_PRESENT, vec![]));
            };
            let ship = cx.ship_id;
            let sample = cx.world.with(cx.system_id, move |s| {
                s.sampled_ball(ship).zip(s.sampled_ball(target)).map(|(a, b)| {
                    (a.mode == MODE_WARP, crate::space::dist(a.pos, b.pos) - a.radius as f64 - b.radius as f64)
                })
            }).await.flatten();
            let Some((in_warp, gap)) = sample else { return Err(user_error(messages::MSG_DENIED_ACTIVATE_TARGET_NOT_PRESENT, vec![])); };
            if in_warp { return Err(user_error(messages::MSG_DENIED_ACTIVATE_IN_WARP, vec![])); }
            let optimal = activation.effect.attribute(EffectAttribute::Range).map(|a| fit.value(Key::Module(activation.module.id), a)).unwrap_or(0.0);
            let falloff = activation.effect.attribute(EffectAttribute::Falloff).map(|a| fit.value(Key::Module(activation.module.id), a)).unwrap_or(0.0);
            if range::range_multiplier(gap.max(0.0), optimal, falloff) <= 0.0 {
                return Err(user_error(messages::MSG_DENIED_ACTIVATE_TARGET_NOT_PRESENT, vec![]));
            }
            Ok(())
        }
        Mining => mining::validate(cx, fit, &activation.module, activation.target).await,
        RemoteRepair { .. } => {
            let key = Key::Module(activation.module.id);
            let optimal = activation.effect.attribute(EffectAttribute::Range).map(|a| fit.value(key, a)).unwrap_or(0.0);
            let falloff = activation.effect.attribute(EffectAttribute::Falloff).map(|a| fit.value(key, a)).unwrap_or(0.0);
            let (target, _, _) = repair::target(cx, activation.target, optimal, falloff).await?;
            if cx.world.with(cx.system_id, move |s| crate::space::sovereignty::links::assistance_blocked(s, target)).await.unwrap_or(true) {
                return Err(crate::services::notify_error(messages::effects::ENTOSIS_ASSISTANCE_BLOCKED));
            }
            Ok(())
        }
        CapTransfer | Neutralizer | Nosferatu | CapBooster => capacitor::validate(cx, fit, &activation.module, &activation.effect, activation.target).await,
        Ecm | BurstJammer => ecm::validate(cx, fit, &activation.module, &activation.effect, activation.target).await,
        Propulsion if activation.effect.id == i64::from(EFFECT_MICROWARPDRIVE) => {
            if fit.value(Key::Module(activation.module.id), ATTR_ACTIVATION_BLOCKED) > 0.0 {
                return Err(user_error(messages::MSG_MODULE_IS_BLOCKED, vec![]));
            }
            Ok(())
        }
        Modifiers | LocalRepair { .. } | FueledLocalRepair { .. } | Propulsion => Ok(()),
        Smartbomb => aoe::validate(cx, fit, &activation.module, &activation.effect, activation.target).await,
        Bomb => bombs::validate(cx, fit, &activation.module, &activation.effect, activation.target).await,
        Vorton => vorton::validate(cx, fit, &activation.module, &activation.effect, activation.target).await,
        Cloak => cloak::validate(cx, fit, &activation.module, &activation.effect, activation.target).await,
        MicroJumpDrive => mjd::validate(cx, fit, &activation.module, &activation.effect, activation.target).await,
        Compression => compression::validate(cx, fit, &activation.module, &activation.effect, activation.target).await,
        Tractor => tractor::validate(cx, fit, &activation.module, &activation.effect, activation.target).await,
        CargoScan | ShipScan | SurveyScan => scan::validate(cx, fit, &activation.module, &activation.effect, activation.target).await,
        AutoTarget | PassiveTarget => targeting::validate(cx, fit, &activation.module, &activation.effect, activation.target).await,
        Salvage => salvage::validate(cx, fit, &activation.module, &activation.effect, activation.target).await,
        Entosis => super::entosis::validate(cx, fit, activation).await,
        kind @ (ProbeLaunch | Hacking | Online) => Err(CallError::Internal(anyhow::anyhow!("unsupported activation kind: {kind:?}"))),
    }
}
