//! Turret accuracy and missile launches using fitted dogma, module-owned
//! damage for turrets with no charge compatibility, and charge data otherwise.
//! Mirrors clientDogmaLocation and michelle, with accuracy from native GetAccuracy
//! and SDE attributes; missiles fly as actor-owned native-steered flights that
//! detonate on swept target contact or expire at their effective lifetime.
//! Admitted combat source identities remain frozen until their damage is resolved.
//! Fitted player turret banks fire as one volley: the master's captured roll
//! inputs, summed member raw damage and one `isBanked` report per observer.
//! The shared precursor-turret spool binds once per activation to a target
//! incarnation and resolves wholly inside the final actor turn.
use super::lifecycle::Module;
use super::parameters::{MissileInputs, SourceInputs, TurretInputs};
use super::space_source::PreparedCombatAttempt;
use super::spool::{SpoolBinding, SpoolParameters, SpoolState};
use crate::services::combat::{quantity_change, state, Ctx, ATTR_DAMAGE};
use crate::services::combat::damage::{apply_damage_guarded, apply_damage_in_actor, DamageSource};
use crate::services::combat::targeting_charges::targets_of;
use crate::services::fit::{Fit, Key};
use crate::services::{rand01, CallError};
use infinity_core::consts::{dogma::ATTR_DAMAGE_MULTIPLIER, dogma::ATTR_FALLOFF, dogma::ATTR_MAX_RANGE, dogma::ATTR_OPTIMAL_SIG_RADIUS, dogma::ATTR_SIGNATURE_RADIUS, dogma::ATTR_TRACKING_SPEED};
use crate::db::Db;
use crate::space::combat::inputs::ChargeToken;
use crate::space::combat::loaded::LoadedPermit;
use crate::space::combat::CombatPhase;
use crate::sde::Sde;
use crate::space::{dist, now_ms, Ball, System, World, MODE_WARP};
use marshal::Value;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::Arc;

pub(in crate::services::effects) mod bank;

/// SDE signatureRadius default when the attribute metadata is unavailable.
const DEFAULT_SIGNATURE_RADIUS: f64 = 100.0;
/// Existing turret falloff hit-chance formula.
const TURRET_FALLOFF_CHANCE: f64 = 0.5;
/// Existing turret wrecking-hit probability.
const WRECKING_CHANCE: f64 = 0.01;
/// Existing turret wrecking-hit damage multiplier.
const WRECKING_DAMAGE_MULTIPLIER: f64 = 3.0;
/// Existing turret normal-hit damage formula.
const NORMAL_HIT_MULTIPLIER_OFFSET: f64 = 0.49;
/// Existing combat-log quality IDs and thresholds.
const HIT_MISS: i64 = 0;
/// Existing combat-log grazing quality.
const HIT_GRAZING: i64 = 1;
/// Existing combat-log glancing quality.
const HIT_GLANCING: i64 = 2;
/// Existing combat-log normal quality.
const HIT_NORMAL: i64 = 3;
/// Existing combat-log good quality.
const HIT_GOOD: i64 = 4;
/// Existing combat-log excellent quality.
const HIT_EXCELLENT: i64 = 5;
/// Existing combat-log wrecking quality.
const HIT_WRECKING: i64 = 6;
/// Existing combat-log wrecking threshold.
const HIT_WRECKING_THRESHOLD: f64 = 2.9;
/// Existing combat-log grazing threshold.
const HIT_GRAZING_THRESHOLD: f64 = 0.6;
/// Existing combat-log glancing threshold.
const HIT_GLANCING_THRESHOLD: f64 = 0.8;
/// Existing combat-log normal threshold.
const HIT_NORMAL_THRESHOLD: f64 = 1.0;
/// Existing combat-log good threshold.
const HIT_GOOD_THRESHOLD: f64 = 1.2;

/// Server design decision: missile IDs clear NPCs (9.0e12), exploration sites (9.5e12), probes (9.6e12), and asteroids (9.7e12).
const MISSILE_ID_START: i64 = 9_800_000_000_000;
/// Temporary item IDs for missiles in flight.
static NEXT_MISSILE_ID: AtomicI64 = AtomicI64::new(MISSILE_ID_START);

/// Allocate from the shared temporary projectile namespace.
pub(super) fn next_missile_id() -> i64 {
    NEXT_MISSILE_ID.fetch_add(1, Ordering::Relaxed)
}

