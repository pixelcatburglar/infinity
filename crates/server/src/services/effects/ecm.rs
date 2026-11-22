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
use super::{EffectKind, lifecycle::{Cycle, CycleOutcome, Module}, parameters::{EcmInputs, MasterPacket, SourceInputs}, range::range_multiplier, registry::{Effect, EffectAttribute, EffectFlag}};
use super::space_source::{SourceContext, SourceGuard};
use infinity_core::consts::{dogma::ATTR_SCAN_GRAVIMETRIC_STRENGTH, dogma::ATTR_SCAN_LADAR_STRENGTH, dogma::ATTR_SCAN_MAGNETOMETRIC_STRENGTH, dogma::ATTR_SCAN_RADAR_STRENGTH, CATEGORY_SHIP, CATEGORY_ENTITY, CATEGORY_DRONE};
use crate::db::Db;
use infinity_core::messages;
use crate::sde::Sde;
use crate::services::{combat::{self, Ctx, targeting_charges::targets_of}, crimewatch, fit::{Fit, Key}, rand01, user_error, CallError};
use crate::space::{dist, Ball, System, World, MODE_WARP, drone::DronePhase};

/// Client dogma/const.py: racial ECM strengths in primary-sensor order.
pub(super) const JAM_STRENGTH: [i32; 4] = [238, 239, 240, 241];
/// Client dogma/const.py: primary sensor order.
const SENSORS: [i32; 4] = [ATTR_SCAN_GRAVIMETRIC_STRENGTH, ATTR_SCAN_LADAR_STRENGTH, ATTR_SCAN_MAGNETOMETRIC_STRENGTH, ATTR_SCAN_RADAR_STRENGTH];
/// Client dogma/const.py: ecmBurstRange.
pub(super) const BURST_RANGE: i32 = 142;
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
pub(super) fn selected_duration(fit: &Fit, key: Key, cycle: f64) -> f64 {
    choose_duration(fit.type_has_attribute(key, JAM_DURATION).then(|| fit.value(key, JAM_DURATION)), cycle)
}

pub(crate) fn npc_cycle_duration(sde: &Arc<Sde>, type_id: i32, effect: &Effect) -> Option<f64> {
    let fit = Fit::for_space_type(sde.clone(), type_id, Vec::new());
    let attr = effect.attribute(EffectAttribute::Duration)?;
    if !fit.type_has_attribute(Key::Ship, attr) { return None; }
    let value = fit.value(Key::Ship, attr);
    (value.is_finite() && value > 0.0).then_some(value)
}

/// Actor-side target admission shared by cold module validation and the
/// typed per-cycle preflight: source ball present and out of warp, target
/// eligible and inside scaled range.
async fn validate_target(cx: &Ctx, target: Option<i64>, optimal: f64, falloff: f64) -> Result<(), CallError> {
    let source = cx.ship_id;
    let sde = cx.world.sde().clone();
    let valid = cx.world.with(cx.system_id, move |s| {
        let Some(a) = s.sampled_ball(source) else { return false };
        if a.mode == MODE_WARP { return false; }
        target.is_none_or(|id| s.sampled_ball(id).is_some_and(|b| eligible(&sde, &b) && range_multiplier(gap(&a, &b), optimal, falloff) > 0.0))
    }).await.unwrap_or(false);
    if valid { Ok(()) } else { Err(missing()) }
}

pub(super) async fn validate(cx: &Ctx, fit: &Fit, module: &Module, effect: &Effect, target: Option<i64>) -> Result<(), CallError> {
    let target = if effect.kind == EffectKind::Ecm {
        Some(target.filter(|&id| id != cx.ship_id && targets_of(cx.char_id).contains(&id)).ok_or_else(missing)?)
    } else { None };
    // Cold validation consumes the shared cold extraction: duration takes the
    // effect's real Duration column when present (0 otherwise) and never
    // participates in target admission.
    let duration = effect.attribute(EffectAttribute::Duration).map_or(0.0, |a| fit.value(Key::Module(module.id), a));
    let src = source(fit, Key::Module(module.id), cx.ship_id, module.id, effect, duration);
    validate_target(cx, target, src.optimal, src.falloff).await
}

/// Per-cycle admission from the captured packet; the range values come from
/// the resident fit's capture, not a fresh database read.
pub(super) async fn validate_cycle(cx: &Ctx, cycle: &Cycle) -> Result<(), CallError> {
    let Some(MasterPacket::Ecm(inputs)) = cycle.inputs.frame.as_ref().map(|frame| &frame.packet) else {
        return Err(CallError::Internal(anyhow::anyhow!("ecm cycle captured without ecm inputs")));
    };
    let target = if cycle.effect.kind == EffectKind::Ecm {
        Some(cycle.target.filter(|&id| id != cx.ship_id && targets_of(cx.char_id).contains(&id)).ok_or_else(missing)?)
    } else { None };
    validate_target(cx, target, inputs.ranges.optimal, inputs.ranges.falloff).await
}

