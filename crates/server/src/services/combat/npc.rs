//! NPC combat uses SDE entity and charge attributes, the actor's resident Fit
//! and shared weapon application; routing and state live in the parent module.
//! Queued shots revalidate the actor's current target and ECM lockout; a
//! missile's range only gates the launch decision, while its in-flight
//! lifetime is the effective explosion delay.
//! The client has no NPC server combat implementation to mirror.
//! Weapon animations are published through the system's ordered scene channel.
use super::*;
use crate::services::effects::weapons::{launch_missile, turret_shot, Missile, TurretSpool};
use crate::services::effects::spool::{SpoolBinding, SpoolParameters};
use infinity_core::consts::dogma::ATTR_AGILITY;
use infinity_core::consts::{dogma::ATTR_TRACKING_SPEED, dogma::ATTR_OPTIMAL_SIG_RADIUS, dogma::ATTR_AOE_VELOCITY, dogma::ATTR_AOE_DAMAGE_REDUCTION_FACTOR, dogma::ATTR_MISSILE_ENTITY_VELOCITY_MULTIPLIER, dogma::ATTR_MISSILE_ENTITY_FLIGHT_TIME_MULTIPLIER, dogma::ATTR_MISSILE_ENTITY_AOE_VELOCITY_MULTIPLIER, dogma::ATTR_MISSILE_ENTITY_AOE_CLOUD_SIZE_MULTIPLIER, dogma::ATTR_DAMAGE_MULTIPLIER_BONUS_PER_CYCLE, dogma::ATTR_DAMAGE_MULTIPLIER_BONUS_MAX, dogma::EFFECT_TARGET_DISINTEGRATOR_ATTACK, dogma::EFFECT_NPC_BEHAVIOR_SHIELD_BOOST, dogma::EFFECT_NPC_BEHAVIOR_ARMOR_REPAIR, dogma::ATTR_BEHAVIOR_SHIELD_BOOSTER_AMOUNT, dogma::ATTR_BEHAVIOR_SHIELD_BOOSTER_DURATION, dogma::ATTR_BEHAVIOR_ARMOR_REPAIRER_AMOUNT, dogma::ATTR_BEHAVIOR_ARMOR_REPAIRER_DURATION};
use crate::sde::types::TypeData;
use crate::space::{special_fx, System, MODE_STOP, V3};

/// SDE optimalSigRadius attribute metadata default when metadata is absent.
const DEFAULT_OPTIMAL_SIG_RADIUS: f64 = 1000.0;
/// The existing shared NPC repair cycle floor (ms): both local repair layers
/// tick on one common cycle and only longer legacy durations raise the period.
const NPC_REPAIR_CYCLE_FLOOR_MS: f64 = 5_000.0;

/// Create the SDE-derived NPC ball and attach its existing combat brain.
pub(crate) async fn spawn(db: &Db, world: &Arc<World>, system: i64, data: &TypeData, corp: i64, pos: V3, tick: i64, owner: Option<i64>) -> Result<i64, CallError> {
    let ball = Ball {
        id: crate::services::missions::NEXT_NPC_ID.fetch_add(1, Ordering::Relaxed),
        type_id: data.type_id,
        owner_id: corp,
        corp_id: corp,
        alliance_id: -1,
        char_id: None,
        mode: MODE_STOP,
        radius: data.radius.unwrap_or(50.0) as f32,
        signature_radius: None,
        mass: data.mass.unwrap_or(1_000_000.0),
        max_vel: data.attribute(ATTR_MAX_VELOCITY).unwrap_or(300.0) as f32,
        agility: data.attribute(ATTR_AGILITY).unwrap_or(3.0) as f32,
        speed_fraction: 0.0,
        pos,
        vel: [0.0; 3],
        old_pos: pos,
        old_vel: [0.0; 3], segment_acceleration: None,
        goto: [0.0; 3],
        follow_id: 0,
        follow_range: 0.0,
        free: true,
        global: false,
        massive: true,
        interactive: false,
        warp: crate::space::WarpState::default(),
        last_ms: tick,
        interpolation_anchor_ms: tick,
        effect_stamp: 0,
        cloak: 0,
        cloak_until: 0,
        health: [1.0; 3],
        launcher: 0,
        slim: vec![("corpID", Value::Int(corp))],
    };
    let id = ball.id;
    world.add_npc_ship(system, ball).await.map_err(CallError::Internal)?;
    spawn_npc(db.clone(), world.clone(), system, id, data.type_id, owner).await?;
    Ok(id)
}

