//! Electronic countermeasures for targeted and burst jammers.
//! `eve/client/script/dogma/clientDogmaLocation.py` supplies the primary-sensor
//! order; `eve/client/script/environment/godma.py` consumes jam timers.
//! Fit reads immutable SDE dogma attributes with current incoming projections.
//! The system actor owns jam lifetimes; jams and locks are not resumed after
//! restart. Other burst projectors are not supported.
//! The existing TargetingAttemptCancelled lock error is the server's choice:
//! the client supplies no ECM-specific lock error. Attempts are revalidated
//! in the system actor before a jam is applied.
use std::sync::{Arc, atomic::Ordering};
use super::{EffectKind, lifecycle::{Cycle, CycleOutcome, Module}, range::range_multiplier, registry::{Effect, EffectAttribute, EffectFlag}};
use super::space_source::{SourceContext, SourceGuard};
use infinity_core::consts::{dogma::ATTR_SCAN_GRAVIMETRIC_STRENGTH, dogma::ATTR_SCAN_LADAR_STRENGTH, dogma::ATTR_SCAN_MAGNETOMETRIC_STRENGTH, dogma::ATTR_SCAN_RADAR_STRENGTH, CATEGORY_SHIP, CATEGORY_ENTITY, CATEGORY_DRONE};
use crate::db::Db;
use infinity_core::messages;
use crate::sde::Sde;
use crate::services::{combat::{self, Ctx, targeting_charges::targets_of}, crimewatch, fit::{Fit, Key}, rand01, user_error, CallError};
use crate::space::{dist, Ball, System, World, MODE_WARP, drone::DronePhase};

/// Client dogma/const.py: racial ECM strengths in primary-sensor order.
const JAM_STRENGTH: [i32; 4] = [238, 239, 240, 241];
/// Client dogma/const.py: primary sensor order.
const SENSORS: [i32; 4] = [ATTR_SCAN_GRAVIMETRIC_STRENGTH, ATTR_SCAN_LADAR_STRENGTH, ATTR_SCAN_MAGNETOMETRIC_STRENGTH, ATTR_SCAN_RADAR_STRENGTH];
/// Client dogma/const.py: ecmBurstRange.
const BURST_RANGE: i32 = 142;
/// Client dogma/const.py: ecmJamDuration.
const JAM_DURATION: i32 = 2822;

fn missing() -> CallError { user_error(messages::MSG_DENIED_ACTIVATE_TARGET_NOT_PRESENT, vec![]) }
fn primary(values: [f64; 4]) -> Option<(usize, f64)> {
    let mut best = None;
    for (index, value) in values.into_iter().enumerate() {
        if value > 0.0 && best.is_none_or(|(_, old)| value > old) { best = Some((index, value)); }
    }
    best
}
fn chance(strength: f64, defense: f64, range: f64) -> f64 {
    if !strength.is_finite() || !defense.is_finite() || !range.is_finite() || strength <= 0.0 || defense <= 0.0 || range <= 0.0 { return 0.0; }
    (strength * range / defense).min(1.0)
}
fn eligible(sde: &Sde, ball: &Ball) -> bool {
    ball.cloak == 0 && ball.mode != MODE_WARP && sde.types.get(ball.type_id).is_some_and(|t| matches!(t.category_id, Some(CATEGORY_SHIP | CATEGORY_ENTITY | CATEGORY_DRONE)))
}
fn gap(a: &Ball, b: &Ball) -> f64 { (dist(a.pos, b.pos) - f64::from(a.radius) - f64::from(b.radius)).max(0.0) }
fn choose_duration(jam: Option<f64>, cycle: f64) -> f64 {
    jam.filter(|value| value.is_finite() && *value > 0.0).unwrap_or(cycle)
}
fn selected_duration(fit: &Fit, key: Key, cycle: f64) -> f64 {
    choose_duration(fit.type_has_attribute(key, JAM_DURATION).then(|| fit.value(key, JAM_DURATION)), cycle)
}

pub(crate) fn npc_cycle_duration(sde: &Arc<Sde>, type_id: i32, effect: &Effect) -> Option<f64> {
    let fit = Fit::for_space_type(sde.clone(), type_id, Vec::new());
    let attr = effect.attribute(EffectAttribute::Duration)?;
    if !fit.type_has_attribute(Key::Ship, attr) { return None; }
    let value = fit.value(Key::Ship, attr);
    (value.is_finite() && value > 0.0).then_some(value)
}