/// Pay one admitted loaded use through the actor: the member's captured
/// permit and charge token debit one unit, remove an emptied node in the same
/// turn and emit the quantity delta; any refusal releases the permit unpaid.
pub(super) async fn consume_charge(cx: &Ctx, permit: Option<LoadedPermit>, token: Option<ChargeToken>) -> Option<i32> {
    let Some(permit) = permit else { return None };
    let flag = permit.flag;
    let Some(token) = token else {
        cx.world.post(cx.system_id, move |s| s.release_loaded_use(permit));
        return None;
    };
    let (type_id, remaining) = cx.world.with(cx.system_id, move |s| match s.consume_fitted_loaded(permit, token) {
        done @ Some(_) => done,
        None => { s.release_loaded_use(permit); None }
    }).await.flatten()?;
    cx.notify("OnModuleAttributeChanges", vec![quantity_change(cx.char_id, cx.ship_id, flag, type_id, remaining, remaining + 1)]);
    Some(type_id)
}

/// An uncharged ancillary cycle may proceed only while the actor proves the
/// slot has no quantity and no pending, held or unknown race on it.
pub(super) async fn empty_charge(cx: &Ctx, module: &Module) -> bool {
    let (ship, module) = (cx.ship_id, module.id);
    cx.world.with(cx.system_id, move |s| s.loaded_empty_proof(ship, module)).await.unwrap_or(false)
}

/// The game's hit quality for the combat log: 0 miss .. 6 wrecking.
pub(crate) fn hit_quality(multiplier: f64) -> i64 {
    match multiplier {
        m if m <= 0.0 => HIT_MISS,
        m if m >= HIT_WRECKING_THRESHOLD => HIT_WRECKING,
        m if m < HIT_GRAZING_THRESHOLD => HIT_GRAZING,
        m if m < HIT_GLANCING_THRESHOLD => HIT_GLANCING,
        m if m < HIT_NORMAL_THRESHOLD => HIT_NORMAL,
        m if m < HIT_GOOD_THRESHOLD => HIT_GOOD,
        _ => HIT_EXCELLENT,
    }
}

/// Center distance, clamped surface distance, relative speed and squared angular velocity.
pub(crate) fn turret_geometry(src: &Ball, dst: &Ball) -> (f64, f64, f64, f64) {
    let r = [dst.pos[0] - src.pos[0], dst.pos[1] - src.pos[1], dst.pos[2] - src.pos[2]];
    let distance = dist(src.pos, dst.pos);
    let surface = ((distance - dst.radius as f64) - src.radius as f64).max(0.0);
    let v = [dst.vel[0] - src.vel[0], dst.vel[1] - src.vel[1], dst.vel[2] - src.vel[2]];
    let speed_squared = v.iter().map(|x| x * x).sum::<f64>();
    let angular_squared = if distance == 0.0 { 0.0 } else {
        let radial_speed = v.iter().zip(r).map(|(a, b)| a * b).sum::<f64>() / distance;
        ((speed_squared - radial_speed * radial_speed) / (distance * distance)).max(0.0)
    };
    (distance, surface, speed_squared.sqrt(), angular_squared)
}

/// Native GetAccuracy uses center-distance angular velocity and surface range.
/// NPC deviations: zero falloff is a range step, and zero tracking ignores angular velocity.
pub(crate) fn turret_chance(src: &Ball, dst: &Ball, optimal: f64, falloff: f64, tracking: f64, opt_sig: f64, dst_sig: f64) -> f64 {
    let (distance, surface, _, angular_squared) = turret_geometry(src, dst);
    if src.id == dst.id || optimal < 0.0 || distance == 0.0 { return 0.0; }
    let angular = if tracking == 0.0 { 0.0 } else { angular_squared * (opt_sig / (tracking * dst_sig)).powi(2) };
    let excess = (surface - optimal).max(0.0);
    if falloff == 0.0 && excess > 0.0 { return 0.0; }
    let range = if falloff == 0.0 { 0.0 } else { excess / falloff };
    TURRET_FALLOFF_CHANCE.powf(angular + range * range)
}

/// Resolve the effective cached signature, or the type and SDE attribute default.
pub(crate) fn target_signature(sde: &Sde, target: &Ball) -> f64 {
    target.signature_radius.filter(|v| *v > 0.0)
        .or_else(|| sde.types.attribute(target.type_id, ATTR_SIGNATURE_RADIUS).filter(|v| *v > 0.0))
        .or_else(|| sde.dogma.attribute(ATTR_SIGNATURE_RADIUS).and_then(|a| a.default_value).filter(|v| *v > 0.0))
        .unwrap_or(DEFAULT_SIGNATURE_RADIUS)
}