// ---------------------------------------------------------------- NPCs

/// An NPC's fighting attributes (entity attributes on its type).
#[derive(Clone)]
pub(super) struct NpcStats {
    /// (shield boost, armor repair) amount per repair cycle, and the cycle (ms).
    /// A carried behavior repair effect (SDE 6990/6884) supplies a layer's
    /// amount rescaled to the shared cycle when no valid legacy amount exists.
    pub(super) repairs: (f64, f64, f64),
    pub(super) scan_resolution: f64,
    pub(super) fly_range: f64,
    pub(super) aggro_range: f64,
    pub(super) rate_of_fire_ms: f64,
    pub(super) optimal: f64,
    pub(super) falloff: f64,
    pub(super) tracking: f64,
    pub(super) opt_sig: f64,
    pub(super) damage: [f64; DAMAGE_TYPE_COUNT],
    pub(super) missile: Option<(i32, f64)>,
    pub(super) fx: Option<String>,
    /// SDE effect id of the NPC's turret weapon when it is the precursor
    /// disintegrator (targetDisintegratorAttack, SDE 6995); `None` for every
    /// other turret NPC. Set only from the type's own effect association.
    pub(super) turret_effect: Option<i32>,
}

pub(super) async fn npc_stats(db: &Db, sde: &Sde, type_id: i32) -> Result<NpcStats, CallError> {
    let multiplier = attr(sde, type_id, ATTR_DAMAGE_MULTIPLIER, 1.0);
    let turret_effect = sde.types.effects(type_id).iter()
        .find(|entry| entry.effect_id == EFFECT_TARGET_DISINTEGRATOR_ATTACK)
        .map(|entry| entry.effect_id);
    let fx = sde.types.effects(type_id).iter().filter_map(|entry| sde.dogma.effect(entry.effect_id))
        .find(|effect| effect.effect_category == Some(2)
            && effect.guid.as_deref().is_some_and(|guid| guid.starts_with("effects."))
            && effect.name.as_deref().is_some_and(|name| name != "missileLaunching"))
        .and_then(|effect| effect.guid.clone());
    let _ = db;
    let missile_type = attr(sde, type_id, ATTR_ENTITY_MISSILE_TYPE, 0.0) as i32;
    let missile = if missile_type > 0 {
        Some((missile_type, attr(sde, type_id, ATTR_MISSILE_DAMAGE_MULTIPLIER, 1.0)))
    } else {
        None
    };
    let optimal = attr(sde, type_id, ATTR_MAX_RANGE, 10_000.0);
    let attribute = |id, fallback| sde.types.attribute(type_id, id)
        .or_else(|| sde.dogma.attribute(id).and_then(|a| a.default_value))
        .unwrap_or(fallback);
    // Both repair layers tick on one existing shared cycle (floor unchanged).
    // A carried npcBehaviorShieldBooster/npcBehaviorArmorRepairer effect (SDE
    // 6990/6884) supplies that layer's amount rescaled onto the shared cycle —
    // only when no finite positive legacy amount exists; never added on top.
    let repair_cycle_ms = attr(sde, type_id, ATTR_ENTITY_ARMOR_REPAIR_DURATION, 0.0)
        .max(attr(sde, type_id, ATTR_ENTITY_SHIELD_BOOST_DURATION, 0.0)).max(NPC_REPAIR_CYCLE_FLOOR_MS);
    let behavior = |legacy: f64, effect: i32, amount_id: i32, duration_id: i32| {
        if legacy.is_finite() && legacy > 0.0
            || !sde.types.effects(type_id).iter().any(|entry| entry.effect_id == effect) { return legacy; }
        behavior_cycle_amount(sde.types.attribute(type_id, amount_id),
            sde.types.attribute(type_id, duration_id), repair_cycle_ms).unwrap_or(legacy)
    };
    let repairs = (
        behavior(attr(sde, type_id, ATTR_ENTITY_SHIELD_BOOST_AMOUNT, 0.0), EFFECT_NPC_BEHAVIOR_SHIELD_BOOST, ATTR_BEHAVIOR_SHIELD_BOOSTER_AMOUNT, ATTR_BEHAVIOR_SHIELD_BOOSTER_DURATION),
        behavior(attr(sde, type_id, ATTR_ENTITY_ARMOR_REPAIR_AMOUNT, 0.0), EFFECT_NPC_BEHAVIOR_ARMOR_REPAIR, ATTR_BEHAVIOR_ARMOR_REPAIRER_AMOUNT, ATTR_BEHAVIOR_ARMOR_REPAIRER_DURATION),
        repair_cycle_ms,
    );
    Ok(NpcStats {
        scan_resolution: attr(sde, type_id, ATTR_SCAN_RESOLUTION, 0.0),
        repairs,
        fly_range: attr(sde, type_id, ATTR_ENTITY_FLY_RANGE, 7_500.0).max(1_000.0),
        aggro_range: attr(sde, type_id, ATTR_ENTITY_ATTACK_RANGE, 0.0).max(optimal * 3.0).max(60_000.0),
        rate_of_fire_ms: attr(sde, type_id, ATTR_RATE_OF_FIRE, 4_000.0).max(1_000.0),
        optimal,
        falloff: attr(sde, type_id, ATTR_FALLOFF, 5_000.0),
        tracking: attribute(ATTR_TRACKING_SPEED, 0.0),
        opt_sig: attribute(ATTR_OPTIMAL_SIG_RADIUS, DEFAULT_OPTIMAL_SIG_RADIUS),
        damage: ATTR_DAMAGE.map(|id| attr(sde, type_id, id, 0.0) * multiplier),
        missile,
        fx,
        turret_effect,
    })
}

