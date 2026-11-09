//! NPC combat uses SDE entity and charge attributes, projected Fit values and
//! shared weapon application; routing and state live in the parent module.
//! Queued shots revalidate the actor's current target and ECM lockout.
//! The client has no NPC server combat implementation to mirror.
use super::*;
use crate::services::effects::{notify::fire_fx, weapons::{launch_missile, turret_shot, Missile}};
use infinity_core::consts::dogma::ATTR_AGILITY;
use infinity_core::consts::{dogma::ATTR_TRACKING_SPEED, dogma::ATTR_OPTIMAL_SIG_RADIUS, dogma::ATTR_AOE_VELOCITY, dogma::ATTR_AOE_DAMAGE_REDUCTION_FACTOR, dogma::ATTR_MISSILE_ENTITY_VELOCITY_MULTIPLIER, dogma::ATTR_MISSILE_ENTITY_FLIGHT_TIME_MULTIPLIER, dogma::ATTR_MISSILE_ENTITY_AOE_VELOCITY_MULTIPLIER, dogma::ATTR_MISSILE_ENTITY_AOE_CLOUD_SIZE_MULTIPLIER};
use crate::sde::types::TypeData;
use crate::space::{MODE_STOP, V3};

/// SDE optimalSigRadius attribute metadata default when metadata is absent.
const DEFAULT_OPTIMAL_SIG_RADIUS: f64 = 1000.0;

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
    world.add_ship(system, ball).await.map_err(CallError::Internal)?;
    spawn_npc(db.clone(), world.clone(), system, id, data.type_id, owner).await?;
    Ok(id)
}

// ---------------------------------------------------------------- NPCs

/// An NPC's fighting attributes (entity attributes on its type).
pub(super) struct NpcStats {
    /// (shield boost, armor repair) amount per repair cycle, and the cycle (ms).
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
}