/// Missile damage fraction using the target's absolute impact-time speed.
pub(crate) fn missile_applied(dst_sig: f64, dst_speed: f64, aoe_cloud: f64, aoe_velocity: f64, drf: f64) -> f64 {
    let sig = dst_sig / aoe_cloud.max(1.0);
    let velocity = if dst_speed == 0.0 || aoe_velocity == 0.0 { 1.0 } else { (sig * aoe_velocity / dst_speed).powf(drf) };
    1.0_f64.min(sig).min(velocity)
}

/// A turret's damage multiplier: miss (0), 0.49..1.49, or a 1% wrecking 3x.
pub(crate) fn turret_roll(chance: f64) -> f64 {
    let roll = rand01();
    if roll >= chance {
        0.0
    } else if roll < WRECKING_CHANCE {
        WRECKING_DAMAGE_MULTIPLIER
    } else {
        roll + NORMAL_HIT_MULTIPLIER_OFFSET
    }
}

/// `OnDamageMessage` for the shooter (`target`) or the victim (`source`).
pub(super) fn damage_message(key: &str, other: i64, weapon: i32, damage: f64, quality: i64, banked: bool) -> Value {
    Value::dict([
        (key, Value::Int(other)),
        ("weapon", Value::Int(weapon as i64)),
        ("damage", Value::Float(damage)),
        ("hitQuality", Value::Int(quality)),
        ("isBanked", Value::Bool(banked)),
        // uilog: "me" picks AttackHits ("to {target}"), "otherPlayerWeapons" AttackHitsR
        // ("from {source}").
        ("attackType", Value::str(if key == "source" { "otherPlayerWeapons" } else { "me" })),
        ("damageTypes", Value::Dict(vec![])),
    ])
}

/// Tell both sides of a hit: the shooter (if a pilot) and the victim (if a pilot).
pub(crate) fn report_hit(world: &World, shooter: i64, target: i64, shooter_pilot: Option<i64>, target_pilot: Option<i64>, weapon: i32, damage: f64, quality: i64, banked: bool) {
    if let Some(ch) = shooter_pilot {
        world.notify_pilot(ch, "OnDamageMessage", vec![damage_message("target", target, weapon, damage, quality, banked)]);
    }
    if let Some(ch) = target_pilot {
        world.notify_pilot(ch, "OnDamageMessage", vec![damage_message("source", shooter, weapon, damage, quality, banked)]);
    }
}

/// The shared fitted-source check inside the actor: master's activation
/// generation owns the activation, the pilot flies a live, unwarped ship and
/// the admitted source membership stands. Singleton shots and bank volleys
/// share this one predicate.
pub(crate) fn fitted_source_current(s: &System, ship: i64, master: i64, pilot: i64, stop: &Arc<AtomicBool>, source_membership: Option<u64>) -> bool {
    !stop.load(Ordering::Relaxed)
        && state().active.get(&master).is_some_and(|active|
            active.char_id == pilot && !active.forced_inactive && Arc::ptr_eq(&active.stop, stop))
        && s.combat_membership_generation(ship) == source_membership
        && s.combat_phase(ship) == Some(CombatPhase::Live)
        && s.pilot_ball(pilot).is_some_and(|ball| ball.id == ship && ball.mode != MODE_WARP)
}

/// The shared locked-target check: the frozen combat generation, still live
/// (not dying, transitioning or replaced) and still locked by the pilot.
pub(crate) fn fitted_target_current(s: &System, pilot: i64, target: i64, target_generation: u64) -> bool {
    s.combat_membership_generation(target) == Some(target_generation)
        && s.combat_phase(target) == Some(CombatPhase::Live)
        && targets_of(pilot).contains(&target)
}

/// The typed spool context a turret shot carries between its admission and
/// the final actor turn. `Fitted` borrows the member buffer's bound state so
/// the caller can write the advanced ramp back; `Npc` reads and commits the
/// brain-owned ramp inside the actor, so its result carries no state.
pub(crate) enum TurretSpool {
    Fitted {
        binding: SpoolBinding,
        module: i64,
        master: i64,
        pilot: i64,
        stop: Arc<AtomicBool>,
        parameters: SpoolParameters,
        state: SpoolState,
    },
    Npc { binding: SpoolBinding, parameters: SpoolParameters },
}

/// A fitted member's committed ramp after a fired shot: the advanced pure
/// state and the bonus this shot actually applied.
pub(crate) struct FittedSpoolResult {
    pub(crate) state: SpoolState,
    pub(crate) applied_bonus: f64,
}

