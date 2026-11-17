//! Player and NPC smartbomb radius damage from fitted dogma attributes, plus
//! the shared area settlement Vorton chain strikes use.
//! Mirrors EMPWave.py's empFieldRange; candidates use actor-sampled positions and
//! damage uses the shared combat pipeline. Damageable projectiles in range take
//! the shared projectile capability after the pulse's source admission and never
//! enter ship plans, combat facts or loss. Effects and damage are transient.
//! Vorton victims apply the shared missile formula per target, never a turret roll.
use super::lifecycle::{self, Cycle, CycleOutcome, Module};
use super::parameters::MemberPacket;
use super::registry::Effect;
use super::space_source::PreparedCombatAttempt;
use infinity_core::consts::dogma::ATTR_EMP_FIELD_RANGE;
use infinity_core::messages;
use crate::db::Db;
use crate::services::combat::{self, Ctx, ATTR_DAMAGE};
use crate::services::combat::damage;
use crate::services::fit::{Fit, Key};
use crate::services::{user_error, CallError};
use crate::space::{dist, Ball, System, World, V3, MODE_WARP};
use std::sync::{Arc, atomic::AtomicBool};

/// Sample only victims within the inclusive radius; the source is never hit.
pub(crate) fn victims(s: &System, source: i64, center: V3, radius: f64) -> Vec<(i64, Option<i64>, Option<u64>, bool, Option<(i64, f64)>)> {
    if !radius.is_finite() || radius < 0.0 { return Vec::new(); }
    let mut victims: Vec<_> = s.balls().filter(|ball| ball.id != source)
        .filter_map(|ball| s.sample_position(ball.id).filter(|pos| dist(*pos, center) <= radius)
            .map(|_| (ball.id, ball.char_id, s.combat_membership_generation(ball.id), false, None)))
        .collect();
    victims.sort_unstable_by_key(|entry| entry.0);
    victims
}

fn generation_valid(master: i64, generation: &Arc<AtomicBool>) -> bool {
    combat::state().active.get(&master).is_some_and(|active| Arc::ptr_eq(&active.stop, generation))
}

#[derive(Clone)]
pub(super) enum DamageGuard {
    Player(i64, Arc<AtomicBool>),
    Npc(i64, i64, u64),
}

/// How each accepted victim's raw damage vector is derived inside the actor
/// settlement. Vorton application samples every target's signature and
/// absolute sampled velocity independently through the shared missile
/// formula; there is no turret chance, miss or wrecking roll.
#[derive(Clone, Copy)]
pub(crate) enum DamageApplication {
    /// One fixed typed damage vector applied to every victim.
    Fixed([f64; 4]),
    /// Vorton packet: charge-raw damage scaled per target by missile
    /// application on that target's own signature and absolute speed.
    Vorton(super::parameters::VortonInputs),
    /// Fitted-signature scaling: `raw` scaled per target by
    /// `min(1, target_signature / signature_radius)`; a non-positive radius
    /// applies the full vector and a zero factor skips the victim.
    Signature { raw: [f64; 4], signature_radius: f64 },
}

