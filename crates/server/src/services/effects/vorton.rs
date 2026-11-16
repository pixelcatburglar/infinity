//! Vorton arcs using fitted dogma and the shared missile damage application.
//! Mirrors turrets.py and brawlChainWeaponEffect.py; arc range and count come
//! from SDE dogma. Per EVE University Vorton mechanics a hit uses missile
//! damage application per target: no tracking, falloff, turret chance, random
//! miss or wrecking roll. Victims are sampled per cycle; effects are
//! transient. Effect animations are published through the system's ordered
//! scene channel.
use super::lifecycle::{Cycle, CycleOutcome, Module};
use super::parameters::{MemberPacket, SourceInputs};
use super::space_source::{PreparedCombatAttempt, SourceContext, SourceGuard};
use super::registry::{Effect, EffectFlag};
use super::weapons;
use infinity_core::consts::{CATEGORY_DRONE, CATEGORY_ENTITY, CATEGORY_SHIP};
use infinity_core::messages;
use crate::services::combat::{self, Ctx, targeting_charges::targets_of};
use crate::services::fit::Fit;
use crate::services::{user_error, CallError};
use crate::space::{dist, drone::DronePhase, special_fx, FxModule, System, MODE_WARP};
use marshal::Value;
use std::sync::{Arc, atomic::AtomicBool};

/// SDE dogma VortonArcRange: distance from the original target.
pub(super) const ATTR_VORTON_ARC_RANGE: i32 = 3036;
/// SDE dogma VortonArcTargets: maximum number of targets including the primary.
pub(super) const ATTR_VORTON_ARC_TARGETS: i32 = 3037;
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

/// Scale one target's typed raw damage by the shared missile application:
/// that target's own effective signature and absolute sampled speed decide the
/// factor, so the primary and every arc target apply independently.
pub(super) fn vorton_application(
    raw: [f64; 4],
    target_signature: f64,
    target_absolute_speed: f64,
    aoe_cloud: f64,
    aoe_velocity: f64,
    drf: f64,
) -> [f64; 4] {
    let factor = weapons::missile_applied(target_signature, target_absolute_speed, aoe_cloud, aoe_velocity, drf);
    raw.map(|value| value * factor)
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
    let module_id = module.id;
    let charged = cx.world.with(cx.system_id, move |s| {
        s.loaded_effective(ship, module_id).is_some_and(|(_, _, quantity, pending)| quantity > 0 && !pending)
    }).await == Some(true);
    if !charged {
        return Err(user_error(messages::MSG_NO_CHARGES, vec![("launcher".into(), Value::tuple([Value::Int(UE_TYPEID), Value::Int(module.type_id as i64)]))]));
    }
    Ok(())
}

pub(super) async fn start(cx: &Ctx, cycle: &mut Cycle) -> Result<CycleOutcome, CallError> {
    let Some(primary) = cycle.target.filter(|t| targets_of(cx.char_id).contains(t)) else { return Ok(super::lifecycle::target_loss(cycle).await); };
    let master = cycle.modules[0].id;
    let generation = cycle.stop.clone();
    let mut fired = false;
    for index in 0..cycle.modules.len() {
        let module = cycle.modules[index].clone();
        let Some(attempt) = super::lifecycle::combat_source::prepare_combat_source(cycle, module.id, Some(module.type_id)).await? else {
            return Ok(CycleOutcome::Stop("source lost or activation replaced"));
        };
        if !generation_valid(master, &generation) { return Ok(CycleOutcome::Stop("activation replaced")); }
        let Some(member) = cycle.inputs.members.get_mut(index) else {
            return Err(CallError::Internal(anyhow::anyhow!("vorton cycle captured without member inputs")));
        };
        let (permit, token) = member.take_charge();
        let MemberPacket::Vorton(inputs) = &member.packet else {
            return Err(CallError::Internal(anyhow::anyhow!("vorton cycle captured without vorton inputs")));
        };
        let inputs = *inputs;
        let (range, count) = (inputs.arc_range, secondary_count(inputs.arc_targets));
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
        let Some(charge) = weapons::consume_charge(cx, permit, token).await else { continue; };
        cycle.charge_type = Some(charge);
        fired = true;
        super::aoe::apply_damage_guarded(&cx.db, &cx.world, cx.system_id, module.type_id, victims,
            super::aoe::DamageApplication::Vorton(inputs),
            Some(super::aoe::DamageGuard::Player(master, generation.clone())), attempt).await?;
        if let Some(guid) = cycle.effect.guid.as_deref().and_then(|g| g.split(',').next()).map(str::to_owned) {
            let info = Value::dict([("targets", Value::list(targets.iter().map(|(id, _)| Value::Int(*id))))]);
            let (id, type_id, duration, repeat, offensive) = (module.id, module.type_id, cycle.duration_ms, cycle.repeat, cycle.effect.has(EffectFlag::Offensive));
            cx.world.publish_destiny(cx.system_id, source, move |stamp| vec![special_fx(
                stamp, source, Some(FxModule { item: Some(id), type_id }), Some(primary), Some(i64::from(charge)),
                &guid, offensive, 1, 1, Value::Float(duration), repeat, None, 0, info)]);
        }
    }
    if fired { Ok(CycleOutcome::Continue) } else { Ok(CycleOutcome::Stop("out of charges")) }
}

/// NPC EntityChainLightning phase: the admitted attempt guards the source and
/// the hostile primary through `npc_projection_valid`. The primary is prepended
/// explicitly; every secondary reuses the broad smartbomb area policy
/// (inclusive radius around the primary, source excluded, no per-candidate
/// hostility, category or cloak filter), capped by the fitted arc count. A
/// stale source or target releases the attempt through the existing guard and
/// settlement paths and never applies damage.
pub(crate) async fn apply_source(
    cx: &SourceContext<'_>,
    attempt: Option<PreparedCombatAttempt>,
) -> Result<(), CallError> {
    if !matches!(cx.phase, DronePhase::Begin) { return Ok(()); }
    let SourceInputs::Vorton(inputs) = cx.inputs else {
        return Err(CallError::Internal(anyhow::anyhow!("vorton source captured without vorton inputs")));
    };
    let Some(attempt) = attempt else { return Ok(()) };
    let SourceGuard::Npc { source, target: hostile, generation } = cx.guard else {
        return Err(CallError::Internal(anyhow::anyhow!("vorton source admitted without an NPC guard")));
    };
    let inputs = *inputs;
    let Some(victims) = cx.world.with(cx.system, move |s| {
        if !s.npc_projection_valid(source, hostile, generation) { return None; }
        let me = s.sampled_ball(source)?;
        if me.mode == MODE_WARP { return None; }
        let primary = s.sampled_ball(hostile)?;
        let mut victims = vec![(hostile, primary.char_id, s.combat_membership_generation(hostile), false, None)];
        let mut secondary = super::aoe::victims(s, source, primary.pos, inputs.arc_range);
        secondary.retain(|(id, ..)| *id != hostile);
        victims.extend(secondary.into_iter().take(secondary_count(inputs.arc_targets)));
        Some(victims)
    }).await.flatten() else { return Ok(()) };
    super::aoe::apply_damage_guarded(cx.db, cx.world, cx.system, cx.source.type_id, victims,
        super::aoe::DamageApplication::Vorton(inputs),
        Some(super::aoe::DamageGuard::Npc(source, hostile, generation)), attempt).await
}

#[cfg(all(test, feature = "tests-effects"))] mod tests;