/// How one turret shot ended: only an admitted, rolled shot counts as fired —
/// a genuine miss and a fully resisted accepted hit both advance the ramp,
/// while guard refusal, an unadmitted source, a replaced incarnation or an
/// out-of-optimal gap do not.
pub(crate) enum TurretResolution {
    NotFired,
    Fired { fitted_spool: Option<FittedSpoolResult> },
}

/// Resolve and apply one source-neutral turret shot.
pub(crate) async fn turret_shot(db: &Db, world: &Arc<World>, system: i64, source: &Ball, target: &Ball,
    attacker: Option<i64>, weapon: i32, illegal: bool, parameters: (f64, f64, f64, f64), raw: [f64; 4],
    guard: Option<super::space_source::SourceGuard>, spool: Option<TurretSpool>, attempt: PreparedCombatAttempt) -> Result<TurretResolution, CallError> {
    turret_shot_with_roll(db, world, system, source, target, attacker, weapon, illegal, parameters, raw, guard, spool, attempt, turret_roll).await
}

/// The shared single-shot core behind `turret_shot`; the roll step is a plain
/// injected call so singleton tests can steer it without any global hook.
/// A spooled shot resolves entirely inside one actor turn; a plain shot keeps
/// the original sample-then-guarded-apply shape.
#[allow(clippy::too_many_arguments)]
async fn turret_shot_with_roll(db: &Db, world: &Arc<World>, system: i64, source: &Ball, target: &Ball,
    attacker: Option<i64>, weapon: i32, illegal: bool, parameters: (f64, f64, f64, f64), raw: [f64; 4],
    guard: Option<super::space_source::SourceGuard>, spool: Option<TurretSpool>, mut attempt: PreparedCombatAttempt,
    roll: impl FnOnce(f64) -> f64 + Send + 'static) -> Result<TurretResolution, CallError> {
    if let Some(spool) = spool {
        return turret_spool_shot(db, world, system, source, target, weapon, illegal, parameters, raw, guard, spool, attempt, roll).await;
    }
    let (optimal, falloff, tracking, signature) = parameters;
    let source_id = source.id;
    let target_id = target.id;
    let facts = attempt.facts();
    let shooter_pilot = facts.actual_actor_character;
    let Some(Some((sampled_source, sampled_target, victim_pilot, target_generation))) = world.with(system, move |s| {
        if guard.is_some_and(|guard| !guard.valid(s)) { return None; }
        let source = s.sampled_ball(source_id)?;
        let target = s.sampled_ball(target_id)?;
        let generation = s.combat_membership_generation(target_id)?;
        let victim = s.combat_owner(target_id);
        s.drone_attack(source_id, target_id);
        Some((source, target, victim, generation))
    }).await else { return Ok(TurretResolution::NotFired) };
    let chance = turret_chance(&sampled_source, &sampled_target, optimal, falloff, tracking, signature, target_signature(world.sde(), &sampled_target));
    let multiplier = roll(chance);
    let _ = attacker;
    let mut illegal = illegal;
    if matches!(guard, Some(super::space_source::SourceGuard::Drone { .. })) {
        if let (Some(shooter), Some(victim)) = (shooter_pilot, victim_pilot) {
            illegal = crate::services::crimewatch::aggress(db, world, system, shooter, victim).await?;
        }
    }
    let dealt = if multiplier > 0.0 {
        let outcome = apply_damage_guarded(db, world, system, target_id, raw.map(|d| d * multiplier), DamageSource {
            attacker_id: shooter_pilot, attacker_ball_id: Some(facts.entity.item_id),
            source_ship_id: Some(facts.entity.item_id), source_ship_type: Some(facts.entity.type_id),
            weapon_type_id: Some(weapon), facts: Some(facts), force_origin: None, illegal,
        }, false, move |s| s.combat_membership_generation(target_id) == Some(target_generation)
            && guard.is_none_or(|guard| guard.valid(s))).await?;
        match outcome {
            // A guard, target or damage refusal is not a shot: the attempt
            // drops with its normal finish semantics and nothing reports.
            None => return Ok(TurretResolution::NotFired),
            Some(outcome) => { attempt.disarm(); outcome.dealt }
        }
    } else { 0.0 };
    report_hit(world, source_id, target_id, shooter_pilot, victim_pilot, weapon, dealt, hit_quality(multiplier), false);
    Ok(TurretResolution::Fired { fitted_spool: None })
}