/// Per-cycle amount of one carried NPC behavior repair effect rescaled onto
/// the shared cycle: SDE `amount * cycle_ms / duration`. Missing, non-finite
/// or non-positive inputs admit no behavior amount.
fn behavior_cycle_amount(amount: Option<f64>, duration: Option<f64>, cycle_ms: f64) -> Option<f64> {
    let (amount, duration) = (amount?, duration?);
    (amount.is_finite() && amount > 0.0 && duration.is_finite() && duration > 0.0)
        .then(|| amount * cycle_ms / duration)
        .filter(|scaled| scaled.is_finite())
}

/// Attach a brain to a spawned NPC; its effects run on the system's worker.


fn npc_effective(fit: &Fit, sde: &Sde, attribute: i32, fallback: f64) -> f64 {
    if fit.type_has_attribute(Key::Ship, attribute)
        || sde.dogma.attribute(attribute).and_then(|a| a.default_value).is_some() {
        fit.value(Key::Ship, attribute)
    } else {
        fallback
    }
}
/// Effective missile velocity and lifetime in milliseconds; the AI derives
/// reach from them only to decide whether to launch at all.
pub(super) fn npc_missile_kinematics(velocity: f64, flight_ms: f64, velocity_multiplier: f64, flight_multiplier: f64) -> (f64, f64) {
    ((velocity * velocity_multiplier).max(1.0), flight_ms * flight_multiplier)
}

/// The fixed-size typed packet one admitted NPC shot consumes: turret numbers,
/// the missile's effective multipliers and, for a 6995 source, the spool
/// binding and ramp values — all captured inside the actor from the resident
/// combat fit so the shot never rebuilds a `Fit` or re-reads projections.
pub(super) struct NpcShotInputs {
    pub(super) optimal: f64,
    pub(super) falloff: f64,
    pub(super) tracking: f64,
    pub(super) opt_sig: f64,
    pub(super) damage: [f64; DAMAGE_TYPE_COUNT],
    pub(super) velocity_multiplier: f64,
    pub(super) flight_multiplier: f64,
    pub(super) aoe_velocity_multiplier: f64,
    pub(super) aoe_cloud_multiplier: f64,
    pub(super) spool: Option<(SpoolBinding, SpoolParameters)>,
}

