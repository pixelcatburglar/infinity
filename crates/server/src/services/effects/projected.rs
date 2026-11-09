//! Snapshot and apply targeted dogma modifiers for in-space sources.
//! Mirrors the projected effects in dogma/pythonEffects and uses SDE modifier
//! rows with effective source Fit values. Lifetime and notifications belong to
//! the system actor; effects with no target modifier rows are not synthesized here.
//! Remote assistance insertion is denied for capital targets with an active Entosis link.
use super::{projection::Projection, range::range_multiplier, registry::{Effect, EffectAttribute, EffectFlag}};
use super::space_source::{PreparedCombatAttempt, SourceContext, SourceGuard};
use infinity_core::messages;
use crate::sde::{dogma::Modifier, Sde};
use crate::services::{combat::Ctx, fit::{Fit, Key, OP_PRE_MUL, OP_PRE_DIV, OP_POST_MUL, OP_POST_DIV}, user_error, CallError};
use crate::space::{dist, seconds_to_ms, World, drone::DronePhase};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Scale multiplier and divisor operands around their neutral value, rather
/// than scaling the operand itself. Percent and additive operands have zero
/// as their neutral value.
fn scaled(value: f64, op: i32, strength: f64) -> f64 {
    match op {
        OP_PRE_MUL | OP_PRE_DIV | OP_POST_MUL | OP_POST_DIV => 1.0 + (value - 1.0) * strength,
        _ => value * strength,
    }
}

fn snapshot_rows(fit: &Fit, key: Key, rows: &[Modifier], values: &mut Vec<(i32, f64, i32)>) {
    for row in rows.iter().filter(|row| row.domain == "targetID") {
        if values.iter().any(|entry| entry.0 == row.modifying) { continue; }
        let source = crate::sde::dogma::python_effects::projected_interim_source(row.modifying).unwrap_or(row.modifying);
        values.push((row.modifying, fit.value(key, source), row.op));
    }
}

fn snapshot(sde: &Sde, fit: &Fit, key: Key, effect: &Effect, charge_type: Option<i32>) -> (Vec<(i32, f64, i32)>, Vec<(i32, f64, i32)>) {
    let mut values = Vec::new();
    let mut charge_values = Vec::new();
    if let Some(row) = sde.dogma.effect(effect.id as i32) {
        snapshot_rows(fit, key, row.modifiers(), &mut values);
    }
    if let (Key::Module(module), Some(charge)) = (key, charge_type) {
        for entry in sde.types.effects(charge) {
            if let Some(row) = sde.dogma.effect(entry.effect_id) {
                snapshot_rows(fit, Key::Charge(module), row.modifiers(), &mut charge_values);
            }
        }
    }
    (values, charge_values)
}

fn missing_target() -> CallError {
    user_error(messages::MSG_DENIED_ACTIVATE_TARGET_NOT_PRESENT, vec![])
}