/// Resolve one spooling turret shot inside a single actor turn: the bound
/// identities, sampled geometry, bonus-scaled roll, damage application and
/// ramp commit share the turn, so a queued successor can never observe or
/// spend a stale bonus. The member's ramp bonus scales its own raw before the
/// roll multiplier; the turn's surface gap gates the beam at `optimal` rather
/// than degrading into a zero chance roll. A genuine miss and a fully
/// resisted accepted hit both count as fired and advance the ramp.
#[allow(clippy::too_many_arguments)]
async fn turret_spool_shot(
    db: &Db, world: &Arc<World>, system: i64, source: &Ball, target: &Ball,
    weapon: i32, illegal: bool, parameters: (f64, f64, f64, f64), raw: [f64; 4],
    guard: Option<super::space_source::SourceGuard>, mut spool: TurretSpool,
    mut attempt: PreparedCombatAttempt,
    roll: impl FnOnce(f64) -> f64 + Send + 'static,
) -> Result<TurretResolution, CallError> {
    let source_id = source.id;
    let target_id = target.id;
    let facts = attempt.facts();
    let shooter_pilot = facts.actual_actor_character;
    let sde = world.sde().clone();
    let db_worker = db.clone();
    /// The actor turn's verdict carried back to the worker: the typed
    /// resolution, the report values and any loss receipt to await outside.
    struct Turn {
        resolution: TurretResolution,
        dealt: f64,
        multiplier: f64,
        victim_pilot: Option<i64>,
        receipt: Option<tokio::sync::oneshot::Receiver<Result<(), CallError>>>,
    }
    let turn = world.with(system, move |s| -> Result<Turn, CallError> {
        // An NPC bound shot that cannot fire ends its ramp and beam inside
        // this same turn: nothing queues a reset behind a later shot.
        let npc_binding = match &spool { TurretSpool::Npc { binding, .. } => Some(*binding), _ => None };
        let not_fired = |attempt: &mut PreparedCombatAttempt, s: &mut crate::space::System| {
            if let Some(binding) = npc_binding { s.npc_spool_reset(source_id, binding); }
            attempt.finish_in_actor(s);
            Turn { resolution: TurretResolution::NotFired, dealt: 0.0, multiplier: 0.0, victim_pilot: None, receipt: None }
        };
        if guard.is_some_and(|guard| !guard.valid(s)) { return Ok(not_fired(&mut attempt, s)); }
        // The bound identity is the only ramp authority; current membership
        // values are compared, never rebound.
        let bonus = match &mut spool {
            TurretSpool::Fitted { binding, module, master, pilot, stop, parameters, state, .. } => {
                // The singleton path only fires the activation's own master.
                debug_assert_eq!(module, master);
                if binding.target != target_id
                    || !fitted_source_current(s, source_id, *master, *pilot, stop, Some(binding.source_membership))
                    || !fitted_target_current(s, *pilot, target_id, binding.target_membership) {
                    return Ok(not_fired(&mut attempt, s));
                }
                state.current(*parameters)
            }
            TurretSpool::Npc { binding, parameters } => {
                if binding.target != target_id { return Ok(not_fired(&mut attempt, s)); }
                match s.npc_spool_current(source_id, *binding, *parameters) {
                    Some(bonus) => bonus,
                    None => return Ok(not_fired(&mut attempt, s)),
                }
            }
        };
        let Some((sampled_source, sampled_target)) = s.sampled_ball(source_id).zip(s.sampled_ball(target_id)) else {
            return Ok(not_fired(&mut attempt, s));
        };
        let (_, surface, _, _) = turret_geometry(&sampled_source, &sampled_target);
        if surface > parameters.0 { return Ok(not_fired(&mut attempt, s)); }
        s.drone_attack(source_id, target_id);
        let chance = turret_chance(&sampled_source, &sampled_target, parameters.0, parameters.1, parameters.2, parameters.3,
            target_signature(&sde, &sampled_target));
        let multiplier = roll(chance);
        let victim_pilot = s.combat_owner(target_id);
        let (dealt, receipt) = if multiplier > 0.0 {
            // Once invoked, the helper finishes this attempt's token on every
            // return; a fired attempt is disarmed, never finished twice.
            match apply_damage_in_actor(s, &db_worker, system, target_id,
                raw.map(|damage| damage * (1.0 + bonus) * multiplier), DamageSource {
                    attacker_id: shooter_pilot, attacker_ball_id: Some(facts.entity.item_id),
                    source_ship_id: Some(facts.entity.item_id), source_ship_type: Some(facts.entity.type_id),
                    weapon_type_id: Some(weapon), facts: Some(facts), force_origin: None, illegal,
                }, false)
            {
                Ok(Some((outcome, receipt))) => (outcome.dealt, receipt),
                // The helper already finished the token; disarm it without
                // finishing twice and report the refusal as not fired.
                Ok(None) => { attempt.disarm(); return Ok(not_fired(&mut attempt, s)); },
                Err(error) => { attempt.disarm(); return Err(error); }
            }
        } else {
            // A genuine miss still fires and reports zero damage.
            attempt.finish_in_actor(s);
            (0.0, None)
        };
        // The accepted point advances the ramp exactly once, in the same turn.
        let fitted_spool = match spool {
            TurretSpool::Fitted { mut state, parameters, .. } => {
                state.advance(parameters);
                Some(FittedSpoolResult { state, applied_bonus: bonus })
            }
            TurretSpool::Npc { binding, parameters } => {
                s.npc_spool_advance(source_id, binding, parameters);
                None
            }
        };
        if multiplier > 0.0 { attempt.disarm(); }
        Ok(Turn { resolution: TurretResolution::Fired { fitted_spool }, dealt, multiplier, victim_pilot, receipt })
    }).await;
    let Some(turn) = turn else { return Err(CallError::Internal(anyhow::anyhow!("combat actor unavailable"))) };
    let turn = turn?;
    if let Some(receipt) = turn.receipt {
        receipt.await.map_err(|_| CallError::Internal(anyhow::anyhow!("combat loss worker stopped")))??;
    }
    if matches!(turn.resolution, TurretResolution::Fired { .. }) {
        report_hit(world, source_id, target_id, shooter_pilot, turn.victim_pilot, weapon, turn.dealt, hit_quality(turn.multiplier), false);
    }
    Ok(turn.resolution)
}