pub(super) fn capture_shot(s: &mut System, source: i64, target: i64, generation: u64, stats: &NpcStats) -> Option<NpcShotInputs> {
    if !s.npc_projection_valid(source, target, generation) { return None; }
    let (source_membership, target_membership) =
        (s.combat_membership_generation(source), s.combat_membership_generation(target));
    let inputs = s.with_combat_fit(source, |fit, key, _| {
        let sde = fit.sde();
        let attribute = |attribute, fallback| if fit.type_has_attribute(key, attribute) { fit.value(key, attribute) } else { fallback };
        let effective = |attribute, fallback| npc_effective(fit, sde, attribute, fallback);
        let disintegrator = stats.turret_effect == Some(EFFECT_TARGET_DISINTEGRATOR_ATTACK);
        NpcShotInputs {
            optimal: attribute(ATTR_MAX_RANGE, stats.optimal),
            falloff: attribute(ATTR_FALLOFF, stats.falloff),
            tracking: effective(ATTR_TRACKING_SPEED, stats.tracking),
            opt_sig: stats.opt_sig,
            // A disintegrator's raw damage is a resident-fit value: spawn-time
            // stats stay the ordinary fallback, projections and bonuses apply.
            damage: if disintegrator {
                let multiplier = effective(ATTR_DAMAGE_MULTIPLIER, 1.0);
                ATTR_DAMAGE.map(|id| fit.value(key, id) * multiplier)
            } else { stats.damage },
            velocity_multiplier: effective(ATTR_MISSILE_ENTITY_VELOCITY_MULTIPLIER, 1.0),
            flight_multiplier: effective(ATTR_MISSILE_ENTITY_FLIGHT_TIME_MULTIPLIER, 1.0),
            aoe_velocity_multiplier: effective(ATTR_MISSILE_ENTITY_AOE_VELOCITY_MULTIPLIER, 1.0),
            aoe_cloud_multiplier: effective(ATTR_MISSILE_ENTITY_AOE_CLOUD_SIZE_MULTIPLIER, 1.0),
            spool: disintegrator.then(|| source_membership.zip(target_membership).map(|(source_membership, target_membership)| (
                SpoolBinding { source_membership, activation_generation: generation, target, target_membership },
                SpoolParameters {
                    step: fit.value(key, ATTR_DAMAGE_MULTIPLIER_BONUS_PER_CYCLE),
                    maximum: fit.value(key, ATTR_DAMAGE_MULTIPLIER_BONUS_MAX),
                }))).flatten(),
        }
    })?;
    // A disintegrator whose resident fit deals no damage never fires: the
    // bound ramp and its published beam end inside this same actor turn, the
    // same reset a bound shot that could not fire receives.
    if let Some((binding, _)) = inputs.spool {
        if inputs.damage.iter().all(|damage| *damage <= 0.0) {
            s.npc_spool_reset(source, binding);
        }
    }
    Some(inputs)
}

