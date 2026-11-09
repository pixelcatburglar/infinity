//! Turret accuracy and missile launches using fitted dogma and charge data.
//! Mirrors clientDogmaLocation and michelle, with accuracy from native GetAccuracy
//! and SDE attributes; missile flight remains an approximation of native physics.
//! Admitted combat source identities remain frozen until their damage is resolved.
use super::lifecycle::Module;
use super::space_source::PreparedCombatAttempt;
use crate::services::combat::{quantity_change, Ctx, ATTR_DAMAGE};
use crate::services::combat::damage::{apply_damage_guarded, DamageSource};
use crate::services::fit::{Fit, Key};
use crate::services::{db_err, rand01, CallError};
use infinity_core::consts::{dogma::ATTR_AOE_CLOUD_SIZE, dogma::ATTR_AOE_DAMAGE_REDUCTION_FACTOR, dogma::ATTR_AOE_VELOCITY, dogma::ATTR_DAMAGE_MULTIPLIER, dogma::ATTR_EXPLOSION_DELAY, dogma::ATTR_FALLOFF, dogma::ATTR_MAX_RANGE, dogma::ATTR_MAX_VELOCITY, dogma::ATTR_MISSILE_DAMAGE_MULTIPLIER, dogma::ATTR_OPTIMAL_SIG_RADIUS, dogma::ATTR_SIGNATURE_RADIUS, dogma::ATTR_TRACKING_SPEED};
use crate::db::Db;
use crate::db::outbox::{dispatch_world, Outbox};
use crate::sde::Sde;
use crate::space::{dist, ms_to_seconds, now_ms, seconds_to_ms, Ball, World};
use marshal::Value;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

/// Existing missile flight-time clamp.
const MIN_MISSILE_FLIGHT_SECONDS: f64 = 0.5;
/// Existing missile flight-time clamp.
const MAX_MISSILE_FLIGHT_SECONDS: f64 = 30.0;
/// Existing missile reach tolerance.
const MISSILE_REACH_ALLOWANCE_METRES: f64 = 2000.0;
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

/// Use one charge from a module; `None` when it is empty.
pub(super) async fn consume_charge(cx: &Ctx, module: &Module, expected: Option<i32>) -> Result<Option<i32>, CallError> {
    let (ship, flag, char_id) = (cx.ship_id, module.flag, cx.char_id);
    let notify = cx.notify.clone();
    cx.db.transaction(
        |tx, outbox: &mut Outbox| {
            let notify = notify.clone();
            Box::pin(async move {
            if !crate::dao::items::ship::lock_reload_ship(tx, ship).await.map_err(db_err)?
                || !crate::dao::items::pending_reloads::for_flags(tx, ship, &[flag]).await.map_err(db_err)?.is_empty() {
                return Ok(None);
            }
            if let Some(expected) = expected {
                if crate::services::combat::targeting_charges::loaded(tx, ship, flag).await?.is_none_or(|(kind, quantity)| kind != expected || quantity <= 0) {
                    return Ok(None);
                }
            }
            let Some(r) = crate::dao::items::combat::consume_charge(tx, ship, flag).await.map_err(db_err)? else { return Ok(None) };
            if r.quantity == 0 {
                crate::dao::items::combat::delete_empty_charge(tx, ship, flag).await.map_err(db_err)?;
            }
            outbox.notify_sender(notify, "OnModuleAttributeChanges", vec![quantity_change(char_id, ship, flag, r.type_id, r.quantity as i64, r.quantity as i64 + 1)]);
            Ok(Some(r.type_id))
            })
        },
        |value, outbox| async move { dispatch_world(value, outbox, &cx.world) },
    ).await
}