/// Calculate a fitted turret's parameters and unscaled typed damage. A turret
/// with no charge compatibility reads raw damage from the module itself.
pub(super) fn turret_parameters(fit: &Fit, module: &Module, direct_damage: bool) -> ((f64, f64, f64, f64), [f64; 4]) {
    let (m, ch) = (Key::Module(module.id), Key::Charge(module.id));
    let damage = if direct_damage { m } else { ch };
    let raw = ATTR_DAMAGE.map(|id| fit.value(damage, id) * fit.value(m, ATTR_DAMAGE_MULTIPLIER).max(0.0));
    ((fit.value(m, ATTR_MAX_RANGE), fit.value(m, ATTR_FALLOFF),
        fit.value(m, ATTR_TRACKING_SPEED), fit.value(m, ATTR_OPTIMAL_SIG_RADIUS)), raw)
}

pub(super) async fn fire_turret(cx: &Ctx, inputs: &TurretInputs, module: &Module, target: i64, illegal: bool, spool: Option<TurretSpool>, attempt: PreparedCombatAttempt) -> Result<TurretResolution, CallError> {
    let ship_id = cx.ship_id;
    let Some((ship, t)) = cx.world.with(cx.system_id, move |s| Some((s.sampled_ball(ship_id)?, s.sampled_ball(target)?))).await.flatten() else { return Ok(TurretResolution::NotFired) };
    turret_shot(&cx.db, &cx.world, cx.system_id, &ship, &t, Some(cx.char_id), module.type_id, illegal,
        inputs.parameters, inputs.raw, None, spool, attempt).await
}

/// What a missile does when it arrives.
pub(crate) struct Missile {
    pub(crate) charge: i32,
    pub(crate) raw: [f64; 4],
    pub(crate) aoe_cloud: f64,
    pub(crate) aoe_velocity: f64,
    pub(crate) drf: f64,
    pub(crate) velocity: f64,
    /// Effective lifetime in fractional milliseconds from the applied launch.
    pub(crate) flight_time_ms: f64,
    pub(crate) weapon: i32,
    pub(crate) illegal: bool,
}


