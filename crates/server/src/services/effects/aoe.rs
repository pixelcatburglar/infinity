//! Player and NPC smartbomb radius damage from fitted dogma attributes.
//! Mirrors EMPWave.py's empFieldRange; candidates use actor-sampled positions and
//! damage uses the shared combat pipeline. Effects and damage are transient.
use super::lifecycle::{Cycle, CycleOutcome, Module};
use super::registry::Effect;
use crate::consts::dogma::ATTR_EMP_FIELD_RANGE;
use crate::messages;
use crate::db::Db;
use crate::services::combat::{self, Ctx, ATTR_DAMAGE, groups_health::damage};
use crate::services::fit::{Fit, Key};
use crate::services::{user_error, CallError};
use crate::space::{dist, Ball, System, World, V3, MODE_WARP};
use std::sync::{Arc, atomic::AtomicBool};

/// Sample only victims within the inclusive radius; the source is never hit.
pub(crate) fn victims(s: &System, source: i64, center: V3, radius: f64) -> Vec<(i64, Option<i64>)> {
    if !radius.is_finite() || radius < 0.0 { return Vec::new(); }
    let mut victims: Vec<_> = s.balls().filter(|ball| ball.id != source)
        .filter_map(|ball| s.sample_position(ball.id).filter(|pos| dist(*pos, center) <= radius).map(|_| (ball.id, ball.char_id)))
        .collect();
    victims.sort_unstable_by_key(|entry| entry.0);
    victims
}

fn generation_valid(master: i64, generation: &Arc<AtomicBool>) -> bool {
    combat::state().active.get(&master).is_some_and(|active| Arc::ptr_eq(&active.stop, generation))
}

/// Apply raw area damage and Crimewatch independently to each sampled victim.
pub(super) async fn apply_damage(cx: &Ctx, weapon: i32, victims: Vec<(i64, Option<i64>)>, raw: [f64; 4]) -> Result<(), CallError> {
    apply_damage_guarded(&cx.db, &cx.world, cx.system_id, cx.ship_id, weapon, victims, raw, Some(cx.char_id), None).await
}

async fn apply_damage_guarded(
    db: &Db, world: &Arc<World>, system: i64, ship: i64, weapon: i32,
    victims: Vec<(i64, Option<i64>)>, raw: [f64; 4], attacker: Option<i64>,
    guard: Option<DamageGuard<'_>>,
) -> Result<(), CallError> {
    let ship_type = if attacker.is_some() {
        world.with(system, move |s| s.ball(ship).map(|b| b.type_id)).await.flatten().unwrap_or(0)
    } else { 0 };
    for (target, pilot) in victims {
        if !guard_valid(world, system, &guard).await { break; }
        if let (Some(attacker), Some(pilot)) = (attacker, pilot) {
            crate::services::crimewatch::aggress(db, world, system, attacker, ship_type, weapon, pilot, target).await?;
        }
        if !guard_valid(world, system, &guard).await { break; }
        let dealt = damage(db, world, system, target, raw, attacker, Some(ship)).await?;
        super::weapons::report_hit(world, ship, target, attacker, pilot, weapon, dealt, super::weapons::hit_quality(1.0));
    }
    Ok(())
}

enum DamageGuard<'a> {
    Player(i64, &'a Arc<AtomicBool>),
    Npc(i64, i64, u64),
}

async fn guard_valid(world: &World, system: i64, guard: &Option<DamageGuard<'_>>) -> bool {
    match guard {
        None => true,
        Some(DamageGuard::Player(master, generation)) => generation_valid(*master, generation),
        Some(DamageGuard::Npc(source, hostile, generation)) => {
            let (source, hostile, generation) = (*source, *hostile, *generation);
            world.with(system, move |s| s.npc_projection_valid(source, hostile, generation)
                && s.ball(source).is_some_and(|b| b.mode != MODE_WARP)).await.unwrap_or(false)
        }
    }
}

pub(crate) async fn apply_npc(
    db: &Db, world: &Arc<World>, system: i64, _source_module: i64, me: &Ball, victim: &Ball,
    _effect: &Effect, _duration_ms: f64, generation: u64,
) -> Result<(), CallError> {
    let source = me.id;
    let hostile = victim.id;
    let sde = world.sde().clone();
    let Some((targets, raw, weapon)) = world.with(system, move |s| {
        if !s.npc_projection_valid(source, hostile, generation) { return None; }
        let me = s.sampled_ball(source)?;
        if me.mode == MODE_WARP { return None; }
        let fit = Fit::for_space_type(sde, me.type_id, s.projections_on(source));
        let radius = fit.value(Key::Ship, ATTR_EMP_FIELD_RANGE);
        let raw = ATTR_DAMAGE.map(|attr| fit.value(Key::Ship, attr).max(0.0));
        Some((victims(s, source, me.pos, radius), raw, me.type_id))
    }).await.flatten() else { return Ok(()) };
    apply_damage_guarded(db, world, system, source, weapon, targets, raw, None,
        Some(DamageGuard::Npc(source, hostile, generation))).await
}

pub(super) async fn validate(cx: &Ctx, _fit: &Fit, _module: &Module, _effect: &Effect, _target: Option<i64>) -> Result<(), CallError> {
    let ship = cx.ship_id;
    let mode = cx.world.with(cx.system_id, move |s| s.ball(ship).map(|b| b.mode)).await.flatten()
        .ok_or_else(|| user_error(messages::MSG_DENIED_ACTIVATE_TARGET_NOT_PRESENT, vec![]))?;
    if mode == MODE_WARP { return Err(user_error(messages::MSG_DENIED_ACTIVATE_IN_WARP, vec![])); }
    Ok(())
}

pub(super) async fn start(cx: &Ctx, fit: &Fit, cycle: &mut Cycle) -> Result<CycleOutcome, CallError> {
    let master = cycle.modules[0].id;
    let generation = cycle.stop.clone();
    for module in &cycle.modules {
        let key = Key::Module(module.id);
        let raw = ATTR_DAMAGE.map(|attr| fit.value(key, attr).max(0.0));
        let radius = fit.value(key, ATTR_EMP_FIELD_RANGE);
        let ship = cx.ship_id;
        let check = generation.clone();
        let Some(targets) = cx.world.with(cx.system_id, move |s| {
            if !generation_valid(master, &check) { return None; }
            let source = s.sampled_ball(ship)?;
            if source.mode == MODE_WARP { return None; }
            Some(victims(s, ship, source.pos, radius))
        }).await.flatten() else { return Ok(CycleOutcome::Stop("source lost or in warp")); };
        apply_damage_guarded(&cx.db, &cx.world, cx.system_id, cx.ship_id, module.type_id, targets, raw,
            Some(cx.char_id), Some(DamageGuard::Player(master, &generation))).await?;
        if !generation_valid(master, &generation) { return Ok(CycleOutcome::Stop("activation replaced")); }
    }
    Ok(CycleOutcome::Continue)
}

#[cfg(test)] mod tests;