#[allow(clippy::too_many_arguments)]
async fn insert(world: &Arc<World>, system: i64, source: i64, target: i64, module: i64, source_type: i32, effect: &Effect, duration_ms: f64, charge_type: Option<i32>, snapshots: (Vec<(i32, f64, i32)>, Vec<(i32, f64, i32)>), optimal: f64, falloff: f64, guard: Option<SourceGuard>, player: Option<(i64, Arc<AtomicBool>)>, mut attempt: Option<PreparedCombatAttempt>) -> Result<(Option<i64>, bool), CallError> {
    let kind = effect.kind;
    let effect_id = effect.id as i32;
    let offensive = effect.has(EffectFlag::Offensive);
    let assistance = effect.has(EffectFlag::Assistance);
    let inserted = world.with(system, move |s| -> Result<Option<(Option<i64>, bool)>, crate::space::combat_facts::Refusal> {
        if let Some((character, generation)) = &player {
            if generation.load(Ordering::Relaxed) || !crate::services::combat::targeting_charges::targets_of(*character).contains(&target) { return Ok(None); }
            let state = crate::services::combat::state();
            if !state.active.get(&module).is_some_and(|active| Arc::ptr_eq(&active.stop, generation)) { return Ok(None); }
        }
        if guard.is_some_and(|guard| !guard.valid(s)) { return Ok(None); }
        if assistance && source != target && crate::space::sovereignty::links::assistance_blocked(s, target) { return Ok(None); }
        let Some((a, b)) = s.sampled_ball(source).zip(s.sampled_ball(target)) else { return Ok(None) };
        let gap = (dist(a.pos, b.pos) - f64::from(a.radius) - f64::from(b.radius)).max(0.0);
        let strength = range_multiplier(gap, optimal, falloff);
        if strength <= 0.0 { return Ok(None); }
        let scale = |entries: Vec<(i32, f64, i32)>| entries.into_iter().map(|(attr, value, op)| (attr, scaled(value, op, strength))).collect();
        let values = scale(snapshots.0);
        let charge_values = scale(snapshots.1);
        let until_ms = if duration_ms < 0.0 { i64::MAX } else {
            s.now_ms().saturating_add(duration_ms as i64).saturating_add(seconds_to_ms(1.0) as i64)
        };
        let mut projection = Projection { source_ship: source, source_module: module, source_type, effect_id, kind, until_ms, values, charge_values, charge_type, scramble: None };
        if matches!(kind, super::EffectKind::Projected { jamming: super::JammingType::Scramble }) {
            let token = attempt.as_mut().ok_or(crate::space::combat_facts::Refusal::Invalid)?;
            let facts = token.facts();
            let result = s.accept_projection_scramble(target, &projection, facts);
            if result.is_err() { s.finish_combat_attempt(&facts); }
            token.disarm();
            projection.scramble = Some(result?);
        }
        s.upsert_projection(target, projection);
        if offensive { s.drone_attack(source, target); }
        Ok(Some((s.combat_owner(target), s.drone(target).is_some())))
    }).await.ok_or_else(missing_target)?;
    match inserted {
        Ok(value) => value.ok_or_else(missing_target),
        Err(crate::space::combat_facts::Refusal::Full) => Err(crate::services::combat::capacity::refusal(system)),
        Err(error) => Err(CallError::Internal(anyhow::anyhow!("scramble admission refused: {error:?}"))),
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn apply(cx: &Ctx, fit: &Fit, source_module: i64, source_type: i32, effect: &Effect, target: i64, duration_ms: f64, charge_type: Option<i32>, attempt: Option<PreparedCombatAttempt>) -> Result<(), CallError> {
    let key = Key::Module(source_module);
    let generation = crate::services::combat::state().active.get(&source_module).map(|active| active.stop.clone()).ok_or_else(missing_target)?;
    let optimal = effect.attribute(EffectAttribute::Range).map_or(0.0, |attr| fit.value(key, attr));
    let falloff = effect.attribute(EffectAttribute::Falloff).map_or(0.0, |attr| fit.value(key, attr));
    let values = snapshot(cx.world.sde(), fit, key, effect, charge_type);
    let (victim, drone) = insert(&cx.world, cx.system_id, cx.ship_id, target, source_module, source_type, effect, duration_ms, charge_type, values, optimal, falloff, None, Some((cx.char_id, generation)), attempt).await?;
    if drone && effect.has(EffectFlag::Offensive) {
        if let Some(victim) = victim {
            crate::services::crimewatch::aggress(&cx.db, &cx.world, cx.system_id, cx.char_id, victim).await?;
        }
    }
    if matches!(effect.kind, super::EffectKind::Projected { jamming: super::JammingType::Scramble }) {
        super::lifecycle::stop_blocked_propulsion(&cx.db, &cx.world, cx.system_id, target).await;
    }
    Ok(())
}

pub(crate) async fn apply_source(cx: &SourceContext<'_>, attempt: Option<PreparedCombatAttempt>) -> Result<(), CallError> {
    if !matches!(cx.phase, DronePhase::Begin) { return Ok(()); }
    let optimal = cx.effect.attribute(EffectAttribute::Range).map_or(0.0, |attr| cx.fit.value(cx.key, attr));
    let falloff = cx.effect.attribute(EffectAttribute::Falloff).map_or(0.0, |attr| cx.fit.value(cx.key, attr));
    let values = snapshot(cx.world.sde(), cx.fit, cx.key, cx.effect, None);
    let (victim, _) = insert(cx.world, cx.system, cx.source.id, cx.target.id, cx.module, cx.source.type_id, cx.effect, cx.duration_ms, None, values, optimal, falloff, Some(cx.guard), None, attempt).await?;
    if cx.effect.has(EffectFlag::Offensive) {
        if let (Some(attacker), Some(victim)) = (cx.attacker, victim) {
            crate::services::crimewatch::aggress(cx.db, cx.world, cx.system, attacker, victim).await?;
        }
    }
    if matches!(cx.effect.kind, super::EffectKind::Projected { jamming: super::JammingType::Scramble }) {
        super::lifecycle::stop_blocked_propulsion(cx.db, cx.world, cx.system, cx.target.id).await;
    }
    Ok(())
}

#[cfg(test)] mod tests;