/// Launch a native-steered missile flight; the actor sweeps target contact and
/// expires the flight at its frozen effective lifetime. A committed contact
/// applies explosion radius and velocity against the target at that actor turn.
pub(crate) async fn launch_missile(db: Db, world: Arc<World>, system: i64, from: Ball, target: Ball, owner: i64, module: Option<i64>, m: Missile, guard: Option<super::space_source::SourceGuard>, mut attempt: PreparedCombatAttempt) -> Result<(), CallError> {
    let size = world.sde().types.get(m.charge)
        .ok_or_else(|| CallError::Internal(anyhow::anyhow!("unknown type {}", m.charge)))?;
    if !m.flight_time_ms.is_finite() || m.flight_time_ms <= 0.0 {
        return Err(CallError::Internal(anyhow::anyhow!("invalid missile flight time {}", m.flight_time_ms)));
    }
    let mut slim = vec![("sourceShipID", Value::Int(from.id))];
    slim.push(("launchModules", Value::list(module.map(Value::Int))));
    let launch_ms = now_ms();
    let missile = Ball {
        id: next_missile_id(),
        type_id: m.charge,
        owner_id: owner,
        corp_id: from.corp_id,
        alliance_id: -1,
        char_id: None,
        signature_radius: None,
        mode: crate::space::MODE_STOP,
        radius: size.radius.unwrap_or(1.0) as f32,
        mass: size.mass.unwrap_or(1.0),
        max_vel: m.velocity as f32,
        agility: 1.0,
        speed_fraction: 0.0,
        pos: from.pos,
        vel: [0.0; 3],
        old_pos: from.pos, old_vel: [0.0; 3], segment_acceleration: None,
        goto: target.pos,
        follow_id: 0,
        follow_range: 0.0,
        free: true,
        global: false,
        massive: false,
        interactive: false,
        warp: crate::space::WarpState::default(),
        last_ms: launch_ms,
        interpolation_anchor_ms: launch_ms,
        effect_stamp: 0,
        cloak: 0,
        cloak_until: 0,
        health: [1.0; 3],
        launcher: 0,
        slim,
    };
    let missile_id = missile.id;
    let flight_time_ms = m.flight_time_ms;
    let shooter_id = from.id;
    let target_id = target.id;
    let facts = attempt.facts();
    let shooter_pilot = facts.actual_actor_character;
    let (aoe_cloud, aoe_velocity, drf, raw, weapon) =
        (m.aoe_cloud, m.aoe_velocity, m.drf, m.raw, m.weapon);
    let sde = world.sde().clone();
    let world_ref = Arc::downgrade(&world);
    let impact_db = db.clone();
    world.system(system).await.map_err(CallError::Internal)?;
    let Some((victim_pilot, target_generation)) = world.with(system, move |s| {
        if guard.is_some_and(|guard| !guard.valid(s)) { return None; }
        Some((s.combat_owner(target_id), s.combat_membership_generation(target_id)?))
    }).await.flatten() else { return Ok(()) };
    let drone_admission = matches!(guard, Some(super::space_source::SourceGuard::Drone { .. }))
        .then_some((shooter_pilot, victim_pilot));
    let illegal = if let Some((Some(shooter), Some(victim))) = drone_admission {
        crate::services::crimewatch::classify(&db, &world, system, shooter, victim).await?
    } else { m.illegal };
    let Some(()) = world.with(system, move |s| {
        if guard.is_some_and(|guard| !guard.valid(s)) { return None; }
        if s.combat_membership_generation(target_id) != Some(target_generation) { return None; }
        if drone_admission.is_some() && s.combat_owner(target_id) != victim_pilot { return None; }
        s.drone_attack(shooter_id, target_id);
        s.launch_missile(missile, target_id, shooter_id, flight_time_ms, move |s, contact| {
            // The actor already resolved the flight's terminal. `Some` is the
            // single committed contact: read the effective target state in this
            // actor turn and apply damage in place. Everything else is a miss.
            let applied = contact.and_then(|contact| {
                if s.combat_membership_generation(contact.target) != Some(target_generation) { return None; }
                let signature = target_signature(&sde, s.ball(contact.target)?);
                let victim = s.combat_owner(contact.target);
                Some((contact, signature, victim))
            });
            let Some((contact, signature, victim)) = applied else {
                attempt.finish_in_actor(s);
                let victim = (s.combat_membership_generation(target_id) == Some(target_generation))
                    .then(|| s.combat_owner(target_id)).flatten();
                s.defer(async move {
                    if let Some(world) = world_ref.upgrade() {
                        report_hit(&world, shooter_id, target_id, shooter_pilot, victim, weapon, 0.0, HIT_MISS, false);
                    }
                });
                return;
            };
            let target_id = contact.target;
            let scaled = missile_applied(signature, dist(contact.target_velocity, [0.0; 3]), aoe_cloud, aoe_velocity, drf);
            let result = apply_damage_in_actor(s, &impact_db, system, target_id, raw.map(|d| d * scaled), DamageSource {
                attacker_id: shooter_pilot, attacker_ball_id: Some(facts.entity.item_id),
                source_ship_id: Some(facts.entity.item_id), source_ship_type: Some(facts.entity.type_id),
                weapon_type_id: Some(weapon), facts: Some(facts), force_origin: None, illegal,
            }, false);
            attempt.disarm();
            let (dealt, capacity, receipt) = match result {
                Ok(Some((outcome, receipt))) => (outcome.dealt, false, receipt),
                Ok(None) => (0.0, false, None),
                Err(error) if crate::services::combat::capacity::is_refusal(&error) => (0.0, true, None),
                Err(error) => {
                    tracing::warn!(?error, missile = missile_id, "missile impact damage failed");
                    (0.0, false, None)
                }
            };
            s.defer(async move {
                let mut dealt = dealt;
                if let Some(receipt) = receipt {
                    match receipt.await {
                        Ok(Ok(())) => {}
                        Ok(Err(error)) => {
                            tracing::warn!(?error, missile = missile_id, "missile loss settlement failed");
                            dealt = 0.0;
                        }
                        Err(_) => {
                            tracing::warn!(missile = missile_id, "missile loss worker stopped");
                            dealt = 0.0;
                        }
                    }
                }
                let Some(world) = world_ref.upgrade() else { return };
                if capacity { attempt.report_capacity_refusal(); }
                report_hit(&world, shooter_id, target_id, shooter_pilot, victim, weapon, dealt, HIT_NORMAL, false);
            });
        });
        Some(())
    }).await.ok_or_else(|| CallError::Internal(anyhow::anyhow!("system actor stopped")))? else { return Ok(()) };
    if let Some((Some(shooter), Some(victim))) = drone_admission {
        if let Err(error) = crate::services::crimewatch::apply(&db, &world, system, shooter, victim, illegal).await {
            tracing::warn!(?error, missile = missile_id, "missile aggression application failed");
        }
    }
    Ok(())
}

