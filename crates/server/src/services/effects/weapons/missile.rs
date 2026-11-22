//! Missile projectile payloads and their native-steered flight launches,
//! shared by fitted launchers and NPC or drone sources; the transient
//! projectile ball is also built for Defender interceptions. A launched
//! charge flies as a transient projectile ball the actor steers into swept
//! target contact or expires at its frozen effective lifetime. A committed
//! contact applies explosion radius and velocity against the target in that
//! actor turn; a charge carrying dotMissileLaunching registers a Breacher pod
//! damage-over-time application instead of resolving typed damage. Sources
//! are `Sde` charge and type data plus the admitted combat facts; no flight
//! survives its system actor or crosses to another.
use super::{defer_hit_report, missile_applied, next_missile_id, report_hit, target_signature,
    HIT_MISS};
use crate::db::Db;
use crate::sde::Sde;
use crate::services::CallError;
use crate::services::combat::damage::{apply_damage_in_actor, DamageSource};
use crate::services::effects::breacher;
use crate::services::effects::space_source::{PreparedCombatAttempt, SourceGuard};
use crate::space::{dist, now_ms, Ball, World};
use marshal::Value;
use std::sync::Arc;

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
    /// Breacher pod damage-over-time payload, present when the launched charge
    /// carries the dotMissileLaunching effect; contact registers a DOT
    /// application instead of resolving typed damage.
    pub(crate) dot: Option<breacher::DotPayload>,
}


/// Build the transient projectile ball shared by ordinary, Defender and
/// festival launches. `charge` and `velocity` are the only payload fields
/// the ball carries; combat damage stays with the caller.
pub(in crate::services::effects) fn build_projectile_ball(sde: &Sde, from: &Ball, target: &Ball, owner: i64, module: Option<i64>, charge: i32, velocity: f64) -> Result<Ball, CallError> {
    let size = sde.types.get(charge)
        .ok_or_else(|| CallError::Internal(anyhow::anyhow!("unknown type {charge}")))?;
    let mut slim = vec![("sourceShipID", Value::Int(from.id))];
    slim.push(("launchModules", Value::list(module.map(Value::Int))));
    let launch_ms = now_ms();
    Ok(Ball {
        id: next_missile_id(),
        type_id: charge,
        owner_id: owner,
        corp_id: from.corp_id,
        alliance_id: -1,
        char_id: None,
        signature_radius: None,
        mode: crate::space::MODE_STOP,
        radius: size.radius.unwrap_or(1.0) as f32,
        mass: size.mass.unwrap_or(1.0),
        max_vel: velocity as f32,
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
    })
}

/// Launch a native-steered missile flight; the actor sweeps target contact and
/// expires the flight at its frozen effective lifetime. A committed contact
/// applies explosion radius and velocity against the target at that actor turn.
pub(crate) async fn launch_missile(db: Db, world: Arc<World>, system: i64, from: Ball, target: Ball, owner: i64, module: Option<i64>, m: Missile, guard: Option<SourceGuard>, mut attempt: PreparedCombatAttempt) -> Result<(), CallError> {
    if !m.flight_time_ms.is_finite() || m.flight_time_ms <= 0.0 {
        return Err(CallError::Internal(anyhow::anyhow!("invalid missile flight time {}", m.flight_time_ms)));
    }
    let missile = build_projectile_ball(world.sde(), &from, &target, owner, module, m.charge, m.velocity)?;
    let missile_id = missile.id;
    let flight_time_ms = m.flight_time_ms;
    let shooter_id = from.id;
    let target_id = target.id;
    let facts = attempt.facts();
    let shooter_pilot = facts.actual_actor_character;
    let (aoe_cloud, aoe_velocity, drf, raw, weapon, dot) =
        (m.aoe_cloud, m.aoe_velocity, m.drf, m.raw, m.weapon, m.dot);
    let sde = world.sde().clone();
    let world_ref = Arc::downgrade(&world);
    let impact_db = db.clone();
    world.system(system).await.map_err(CallError::Internal)?;
    let Some((victim_pilot, target_generation)) = world.with(system, move |s| {
        if guard.is_some_and(|guard| !guard.valid(s)) { return None; }
        Some((s.combat_owner(target_id), s.combat_membership_generation(target_id)?))
    }).await.flatten() else { return Ok(()) };
    let drone_admission = matches!(guard, Some(SourceGuard::Drone { .. }))
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
            if let Some(payload) = dot {
                // A breacher pod contact registers the damage-over-time
                // application; the launch itself resolves as a miss report
                // and deals no typed damage.
                let registered = breacher::apply_pod_contact(s, impact_db.clone(), world_ref.clone(),
                    target_id, &facts, module, weapon, illegal, payload);
                if !registered {
                    tracing::debug!(missile = missile_id, target = target_id, "breacher pod contact rejected");
                }
                attempt.finish_in_actor(s);
                s.defer(async move {
                    if let Some(world) = world_ref.upgrade() {
                        report_hit(&world, shooter_id, target_id, shooter_pilot, victim, weapon, 0.0, HIT_MISS, false);
                    }
                });
                return;
            }
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
            defer_hit_report(s, world_ref.clone(), missile_id, shooter_id, target_id, shooter_pilot,
                victim, weapon, dealt, receipt, capacity.then_some(attempt));
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