pub(super) async fn npc_stats(db: &Db, sde: &Sde, type_id: i32) -> Result<NpcStats, CallError> {
    let multiplier = attr(sde, type_id, ATTR_DAMAGE_MULTIPLIER, 1.0);
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
    Ok(NpcStats {
        scan_resolution: attr(sde, type_id, ATTR_SCAN_RESOLUTION, 0.0),
        repairs: (
            attr(sde, type_id, ATTR_ENTITY_SHIELD_BOOST_AMOUNT, 0.0),
            attr(sde, type_id, ATTR_ENTITY_ARMOR_REPAIR_AMOUNT, 0.0),
            attr(sde, type_id, ATTR_ENTITY_ARMOR_REPAIR_DURATION, 0.0).max(attr(sde, type_id, ATTR_ENTITY_SHIELD_BOOST_DURATION, 0.0)).max(5_000.0),
        ),
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
    })
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
pub(super) fn npc_missile_kinematics(velocity: f64, flight_ms: f64, velocity_multiplier: f64, flight_multiplier: f64) -> (f64, f64) {
    let velocity = (velocity * velocity_multiplier).max(1.0);
    (velocity, velocity * ms_to_seconds(flight_ms * flight_multiplier))
}


/// `me` and `victim` are the actor-sampled shot snapshots captured before worker dispatch.
pub(super) async fn npc_shot(db: &Db, world: &Arc<World>, system: i64, id: i64, type_id: i32, stats: &NpcStats, me: Ball, victim: Ball, generation: u64,
    missile_attempt: Option<crate::services::effects::space_source::PreparedCombatAttempt>, turret_attempt: Option<crate::services::effects::space_source::PreparedCombatAttempt>) -> Result<(), CallError> {
    let target = victim.id;
    let Some(Some(projections)) = world.with(system, move |s|
        s.npc_projection_valid(id, target, generation).then(|| s.projections_on(id))).await else { return Ok(()) };
    let sde = world.sde();
    let fit = Fit::for_space_type(sde.clone(), type_id, projections);
    let optimal = if fit.type_has_attribute(Key::Ship, ATTR_MAX_RANGE) { fit.value(Key::Ship, ATTR_MAX_RANGE) } else { stats.optimal };
    let falloff = if fit.type_has_attribute(Key::Ship, ATTR_FALLOFF) { fit.value(Key::Ship, ATTR_FALLOFF) } else { stats.falloff };
    let velocity_multiplier = npc_effective(&fit, sde, ATTR_MISSILE_ENTITY_VELOCITY_MULTIPLIER, 1.0);
    let flight_multiplier = npc_effective(&fit, sde, ATTR_MISSILE_ENTITY_FLIGHT_TIME_MULTIPLIER, 1.0);
    let tracking = npc_effective(&fit, sde, ATTR_TRACKING_SPEED, stats.tracking);
    let aoe_velocity_multiplier = npc_effective(&fit, sde, ATTR_MISSILE_ENTITY_AOE_VELOCITY_MULTIPLIER, 1.0);
    let aoe_cloud_multiplier = npc_effective(&fit, sde, ATTR_MISSILE_ENTITY_AOE_CLOUD_SIZE_MULTIPLIER, 1.0);
    let ship = victim.id;
    let gap = (dist(me.pos, victim.pos) - me.radius as f64 - victim.radius as f64).max(0.0);
    let owner_id = me.owner_id;
    let can_turret = stats.damage.iter().any(|d| *d > 0.0) && gap <= optimal + falloff * NPC_TURRET_REACH_FALLOFF_MULTIPLIER;
    let guard = crate::services::effects::space_source::SourceGuard::Npc { source: id, target, generation };
    if let Some((charge, multiplier)) = &stats.missile {
        let (charge, multiplier) = (*charge, *multiplier);
        let (velocity, flight) = npc_missile_kinematics(
            attr(world.sde(), charge, ATTR_MAX_VELOCITY, NPC_MISSILE_VELOCITY_MPS),
            attr(world.sde(), charge, ATTR_EXPLOSION_DELAY, NPC_MISSILE_FLIGHT_MS),
            velocity_multiplier, flight_multiplier);
        if gap <= flight {
            let attempt = missile_attempt.ok_or_else(|| CallError::Internal(anyhow::anyhow!("missing NPC missile admission")))?;
            let missile = Missile {
                charge,
                raw: ATTR_DAMAGE.map(|a| attr(world.sde(), charge, a, 0.0) * multiplier),
                aoe_cloud: attr(world.sde(), charge, ATTR_AOE_CLOUD_SIZE, NPC_MISSILE_EXPLOSION_RADIUS_METRES) * aoe_cloud_multiplier,
                aoe_velocity: attr(world.sde(), charge, ATTR_AOE_VELOCITY, 0.0) * aoe_velocity_multiplier,
                drf: sde.types.attribute(charge, ATTR_AOE_DAMAGE_REDUCTION_FACTOR)
                    .or_else(|| sde.dogma.attribute(ATTR_AOE_DAMAGE_REDUCTION_FACTOR).and_then(|a| a.default_value))
                    .unwrap_or(1.0),
                velocity,
                flight_range: flight,
                weapon: type_id,
                illegal: false,
            };
            launch_missile(db.clone(), world.clone(), system, me.clone(), victim.clone(), owner_id, None, missile,
                Some(guard), attempt).await?;
        }
    }
    if can_turret {
        let attempt = turret_attempt.ok_or_else(|| CallError::Internal(anyhow::anyhow!("missing NPC turret admission")))?;
        if let Some(guid) = stats.fx.as_deref() {
            let guid = guid.to_owned();
            let duration = stats.rate_of_fire_ms;
            world.broadcast(system, id, move |stamp| vec![fire_fx(stamp, id, None, &guid, Some(ship), None, true, true, duration, 1)]);
        }
        turret_shot(db, world, system, &me, &victim, None, type_id, false,
            (optimal, falloff, tracking, stats.opt_sig), stats.damage,
            Some(guard), attempt).await?;
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


