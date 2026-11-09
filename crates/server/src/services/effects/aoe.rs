//! Player and NPC smartbomb radius damage from fitted dogma attributes.
//! Mirrors EMPWave.py's empFieldRange; candidates use actor-sampled positions and
//! damage uses the shared combat pipeline. Effects and damage are transient.
use super::lifecycle::{self, Cycle, CycleOutcome, Module};
use super::registry::Effect;
use super::space_source::PreparedCombatAttempt;
use infinity_core::consts::dogma::ATTR_EMP_FIELD_RANGE;
use infinity_core::messages;
use crate::db::Db;
use crate::services::combat::{self, Ctx, ATTR_DAMAGE};
use crate::services::combat::damage::{DamageSource, complete_damage_outcome};
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

pub(super) async fn apply_damage_guarded(
    db: &Db, world: &Arc<World>, system: i64, weapon: i32,
    mut victims: Vec<(i64, Option<i64>, Option<u64>, bool, Option<(i64, f64)>)>, raw: [f64; 4],
    guard: Option<DamageGuard>, mut attempt: PreparedCombatAttempt,
    turret: Option<(f64, f64, f64, f64)>,
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
        let result = (|| {
        let valid_source = match guard {
            None => true,
            Some(DamageGuard::Player(master, generation)) => generation_valid(master, &generation)
                && attacker.is_some_and(|character| s.pilot_ball(character).is_some_and(|ball| ball.id == ship && ball.mode != MODE_WARP)),
            Some(DamageGuard::Npc(source, hostile, generation)) => s.npc_projection_valid(source, hostile, generation)
                && s.ball(source).is_some_and(|ball| ball.mode != MODE_WARP),
        };
        if !valid_source { return Ok((Vec::new(), None, Vec::new())); }
        s.observe_combat_utc().map_err(|error| match error {
            crate::space::combat_facts::Refusal::Full => crate::services::combat::capacity::refusal(system),
            other => CallError::Internal(anyhow::anyhow!("area boundary refusal: {other:?}")),
        })?;
        let at = s.combat_observed_time();
        victims.sort_unstable_by_key(|entry| entry.0);
        let mut targets = Vec::with_capacity(victims.len());
        for (target, pilot, membership, illegal, report) in &mut victims {
            let (target, pilot, membership, illegal) = (*target, *pilot, *membership, *illegal);
            if !s.ball(target).is_some_and(|ball| ball.char_id == pilot)
                || s.combat_membership_generation(target) != membership { continue; }
            if s.is_damage_immune(target) { continue; }
            let raw = if let Some((optimal, falloff, tracking, signature)) = turret {
                let Some(source) = s.sampled_ball(ship) else { continue };
                let Some(victim) = s.sampled_ball(target) else { continue };
                let chance = super::weapons::turret_chance(&source, &victim, optimal, falloff, tracking,
                    signature, super::weapons::target_signature(&sde, &victim));
                let multiplier = super::weapons::turret_roll(chance);
                *report = Some((super::weapons::hit_quality(multiplier), 0.0));
                if multiplier <= 0.0 { continue; }
                raw.map(|value| value * multiplier)
            } else {
                *report = Some((super::weapons::hit_quality(1.0), 0.0));
                raw
            };
            let source = DamageSource { attacker_id: attacker, attacker_ball_id: Some(ship),
                source_ship_id: Some(ship), source_ship_type: Some(facts.entity.type_id),
                weapon_type_id: Some(weapon), facts: Some(facts), force_origin: None, illegal };
            match s.plan_area_damage(target, raw, source, at) {
                Some(plan) => targets.push(plan),
                None if matches!(s.combat_phase(target), Some(crate::space::combat::CombatPhase::Live)) =>
                    return Err(CallError::Internal(anyhow::anyhow!("area damage plan failed for live target"))),
                None => { *report = None; }
            }
        }
        targets.sort_unstable_by_key(|target| (target.target.item_id, target.target.incarnation));
        let applied = s.apply_area_damage(facts, &mut targets, at, |_| valid_source);
        attempt.disarm();
        match applied {
            Ok(false) => return Ok((Vec::new(), None, victims)),
            Err(crate::space::combat_facts::Refusal::Full) => return Err(crate::services::combat::capacity::refusal(system)),
            Err(crate::space::combat_facts::Refusal::Replay | crate::space::combat_facts::Refusal::Busy) => return Ok((Vec::new(), None, Vec::new())),
            Err(error) => return Err(CallError::Internal(anyhow::anyhow!("area combat refusal: {error:?}"))),
            Ok(true) => {}
        }
        let mut receipts = Vec::new();
        let mut first_error = None;
        for target in &mut targets {
            let Some(outcome) = target.payload.outcome.take() else { continue };
            if let Ok(index) = victims.binary_search_by_key(&target.target.item_id, |entry| entry.0) {
                if let Some((_, dealt)) = victims[index].4.as_mut() { *dealt = outcome.dealt; }
            }
            match complete_damage_outcome(s, &db, system, target.target.item_id, outcome) {
                Ok((_, Some(receipt))) => receipts.push(receipt),
                Ok((_, None)) => {},
                Err(error) => if first_error.is_none() { first_error = Some(error); },
            }
        }
        Ok((receipts, first_error, victims))
        })();
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
            super::weapons::report_hit(world, ship, target, attacker, pilot, weapon, dealt, quality);
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
    apply_damage_guarded(db, world, system, weapon, targets, raw,
        Some(DamageGuard::Npc(source, hostile, generation)), attempt, None).await
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
    for index in 0..cycle.modules.len() {
        let module = cycle.modules[index].clone();
        let Some(attempt) = lifecycle::prepare_combat_source(cycle, module.id, Some(module.type_id)).await? else {
            return Ok(CycleOutcome::Stop("source lost or activation replaced"));
        };
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
        apply_damage_guarded(&cx.db, &cx.world, cx.system_id, module.type_id, targets, raw,
            Some(DamageGuard::Player(master, generation.clone())), attempt, None).await?;
        if !generation_valid(master, &generation) { return Ok(CycleOutcome::Stop("activation replaced")); }
    }
    Ok(CycleOutcome::Continue)
}

#[cfg(test)] mod tests;