pub(super) async fn fire_missile(cx: &Ctx, inputs: &MissileInputs, module: &Module, charge: i32, target: i64, illegal: bool, attempt: PreparedCombatAttempt) -> Result<(), CallError> {
    let ship_id = cx.ship_id;
    let Some((ship, t)) = cx.world.with(cx.system_id, move |s| Some((s.sampled_ball(ship_id)?, s.sampled_ball(target)?))).await.flatten() else { return Ok(()) };
    let missile = Missile {
        charge,
        raw: inputs.raw,
        aoe_cloud: inputs.aoe_cloud,
        aoe_velocity: inputs.aoe_velocity,
        drf: inputs.drf,
        velocity: inputs.velocity,
        flight_time_ms: inputs.flight_time_ms,
        weapon: module.type_id,
        illegal,
    };
    launch_missile(cx.db.clone(), cx.world.clone(), cx.system_id, ship, t, cx.char_id, Some(module.id), missile, None, attempt).await
}

/// Execute an actor-owned weapon using the sampled source and its typed inputs.
pub(crate) async fn fire_source(cx: &super::space_source::SourceContext<'_>, attempt: Option<PreparedCombatAttempt>) -> Result<(), CallError> {
    use crate::space::drone::DronePhase;
    if !matches!(cx.phase, DronePhase::Begin) { return Ok(()); }
    let attempt = attempt.ok_or_else(|| CallError::Internal(anyhow::anyhow!("missing admitted combat attempt")))?;
    let source = cx.source.id;
    let target = cx.target.id;
    let guard = cx.guard;
    let sample = || async {
        cx.world.with(cx.system, move |s| {
            if !guard.valid(s) { return None; }
            Some((s.sampled_ball(source)?, s.sampled_ball(target)?))
        }).await.flatten()
    };
    let Some((from, victim)) = sample().await else { return Ok(()) };
    match cx.inputs {
        SourceInputs::Turret(inputs) => {
            turret_shot(cx.db, cx.world, cx.system, &from, &victim, cx.attacker, from.type_id, false,
                inputs.parameters, inputs.raw, Some(cx.guard), None, attempt).await.map(|_| ())
        }
        SourceInputs::Missile(inputs) => {
            let Some(charge) = inputs.charge else {
                let id = cx.effect.id as i32;
                let name = cx.world.sde().dogma.effect(id).and_then(|row| row.name.as_deref()).unwrap_or("unknown");
                return Err(super::unsupported::refusal(Some(id), name, from.type_id));
            };
            let missile = Missile {
                charge,
                raw: inputs.raw,
                aoe_cloud: inputs.aoe_cloud,
                aoe_velocity: inputs.aoe_velocity,
                drf: inputs.drf,
                velocity: inputs.velocity,
                flight_time_ms: inputs.flight_time_ms,
                weapon: from.type_id,
                illegal: false,
            };
            let owner = cx.attacker.unwrap_or(from.owner_id);
            launch_missile(cx.db.clone(), cx.world.clone(), cx.system, from, victim, owner, None, missile, Some(cx.guard), attempt).await
        }
        _ => Err(CallError::Internal(anyhow::anyhow!("weapon source captured without weapon inputs"))),
    }
}


#[cfg(all(test, feature = "tests-effects"))] mod tests;