/// Confirm an uncharged ancillary cycle did not race a reload or a new charge.
pub(super) async fn empty_charge(cx: &Ctx, module: &Module) -> Result<bool, CallError> {
    let (ship, flag) = (cx.ship_id, module.flag);
    cx.db.transaction(
        |tx, _: &mut Outbox| Box::pin(async move {
            if !crate::dao::items::ship::lock_reload_ship(tx, ship).await.map_err(db_err)?
                || !crate::dao::items::pending_reloads::for_flags(tx, ship, &[flag]).await.map_err(db_err)?.is_empty() {
                return Ok(false);
            }
            Ok(crate::services::combat::targeting_charges::loaded(tx, ship, flag).await?.is_none_or(|(_, quantity)| quantity <= 0))
        }),
        |value, outbox| async move { dispatch_world(value, outbox, &cx.world) },
    ).await
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
pub(super) fn damage_message(key: &str, other: i64, weapon: i32, damage: f64, quality: i64) -> Value {
    Value::dict([
        (key, Value::Int(other)),
        ("weapon", Value::Int(weapon as i64)),
        ("damage", Value::Float(damage)),
        ("hitQuality", Value::Int(quality)),
        ("isBanked", Value::Bool(false)),
        // uilog: "me" picks AttackHits ("to {target}"), "otherPlayerWeapons" AttackHitsR
        // ("from {source}").
        ("attackType", Value::str(if key == "source" { "otherPlayerWeapons" } else { "me" })),
        ("damageTypes", Value::Dict(vec![])),
    ])
}

/// Tell both sides of a hit: the shooter (if a pilot) and the victim (if a pilot).
pub(crate) fn report_hit(world: &World, shooter: i64, target: i64, shooter_pilot: Option<i64>, target_pilot: Option<i64>, weapon: i32, damage: f64, quality: i64) {
    if let Some(ch) = shooter_pilot {
        world.notify_pilot(ch, "OnDamageMessage", vec![damage_message("target", target, weapon, damage, quality)]);
    }
    if let Some(ch) = target_pilot {
        world.notify_pilot(ch, "OnDamageMessage", vec![damage_message("source", shooter, weapon, damage, quality)]);
    }
}

/// Resolve and apply one source-neutral turret shot.
pub(crate) async fn turret_shot(db: &Db, world: &Arc<World>, system: i64, source: &Ball, target: &Ball,
    attacker: Option<i64>, weapon: i32, illegal: bool, parameters: (f64, f64, f64, f64), raw: [f64; 4],
    guard: Option<super::space_source::SourceGuard>, mut attempt: PreparedCombatAttempt) -> Result<(), CallError> {
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
    }).await else { return Ok(()) };
    let chance = turret_chance(&sampled_source, &sampled_target, optimal, falloff, tracking, signature, target_signature(world.sde(), &sampled_target));
    let multiplier = turret_roll(chance);
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
        if outcome.is_some() { attempt.disarm(); }
        outcome.map_or(0.0, |outcome| outcome.dealt)
    } else { 0.0 };
    report_hit(world, source_id, target_id, shooter_pilot, victim_pilot, weapon, dealt, hit_quality(multiplier));
    Ok(())
}

/// Calculate a fitted turret's parameters and unscaled typed damage.
pub(super) fn turret_parameters(fit: &Fit, module: &Module) -> ((f64, f64, f64, f64), [f64; 4]) {
    let (m, ch) = (Key::Module(module.id), Key::Charge(module.id));
    let raw = ATTR_DAMAGE.map(|id| fit.value(ch, id) * fit.value(m, ATTR_DAMAGE_MULTIPLIER).max(0.0));
    ((fit.value(m, ATTR_MAX_RANGE), fit.value(m, ATTR_FALLOFF),
        fit.value(m, ATTR_TRACKING_SPEED), fit.value(m, ATTR_OPTIMAL_SIG_RADIUS)), raw)
}

pub(super) async fn fire_turret(cx: &Ctx, fit: &Fit, module: &Module, target: i64, illegal: bool, attempt: PreparedCombatAttempt) -> Result<(), CallError> {
    let ship_id = cx.ship_id;
    let Some((ship, t)) = cx.world.with(cx.system_id, move |s| Some((s.sampled_ball(ship_id)?, s.sampled_ball(target)?))).await.flatten() else { return Ok(()) };
    let (parameters, raw) = turret_parameters(fit, module);
    turret_shot(&cx.db, &cx.world, cx.system_id, &ship, &t, Some(cx.char_id), module.type_id, illegal,
        parameters, raw, None, attempt).await
}

/// What a missile does when it arrives.
pub(crate) struct Missile {
    pub(crate) charge: i32,
    pub(crate) raw: [f64; 4],
    pub(crate) aoe_cloud: f64,
    pub(crate) aoe_velocity: f64,
    pub(crate) drf: f64,
    pub(crate) velocity: f64,
    pub(crate) flight_range: f64,
    pub(crate) weapon: i32,
    pub(crate) illegal: bool,
}