#[derive(Clone)]
struct Source { id: i64, type_id: Option<i32>, module: i64, optimal: f64, falloff: f64, strengths: [f64; 4], duration: f64, resistance_attribute: Option<i32>, burst: bool, offensive: bool, revision: Option<u64>, owner_revision: Option<(i64, u64)> }
fn from_inputs(inputs: &EcmInputs, id: i64, module: i64, effect: &Effect) -> Source {
    Source { id, type_id: inputs.source_type, module, optimal: inputs.ranges.optimal, falloff: inputs.ranges.falloff, strengths: inputs.strengths, duration: inputs.duration_ms, resistance_attribute: inputs.resistance_attribute, burst: effect.kind == EffectKind::BurstJammer, offensive: effect.has(EffectFlag::Offensive), revision: inputs.projection_revision, owner_revision: inputs.owner_projection }
}
fn source(fit: &Fit, key: Key, id: i64, module: i64, effect: &Effect, cycle: f64) -> Source {
    from_inputs(&super::parameters::capture_ecm(fit, key, effect, cycle), id, module, effect)
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

/// Everything the first admission snapshot hands to the actor turn.
struct TargetGate {
    victim: Ball,
    membership: Option<u64>,
    owner: Option<i64>,
    player: Option<(i64, Arc<std::sync::atomic::AtomicBool>)>,
    guard: Option<SourceGuard>,
    roll: f64,
}

/// The original actor-side application, executed inside the system actor's
/// own turn: the admitted body, current defense and one roll decide the jam.
fn apply_one_in_actor(s: &mut System, sde: &Arc<Sde>, src: &Source, gate: TargetGate) -> Option<Option<i64>> {
    let id = gate.victim.id;
    if gate.owner.is_some_and(|owner| s.drone(id).is_none_or(|d| d.owner != owner)) { return None; }
    // The admitted target must still be the same body instance: a removed
    // and re-added ball with a matching owner/type/char carries a new
    // membership and is rejected before the gate runs. Drone record
    // generation is per-order and stays out of this binding.
    if s.combat_membership_generation(id) != gate.membership { return None; }
    // Resident targets read the current fit through the gate: a piloted
    // ship, an owned drone, or a persistent unpiloted hull. NPCs and
    // unowned drones evaluate the type-only snapshot as before; a gate
    // miss for a resident identity is a refusal, never a fallback.
    let drone = s.drone(id);
    let resident = gate.victim.char_id.is_some()
        || drone.as_ref().is_some_and(|drone| drone.owner != infinity_core::consts::OWNER_UNOWNED)
        || (drone.is_none() && s.persistent_generation(id).is_some());
    let (values, resistance, revision, owner_revision) = if resident {
        let Some(captured) = s.with_combat_fit(id, |fit, key, _| (
            SENSORS.map(|a| fit.value(key, a)),
            src.resistance_attribute.map(|attribute| fit.value(key, attribute)),
            if matches!(key, Key::Drone(_)) { fit.drone_projection_revision() } else { fit.projection_revision() },
            if matches!(key, Key::Drone(_)) { fit.ship_id().zip(fit.projection_revision()) } else { None },
        )) else { return None };
        captured
    } else {
        let projections = s.projections_on(id);
        let fit = Fit::for_space_type(sde.clone(), gate.victim.type_id, projections);
        (SENSORS.map(|a| fit.value(Key::Ship, a)), src.resistance_attribute.map(|attribute| fit.value(Key::Ship, attribute)), Some(s.projection_revision(id)), None)
    };
    let (index, defense) = primary(values).unwrap_or((0, 0.0));
    let strength = src.strengths[index] * resistance.unwrap_or(1.0);
    let Some((_, _, multiplier)) = current_attempt(s, sde, src, &gate.victim, revision, owner_revision, &gate.player, gate.guard) else { return None };
    if src.offensive { s.drone_attack(src.id, id); }
    if gate.roll < chance(strength, defense, multiplier) && src.duration.is_finite() && src.duration > 0.0 {
        combat::jamming::apply(s, src.id, src.module, id, src.duration);
    }
    Some(s.combat_owner(id))
}

async fn apply_one(_db: &Db, world: &Arc<World>, system: i64, src: &Source, victim: &Ball, player: Option<(i64, Arc<std::sync::atomic::AtomicBool>)>, guard: Option<SourceGuard>) -> Result<Option<Option<i64>>, CallError> {
    let id = victim.id;
    let (record, membership) = world.with(system, move |s| (s.drone(id), s.combat_membership_generation(id))).await.unwrap_or((None, None));
    let src = src.clone();
    let sde = world.sde().clone();
    let gate = TargetGate { victim: victim.clone(), membership, owner: record.map(|d| d.owner), player, guard, roll: rand01() };
    Ok(world.with(system, move |s| apply_one_in_actor(s, &sde, &src, gate)).await.flatten())
}

pub(super) async fn start(cx: &Ctx, cycle: &mut Cycle) -> Result<CycleOutcome, CallError> {
    let module = &cycle.modules[0];
    let Some(MasterPacket::Ecm(inputs)) = cycle.inputs.frame.as_ref().map(|frame| &frame.packet) else {
        return Err(CallError::Internal(anyhow::anyhow!("ecm cycle captured without ecm inputs")));
    };
    let src = from_inputs(inputs, cx.ship_id, module.id, &cycle.effect);
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
    let SourceInputs::Ecm(inputs) = cx.inputs else {
        return Err(CallError::Internal(anyhow::anyhow!("ecm source captured without ecm inputs")));
    };
    let src = from_inputs(inputs, cx.source.id, cx.module, cx.effect);
    let admitted = apply_one(cx.db, cx.world, cx.system, &src, cx.target, None, Some(cx.guard)).await?;
    if src.offensive {
        if let (Some(attacker), Some(Some(victim))) = (cx.attacker, admitted) {
            crimewatch::aggress(cx.db, cx.world, cx.system, attacker, victim).await?;
        }
    }
    Ok(())
}

#[cfg(all(test, feature = "tests-effects"))] pub(in crate::services::effects) mod tests;