pub(super) async fn validate(cx: &Ctx, fit: &Fit, module: &Module, effect: &Effect, target: Option<i64>) -> Result<(), CallError> {
    let source = cx.ship_id;
    let target = if effect.kind == EffectKind::Ecm {
        Some(target.filter(|&id| id != source && targets_of(cx.char_id).contains(&id)).ok_or_else(missing)?)
    } else { None };
    let optimal = effect.attribute(EffectAttribute::Range).map_or(0.0, |a| fit.value(Key::Module(module.id), a));
    let falloff = effect.attribute(EffectAttribute::Falloff).map_or(0.0, |a| fit.value(Key::Module(module.id), a));
    let sde = cx.world.sde().clone();
    let valid = cx.world.with(cx.system_id, move |s| {
        let Some(a) = s.sampled_ball(source) else { return false };
        if a.mode == MODE_WARP { return false; }
        target.is_none_or(|id| s.sampled_ball(id).is_some_and(|b| eligible(&sde, &b) && range_multiplier(gap(&a, &b), optimal, falloff) > 0.0))
    }).await.unwrap_or(false);
    if valid { Ok(()) } else { Err(missing()) }
}

#[derive(Clone)]
struct Source { id: i64, type_id: Option<i32>, module: i64, optimal: f64, falloff: f64, strengths: [f64; 4], duration: f64, burst: bool, offensive: bool, revision: Option<u64>, owner_revision: Option<(i64, u64)> }
fn source(fit: &Fit, key: Key, id: i64, module: i64, effect: &Effect, cycle: f64) -> Source {
    let drone = matches!(key, Key::Drone(_));
    Source { id, type_id: fit.source_type(if drone { key } else { Key::Ship }), module, optimal: effect.attribute(EffectAttribute::Range).map_or(0.0, |a| fit.value(key, a)), falloff: effect.attribute(EffectAttribute::Falloff).map_or(0.0, |a| fit.value(key, a)), strengths: JAM_STRENGTH.map(|a| fit.value(key, a)), duration: selected_duration(fit, key, cycle), burst: effect.kind == EffectKind::BurstJammer, offensive: effect.has(EffectFlag::Offensive), revision: if drone { fit.drone_projection_revision() } else { fit.projection_revision() }, owner_revision: None }
}

fn current_attempt(s: &System, sde: &Sde, src: &Source, victim: &Ball, revision: Option<u64>, owner_revision: Option<(i64, u64)>, player: &Option<(i64, Arc<std::sync::atomic::AtomicBool>)>, guard: Option<SourceGuard>) -> Option<(Ball, Ball, f64)> {
    let id = victim.id;
    if let Some((character, generation)) = player {
        if generation.load(Ordering::Relaxed) || (!src.burst && !targets_of(*character).contains(&id)) { return None; }
        if !combat::state().active.get(&src.module).is_some_and(|active| Arc::ptr_eq(&active.stop, generation)) { return None; }
    }
    if guard.is_some_and(|guard| !guard.valid(s)) { return None; }
    if revision.is_some_and(|r| s.projection_revision(id) != r) || src.revision.is_some_and(|r| s.projection_revision(src.id) != r)
        || owner_revision.is_some_and(|(ship, r)| s.projection_revision(ship) != r)
        || src.owner_revision.is_some_and(|(ship, r)| s.projection_revision(ship) != r) { return None; }
    let (a, b) = s.sampled_ball(src.id).zip(s.sampled_ball(id))?;
    if a.mode == MODE_WARP || Some(a.type_id) != src.type_id || player.as_ref().is_some_and(|(character, _)| a.char_id != Some(*character))
        || id == src.id || b.type_id != victim.type_id || b.char_id != victim.char_id || !eligible(sde, &b) { return None; }
    let multiplier = if src.burst { if gap(&a, &b) <= src.optimal && src.optimal.is_finite() && src.optimal > 0.0 { 1.0 } else { 0.0 } } else { range_multiplier(gap(&a, &b), src.optimal, src.falloff) };
    (multiplier > 0.0).then_some((a, b, multiplier))
}