/// Launch a native-steered missile actor; the separate gameplay impact timer
/// applies explosion radius and velocity against the impact-time target state.
pub(crate) async fn launch_missile(db: Db, world: Arc<World>, system: i64, from: Ball, target: Ball, owner: i64, module: Option<i64>, m: Missile, guard: Option<super::space_source::SourceGuard>, mut attempt: PreparedCombatAttempt) -> Result<(), CallError> {
    let size = world.sde().types.get(m.charge)
        .ok_or_else(|| CallError::Internal(anyhow::anyhow!("unknown type {}", m.charge)))?;
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
    let distance = dist(from.pos, target.pos);
    let delay_ms = seconds_to_ms((distance / m.velocity.max(1.0)).clamp(MIN_MISSILE_FLIGHT_SECONDS, MAX_MISSILE_FLIGHT_SECONDS));
    let shooter_id = from.id;
    let target_id = target.id;
    let facts = attempt.facts();
    let shooter_pilot = facts.actual_actor_character;
    let (flight_range, aoe_cloud, aoe_velocity, drf, raw, weapon) =
        (m.flight_range, m.aoe_cloud, m.aoe_velocity, m.drf, m.raw, m.weapon);
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
        s.launch_missile(missile, target_id, shooter_id, delay_ms, move |s| {
            if !s.contains(missile_id) { return; }
            s.destroy_ball(missile_id);
            let application = (s.combat_membership_generation(target_id) == Some(target_generation))
                .then(|| s.sampled_ball(target_id)).flatten()
                .filter(|target| target.cloak == 0 && target.mode != crate::space::MODE_WARP)
                .map(|target| {
                    (missile_applied(target_signature(&sde, &target), dist(target.vel, [0.0; 3]), aoe_cloud, aoe_velocity, drf),
                        s.combat_owner(target_id))
                });
            let reached = distance <= flight_range + MISSILE_REACH_ALLOWANCE_METRES;
            s.defer(async move {
                let Some(world) = world_ref.upgrade() else { return };
                let (dealt, quality, victim_pilot) = if let Some((application, victim_pilot)) = application.filter(|_| reached) {
                    let outcome = apply_damage_guarded(&impact_db, &world, system, target_id, raw.map(|d| d * application), DamageSource {
                        attacker_id: shooter_pilot, attacker_ball_id: Some(facts.entity.item_id),
                        source_ship_id: Some(facts.entity.item_id), source_ship_type: Some(facts.entity.type_id),
                        weapon_type_id: Some(weapon), facts: Some(facts), force_origin: None, illegal,
                    }, false, move |s| s.combat_membership_generation(target_id) == Some(target_generation)
                        && s.ball(target_id).is_some_and(|target| target.cloak == 0 && target.mode != crate::space::MODE_WARP)).await;
                    let dealt = match outcome {
                        Ok(Some(outcome)) => { attempt.disarm(); outcome.dealt }
                        Ok(None) => 0.0,
                        Err(error) if crate::services::combat::capacity::is_refusal(&error) => {
                            attempt.report_capacity_refusal();
                            0.0
                        }
                        Err(error) => {
                            tracing::warn!(?error, missile = missile_id, "missile impact damage failed");
                            0.0
                        }
                    };
                    (dealt, HIT_NORMAL, victim_pilot)
                } else {
                    (0.0, HIT_MISS, application.and_then(|(_, owner)| owner))
                };
                report_hit(&world, shooter_id, target_id, shooter_pilot, victim_pilot, weapon, dealt, quality);
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

pub(super) async fn fire_missile(cx: &Ctx, fit: &Fit, module: &Module, charge: i32, target: i64, illegal: bool, attempt: PreparedCombatAttempt) -> Result<(), CallError> {
    let ship_id = cx.ship_id;
    let Some((ship, t)) = cx.world.with(cx.system_id, move |s| Some((s.sampled_ball(ship_id)?, s.sampled_ball(target)?))).await.flatten() else { return Ok(()) };
    let ch = Key::Charge(module.id);
    let velocity = fit.value(ch, ATTR_MAX_VELOCITY).max(1.0);
    let missile = Missile {
        charge,
        raw: ATTR_DAMAGE.map(|id| fit.value(ch, id) * fit.value(Key::Char, ATTR_MISSILE_DAMAGE_MULTIPLIER).max(0.0)),
        aoe_cloud: fit.value(ch, ATTR_AOE_CLOUD_SIZE),
        aoe_velocity: fit.value(ch, ATTR_AOE_VELOCITY),
        drf: fit.value(ch, ATTR_AOE_DAMAGE_REDUCTION_FACTOR),
        velocity,
        flight_range: velocity * ms_to_seconds(fit.value(ch, ATTR_EXPLOSION_DELAY)),
        weapon: module.type_id,
        illegal,
    };
    launch_missile(cx.db.clone(), cx.world.clone(), cx.system_id, ship, t, cx.char_id, Some(module.id), missile, None, attempt).await
}

/// Execute an actor-owned weapon using the sampled source and effective fit.
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
    if matches!(cx.effect.kind, super::EffectKind::Missile) {
        let charge = cx.fit.value(cx.key, infinity_core::consts::dogma::ATTR_ENTITY_MISSILE_TYPE) as i32;
        if charge <= 0 || cx.world.sde().types.get(charge).is_none() {
            let id = cx.effect.id as i32;
            let name = cx.world.sde().dogma.effect(id).and_then(|row| row.name.as_deref()).unwrap_or("unknown");
            return Err(super::unsupported::refusal(Some(id), name, from.type_id));
        }
    }
    let fit = cx.fit;
    let key = cx.key;
    match cx.effect.kind {
        super::EffectKind::Turret => {
            let raw = ATTR_DAMAGE.map(|id| fit.value(key, id) * fit.value(key, ATTR_DAMAGE_MULTIPLIER).max(0.0));
            turret_shot(cx.db, cx.world, cx.system, &from, &victim, cx.attacker, from.type_id, false,
                (fit.value(key, ATTR_MAX_RANGE), fit.value(key, ATTR_FALLOFF), fit.value(key, ATTR_TRACKING_SPEED), fit.value(key, ATTR_OPTIMAL_SIG_RADIUS)), raw, Some(cx.guard), attempt).await
        }
        super::EffectKind::Missile => {
            let charge = fit.value(key, infinity_core::consts::dogma::ATTR_ENTITY_MISSILE_TYPE) as i32;
            let sde = cx.world.sde();
            let attr = |id| sde.types.attribute(charge, id).or_else(|| sde.dogma.attribute(id).and_then(|a| a.default_value)).unwrap_or(0.0);
            let velocity = (attr(ATTR_MAX_VELOCITY) * fit.value(key, infinity_core::consts::dogma::ATTR_MISSILE_ENTITY_VELOCITY_MULTIPLIER)).max(1.0);
            let missile = Missile {
                charge,
                raw: ATTR_DAMAGE.map(|id| attr(id) * fit.value(key, ATTR_MISSILE_DAMAGE_MULTIPLIER)),
                aoe_cloud: attr(ATTR_AOE_CLOUD_SIZE) * fit.value(key, infinity_core::consts::dogma::ATTR_MISSILE_ENTITY_AOE_CLOUD_SIZE_MULTIPLIER),
                aoe_velocity: attr(ATTR_AOE_VELOCITY) * fit.value(key, infinity_core::consts::dogma::ATTR_MISSILE_ENTITY_AOE_VELOCITY_MULTIPLIER),
                drf: attr(ATTR_AOE_DAMAGE_REDUCTION_FACTOR),
                velocity,
                flight_range: velocity * ms_to_seconds(attr(ATTR_EXPLOSION_DELAY) * fit.value(key, infinity_core::consts::dogma::ATTR_MISSILE_ENTITY_FLIGHT_TIME_MULTIPLIER)),
                weapon: from.type_id,
                illegal: false,
            };
            let owner = cx.attacker.unwrap_or(from.owner_id);
            launch_missile(cx.db.clone(), cx.world.clone(), cx.system, from, victim, owner, None, missile, Some(cx.guard), attempt).await
        }
        _ => unreachable!(),
    }
}


#[cfg(test)] mod tests;