pub(super) async fn apply_damage_guarded(
    db: &Db, world: &Arc<World>, system: i64, weapon: i32,
    mut victims: Vec<(i64, Option<i64>, Option<u64>, bool, Option<(i64, f64)>)>,
    application: DamageApplication,
    guard: Option<DamageGuard>, mut attempt: PreparedCombatAttempt,
) -> Result<(), CallError> {
    let facts = attempt.facts();
    let ship = facts.entity.item_id;
    let attacker = facts.actual_actor_character;
    let sde = world.sde().clone();
    for (_, pilot, _, illegal, _) in &mut victims {
        *illegal = if let (Some(attacker), Some(pilot)) = (attacker, *pilot) {
            crate::services::crimewatch::aggress(db, world, system, attacker, pilot).await?
        } else { false };
    }
    let db = db.clone();
    let deferred = guard.is_none();
    let result = world.with(system, move |s| {
        let valid_source = match guard {
            None => true,
            Some(DamageGuard::Player(master, generation)) => generation_valid(master, &generation)
                && attacker.is_some_and(|character| s.pilot_ball(character).is_some_and(|ball| ball.id == ship && ball.mode != MODE_WARP)),
            Some(DamageGuard::Npc(source, hostile, generation)) => s.npc_projection_valid(source, hostile, generation)
                && s.ball(source).is_some_and(|ball| ball.mode != MODE_WARP),
        };
        let completion = damage::apply_area_damage_in_actor(s, &db, system, weapon, &mut victims, application, facts, &sde, valid_source);
        if completion.attempt_finished { attempt.disarm(); }
        let result = match completion.fatal {
            Some(error) => Err(error),
            None => Ok((completion.receipts, completion.first_error, victims)),
        };
        (result, attempt)
    }).await.ok_or_else(|| CallError::Internal(anyhow::anyhow!("combat actor unavailable")))?;
    let (result, attempt) = result;
    if deferred && matches!(&result, Err(CallError::User { msg, dict })
        if msg == infinity_core::messages::MSG_CUSTOM_NOTIFY && *dict == vec![("notify".into(), marshal::Value::unicode(infinity_core::messages::effects::COMBAT_CAPACITY_FULL))]) {
        attempt.report_capacity_refusal();
    }
    let result = result?;
    let (receipts, mut first_error, victims) = result;
    for receipt in receipts {
        let result = receipt.await.map_err(|_| CallError::Internal(anyhow::anyhow!("combat loss worker stopped"))).and_then(|result| result);
        if let Err(error) = result { if first_error.is_none() { first_error = Some(error); } }
    }
    for (target, pilot, _, _, report) in victims {
        if let Some((quality, dealt)) = report {
            super::weapons::report_hit(world, ship, target, attacker, pilot, weapon, dealt, quality, false);
        }
    }
    match first_error { Some(error) => Err(error), None => Ok(()) }
}

pub(crate) async fn apply_npc(
    db: &Db, world: &Arc<World>, system: i64, _source_module: i64, me: &Ball, victim: &Ball,
    _effect: &Effect, _duration_ms: f64, generation: u64, attempt: PreparedCombatAttempt,
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
    apply_damage_guarded(db, world, system, weapon, targets, DamageApplication::Fixed(raw),
        Some(DamageGuard::Npc(source, hostile, generation)), attempt).await
}

pub(super) async fn validate(cx: &Ctx, _fit: &Fit, _module: &Module, _effect: &Effect, _target: Option<i64>) -> Result<(), CallError> {
    let ship = cx.ship_id;
    let mode = cx.world.with(cx.system_id, move |s| s.ball(ship).map(|b| b.mode)).await.flatten()
        .ok_or_else(|| user_error(messages::MSG_DENIED_ACTIVATE_TARGET_NOT_PRESENT, vec![]))?;
    if mode == MODE_WARP { return Err(user_error(messages::MSG_DENIED_ACTIVATE_IN_WARP, vec![])); }
    Ok(())
}

pub(super) async fn start(cx: &Ctx, cycle: &mut Cycle) -> Result<CycleOutcome, CallError> {
    let master = cycle.modules[0].id;
    let generation = cycle.stop.clone();
    for index in 0..cycle.modules.len() {
        let module = cycle.modules[index].clone();
        let Some(attempt) = lifecycle::combat_source::prepare_combat_source(cycle, module.id, Some(module.type_id)).await? else {
            return Ok(CycleOutcome::Stop("source lost or activation replaced"));
        };
        let Some(member) = cycle.inputs.members.get(index) else {
            return Err(CallError::Internal(anyhow::anyhow!("smartbomb cycle captured without member inputs")));
        };
        let MemberPacket::Smartbomb(inputs) = &member.packet else {
            return Err(CallError::Internal(anyhow::anyhow!("smartbomb cycle captured without smartbomb inputs")));
        };
        let (radius, raw) = (inputs.radius, inputs.raw);
        let ship = cx.ship_id;
        let check = generation.clone();
        let Some(targets) = cx.world.with(cx.system_id, move |s| {
            if !generation_valid(master, &check) { return None; }
            let source = s.sampled_ball(ship)?;
            if source.mode == MODE_WARP { return None; }
            Some(victims(s, ship, source.pos, radius))
        }).await.flatten() else { return Ok(CycleOutcome::Stop("source lost or in warp")); };
        apply_damage_guarded(&cx.db, &cx.world, cx.system_id, module.type_id, targets, DamageApplication::Fixed(raw),
            Some(DamageGuard::Player(master, generation.clone())), attempt).await?;
        if !generation_valid(master, &generation) { return Ok(CycleOutcome::Stop("activation replaced")); }
    }
    Ok(CycleOutcome::Continue)
}

#[cfg(all(test, feature = "tests-effects"))] mod tests;