async fn apply_one(db: &Db, world: &Arc<World>, system: i64, src: &Source, victim: &Ball, player: Option<(i64, Arc<std::sync::atomic::AtomicBool>)>, guard: Option<SourceGuard>) -> Result<Option<Option<i64>>, CallError> {
    let id = victim.id;
    let record = world.with(system, move |s| s.drone(id)).await.flatten();
    let victim_owner = record.as_ref().map(|d| d.owner);
    let (fit, key, revision, owner_revision) = if let Some(character) = victim.char_id {
        let fit = Fit::load_in_space(db, world, system, character, id).await?;
        let revision = fit.projection_revision();
        (fit, Key::Ship, revision, None)
    } else if let Some(record) = record.filter(|d| d.owner != infinity_core::consts::OWNER_UNOWNED) {
        let ship = crate::dao::characters::selection::ship_id(db, record.owner).await.map_err(crate::services::db_err)?
            .ok_or_else(|| CallError::Internal(anyhow::anyhow!("character has no selected ship")))?;
        let fit = Fit::drone_in_space(db, world, system, record.owner, id).await?;
        let revision = fit.drone_projection_revision();
        let owner_revision = fit.projection_revision().map(|r| (ship, r));
        (fit, Key::Drone(id), revision, owner_revision)
    } else {
        let Some((projections, revision)) = world.with(system, move |s| (s.projections_on(id), s.projection_revision(id))).await else { return Ok(None) };
        (Fit::for_space_type(world.sde().clone(), victim.type_id, projections), Key::Ship, Some(revision), None)
    };
    let (index, defense) = primary(SENSORS.map(|a| fit.value(key, a))).unwrap_or((0, 0.0));
    let strength = src.strengths[index];
    let src = src.clone();
    let victim = victim.clone();
    let sde = world.sde().clone();
    let roll = rand01();
    Ok(world.with(system, move |s| {
        if victim_owner.is_some_and(|owner| s.drone(id).is_none_or(|d| d.owner != owner)) { return None; }
        let Some((_, _, multiplier)) = current_attempt(s, &sde, &src, &victim, revision, owner_revision, &player, guard) else { return None };
        if src.offensive { s.drone_attack(src.id, id); }
        if roll < chance(strength, defense, multiplier) && src.duration.is_finite() && src.duration > 0.0 {
            combat::jamming::apply(s, src.id, src.module, id, src.duration);
        }
        Some(s.combat_owner(id))
    }).await.flatten())
}

pub(super) async fn start(cx: &Ctx, fit: &Fit, cycle: &mut Cycle) -> Result<CycleOutcome, CallError> {
    let module = &cycle.modules[0];
    let mut src = source(fit, Key::Module(module.id), cx.ship_id, module.id, &cycle.effect, cycle.duration_ms);
    if src.burst { src.optimal = fit.value(Key::Module(module.id), BURST_RANGE); }
    let source_id = cx.ship_id;
    let target = cycle.target;
    let burst = src.burst;
    let range = src.optimal;
    let sde = cx.world.sde().clone();
    let falloff = src.falloff;
    let character = cx.char_id;
    let module_id = module.id;
    let generation = cycle.stop.clone();
    let revision = src.revision;
    let candidates = cx.world.with(cx.system_id, move |s| {
        if generation.load(Ordering::Relaxed) || revision.is_some_and(|r| s.projection_revision(source_id) != r)
            || !combat::state().active.get(&module_id).is_some_and(|active| Arc::ptr_eq(&active.stop, &generation)) { return Vec::new(); }
        let Some(a) = s.sampled_ball(source_id) else { return Vec::new() };
        if a.mode == MODE_WARP { return Vec::new(); }
        if !burst {
            return target.filter(|&id| id != source_id && targets_of(character).contains(&id))
                .and_then(|id| s.sampled_ball(id))
                .filter(|b| eligible(&sde, b) && range_multiplier(gap(&a, b), range, falloff) > 0.0)
                .map(|b| { let revision = s.projection_revision(b.id); (b, revision) })
                .into_iter().collect();
        }
        if !range.is_finite() || range <= 0.0 { return Vec::new(); }
        s.balls().filter(|b| b.id != source_id && eligible(&sde, b) && (dist(a.pos, b.position_at(s.now_ms())) - f64::from(a.radius) - f64::from(b.radius)).max(0.0) <= range).filter_map(|b| s.sampled_ball(b.id)).map(|b| { let revision = s.projection_revision(b.id); (b, revision) }).collect()
    }).await.unwrap_or_default();
    if !burst && candidates.is_empty() { return Ok(super::lifecycle::target_loss(cycle).await); }
    for (victim, _) in candidates {
        let admitted = apply_one(&cx.db, &cx.world, cx.system_id, &src, &victim, Some((cx.char_id, cycle.stop.clone())), None).await?;
        if src.offensive {
            if let (Some(Some(character)), Some(_)) = (admitted, src.type_id) {
                crimewatch::aggress(&cx.db, &cx.world, cx.system_id, cx.char_id, character).await?;
            }
        }
    }
    Ok(CycleOutcome::Continue)
}

pub(crate) async fn apply_source(cx: &SourceContext<'_>) -> Result<(), CallError> {
    if !matches!(cx.phase, DronePhase::Begin) { return Ok(()); }
    let mut src = source(cx.fit, cx.key, cx.source.id, cx.module, cx.effect, cx.duration_ms);
    if let Some(drone) = cx.drone {
        src.owner_revision = drone.controller.zip(cx.fit.projection_revision());
    }
    let admitted = apply_one(cx.db, cx.world, cx.system, &src, cx.target, None, Some(cx.guard)).await?;
    if src.offensive {
        if let (Some(attacker), Some(Some(victim))) = (cx.attacker, admitted) {
            crimewatch::aggress(cx.db, cx.world, cx.system, attacker, victim).await?;
        }
    }
    Ok(())
}

#[cfg(test)] mod tests;