/// `me` and `victim` are the actor-sampled shot snapshots captured before worker dispatch.
pub(super) async fn npc_shot(db: &Db, world: &Arc<World>, system: i64, id: i64, type_id: i32, stats: &NpcStats, me: Ball, victim: Ball, generation: u64,
    missile_attempt: Option<crate::services::effects::space_source::PreparedCombatAttempt>, turret_attempt: Option<crate::services::effects::space_source::PreparedCombatAttempt>) -> Result<(), CallError> {
    let target = victim.id;
    let captured = stats.clone();
    let Some(Some(inputs)) = world.with(system, move |s| capture_shot(s, id, target, generation, &captured)).await else { return Ok(()) };
    let sde = world.sde();
    let ship = victim.id;
    let gap = (dist(me.pos, victim.pos) - me.radius as f64 - victim.radius as f64).max(0.0);
    let owner_id = me.owner_id;
    let turret_damage = inputs.damage.iter().any(|d| *d > 0.0);
    let guard = crate::services::effects::space_source::SourceGuard::Npc { source: id, target, generation };
    if let Some((charge, multiplier)) = &stats.missile {
        let (charge, multiplier) = (*charge, *multiplier);
        let (velocity, flight_time_ms) = npc_missile_kinematics(
            attr(world.sde(), charge, ATTR_MAX_VELOCITY, NPC_MISSILE_VELOCITY_MPS),
            attr(world.sde(), charge, ATTR_EXPLOSION_DELAY, NPC_MISSILE_FLIGHT_MS),
            inputs.velocity_multiplier, inputs.flight_multiplier);
        if gap <= velocity * ms_to_seconds(flight_time_ms) {
            let attempt = missile_attempt.ok_or_else(|| CallError::Internal(anyhow::anyhow!("missing NPC missile admission")))?;
            let missile = Missile {
                charge,
                raw: ATTR_DAMAGE.map(|a| attr(world.sde(), charge, a, 0.0) * multiplier),
                aoe_cloud: attr(world.sde(), charge, ATTR_AOE_CLOUD_SIZE, NPC_MISSILE_EXPLOSION_RADIUS_METRES) * inputs.aoe_cloud_multiplier,
                aoe_velocity: attr(world.sde(), charge, ATTR_AOE_VELOCITY, 0.0) * inputs.aoe_velocity_multiplier,
                drf: sde.types.attribute(charge, ATTR_AOE_DAMAGE_REDUCTION_FACTOR)
                    .or_else(|| sde.dogma.attribute(ATTR_AOE_DAMAGE_REDUCTION_FACTOR).and_then(|a| a.default_value))
                    .unwrap_or(1.0),
                velocity,
                flight_time_ms,
                weapon: type_id,
                illegal: false,
            };
            launch_missile(db.clone(), world.clone(), system, me.clone(), victim.clone(), owner_id, None, missile,
                Some(guard), attempt).await?;
        }
    }
    if let Some((binding, parameters)) = inputs.spool {
        // A 6995 shot is always attempted once the turret can deal damage: the
        // shared actor turn refuses out-of-optimal or stale identities and
        // resets the bound ramp and published beam there, in order.
        if turret_damage {
            let attempt = turret_attempt.ok_or_else(|| CallError::Internal(anyhow::anyhow!("missing NPC turret admission")))?;
            turret_shot(db, world, system, &me, &victim, None, type_id, false,
                (inputs.optimal, inputs.falloff, inputs.tracking, inputs.opt_sig), inputs.damage,
                Some(guard), Some(TurretSpool::Npc { binding, parameters }), attempt).await?;
        }
    } else if turret_damage && gap <= inputs.optimal + inputs.falloff * NPC_TURRET_REACH_FALLOFF_MULTIPLIER {
        let attempt = turret_attempt.ok_or_else(|| CallError::Internal(anyhow::anyhow!("missing NPC turret admission")))?;
        if let Some(guid) = stats.fx.as_deref() {
            let guid = guid.to_owned();
            let duration = stats.rate_of_fire_ms;
            world.publish_destiny(system, id, move |stamp| vec![special_fx(stamp, id, None, Some(ship),
                None, &guid, true, 1, 1, Value::Float(duration), 1, None, 0, Value::None)]);
        }
        turret_shot(db, world, system, &me, &victim, None, type_id, false,
            (inputs.optimal, inputs.falloff, inputs.tracking, inputs.opt_sig), inputs.damage,
            Some(guard), None, attempt).await?;
    }
    Ok(())
}




/// A damaged NPC boosts its shield and repairs its armor every repair cycle.
pub(super) fn npc_repair(world: &World, system: i64, id: i64, stats: &NpcStats) {
    let (shield, armor, _) = stats.repairs;
    if shield <= 0.0 && armor <= 0.0 { return; }
    world.post(system, move |s| {
        if !s.is_npc(id) { return; }
        let Some((max, current)) = s.combat_health_scalars(id) else { return };
        if max[2] <= 0.0 || current[2] >= max[2] { return; }
        let shield_changed = match s.repair_condition(id, 0, shield, crate::space::combat::RepairSource::Local) {
            Ok(changed) => changed,
            Err(error) => { tracing::warn!("NPC local shield repair refused: {error:?}"); false }
        };
        let armor_changed = match s.repair_condition(id, 1, armor, crate::space::combat::RepairSource::Local) {
            Ok(changed) => changed,
            Err(error) => { tracing::warn!("NPC local armor repair refused: {error:?}"); false }
        };
        if !shield_changed && !armor_changed { return; }
        let Some((max, current)) = s.combat_health_scalars(id) else { return };
        let health = [0, 1, 2].map(|i| if max[i] > 0.0 {
            if i == 0 { (current[i] / max[i]).clamp(0.0, 1.0) }
            else { (1.0 - current[i] / max[i]).clamp(0.0, 1.0) }
        } else { 0.0 });
        s.set_health(id, health);
    });
}

#[cfg(all(test, feature = "tests-combat"))] mod tests;


