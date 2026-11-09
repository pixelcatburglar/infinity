//! Sequential actor callbacks for drone work.
use std::sync::{Arc, Weak};
use tracing::warn;
use infinity_core::consts::{dogma::ATTR_DRONE_BANDWIDTH_USED, dogma::ATTR_DRONE_CONTROL_DISTANCE, dogma::ATTR_ENTITY_FLY_RANGE, dogma::ATTR_MAX_RANGE, dogma::ATTR_MAX_TARGET_RANGE, dogma::ATTR_RATE_OF_FIRE, dogma::ATTR_SCAN_RESOLUTION};
use crate::db::Db;
use crate::services::{CallError, fit::{Fit, Key}, effects::{self, EffectKind, JammingType, registry::{self, EffectAttribute}, space_source::{SourceContext, SourceGuard}}};
use crate::space::{World, drone::{DroneAction, DroneEffects, DroneOrder, DroneRecord, DronePhase, DroneSpec}};

fn supported(kind: EffectKind) -> bool {
    matches!(kind, EffectKind::Turret | EffectKind::Missile | EffectKind::Mining | EffectKind::RemoteRepair { .. }
        | EffectKind::Neutralizer | EffectKind::Ecm
        | EffectKind::Projected { jamming: JammingType::Webify | JammingType::Paint | JammingType::Damp | JammingType::TrackingDisrupt })
}

fn invalid_delegation(s: &crate::space::System, drone: &DroneRecord) -> bool {
    if let DroneOrder::Assist(delegate) | DroneOrder::Guard(delegate) = drone.order {
        !crate::services::fleets::same_fleet(drone.owner, delegate)
            || s.pilot_ball(delegate).is_none_or(|ball| ball.mode == crate::space::MODE_WARP || ball.cloak != 0)
    } else { false }
}

pub(crate) fn spec(fit: &Fit, key: Key, type_id: i32) -> Result<DroneSpec, CallError> {
    let sde = fit.sde();
    let effect = sde.types.effects(type_id).iter().filter_map(|row| registry::resolve(sde, row.effect_id)).find(|effect| supported(effect.kind));
    let duration_ms = effect.as_ref().map_or(0.0, |effect| {
        let attr = effect.attribute(EffectAttribute::Duration).unwrap_or(ATTR_RATE_OF_FIRE);
        fit.value(key, attr)
    });
    let optimal_range = effect.as_ref().and_then(|effect| effect.attribute(EffectAttribute::Range))
        .map_or_else(|| fit.value(key, ATTR_MAX_RANGE), |attr| fit.value(key, attr));
    Ok(DroneSpec {
        type_id,
        bandwidth: fit.value(key, ATTR_DRONE_BANDWIDTH_USED),
        scan_resolution: fit.value(key, ATTR_SCAN_RESOLUTION),
        control_range: fit.value(Key::Char, ATTR_DRONE_CONTROL_DISTANCE),
        max_target_range: fit.value(key, ATTR_MAX_TARGET_RANGE),
        optimal_range,
        orbit_range: fit.value(key, ATTR_ENTITY_FLY_RANGE),
        duration_ms,
        effect_id: effect.filter(|_| duration_ms.is_finite() && duration_ms >= 1.0).map(|effect| effect.id as i32),
    })
}

pub(crate) fn effects(db: Db, world: Weak<World>, system: i64) -> DroneEffects {
    Arc::new(move |s, action| -> Option<crate::space::NpcFuture> {
        let attempt = if let DroneAction::Cycle { drone, target, phase } = &action {
            let id = drone.id;
            let generation = drone.generation;
            if s.drone_current(id, generation) && s.drone(id).is_some_and(|current| invalid_delegation(s, &current)) {
                s.drone_idle(id, generation);
                return None;
            }
            let guard = SourceGuard::Drone { source: id, target: *target, generation };
            if !guard.valid(s) { return None; }
            let effect = drone.spec.effect_id.and_then(|id| s.sde().and_then(|sde| registry::resolve(sde, id)));
            let kind = effect.as_ref().map(|effect| effect.kind);
            let delayed = matches!(kind, Some(EffectKind::RemoteRepair { layer }) if !effects::repair::applies_at_start(layer));
            let tracked = matches!(kind, Some(EffectKind::Turret | EffectKind::Missile | EffectKind::RemoteRepair { .. }
                | EffectKind::Projected { jamming: JammingType::Scramble }));
            if tracked && matches!(phase, DronePhase::Begin) {
                let effect = effect.as_ref().unwrap();
                let weapon = matches!(effect.kind, EffectKind::Turret | EffectKind::Missile).then_some(drone.spec.type_id);
                let facts = match s.prepare_body_combat_source(id, None, Some(effect.id), weapon,
                    crate::lua::events::resources::FactOrigin::Natural, move |s| guard.valid(s)) {
                    Ok(facts) => facts,
                    Err(crate::space::combat_facts::Refusal::Full) => { crate::services::combat::capacity::refusal(system); return None; }
                    Err(crate::space::combat_facts::Refusal::Invalid | crate::space::combat_facts::Refusal::Replay | crate::space::combat_facts::Refusal::Busy) => return None,
                    Err(error) => { warn!("drone {id} source admission: {error:?}"); return None; }
                };
                let attempt = effects::space_source::PreparedCombatAttempt::new(facts, world.clone(), system);
                if delayed { s.drone_store_combat_attempt(id, generation, attempt); None } else { Some(attempt) }
            } else if delayed && matches!(phase, DronePhase::End) {
                s.drone_take_combat_attempt(id, generation)
            } else { None }
        } else { None };
        let db = db.clone();
        let world = world.clone();
        Some(Box::pin(async move {
            let Some(world) = world.upgrade() else { return };
            let (id, generation) = match &action {
                DroneAction::Cycle { drone, .. } | DroneAction::Contact { drone, .. } => (drone.id, drone.generation),
            };
            let result = match action {
                DroneAction::Cycle { drone, target, phase } => cycle(&db, &world, system, drone, target, phase, attempt).await,
                DroneAction::Contact { drone, return_to_bay } => crate::services::drones::inventory::returning::contact(&db, &world, system, drone, return_to_bay).await,
            };
            if let Err(error) = result {
                if crate::services::combat::capacity::is_refusal(&error) {
                    world.with(system, move |s| s.drone_idle(id, generation)).await;
                    return;
                }
                match error {
                    CallError::Internal(error) => warn!("drone {id}: {error:#}"),
                    CallError::User { msg, .. } => {
                        warn!("drone {id}: {msg}");
                        world.with(system, move |s| s.drone_idle(id, generation)).await;
                    }
                }
            }
        }))
    })
}

async fn cycle(db: &Db, world: &Arc<World>, system: i64, drone: DroneRecord, target: i64, phase: DronePhase,
    attempt: Option<effects::space_source::PreparedCombatAttempt>) -> Result<(), CallError> {
    let id = drone.id;
    let generation = drone.generation;
    let Some((current, actor_generation, fit_revision)) = world.with(system, move |s| {
        if !s.drone_current(id, generation) { return None; }
        let current = s.drone(id)?;
        if invalid_delegation(s, &current) {
            s.drone_idle(id, generation);
            return None;
        }
        if !s.drone_valid(id, target, generation) { return None; }
        Some((current, s.combat_membership_generation(id)?, s.combat_fit_revision(id)?))
    }).await.flatten() else { return Ok(()) };
    let Some(controller) = current.controller else { return Ok(()) };
    let fit = Fit::drone_in_space(db, world, system, current.owner, id).await?;
    let refreshed = spec(&fit, Key::Drone(id), current.spec.type_id)?;
    let Some(effect_id) = refreshed.effect_id else {
        return Err(crate::services::notify_error(infinity_core::messages::drones::NOT_AVAILABLE));
    };
    if drone.spec.effect_id != Some(effect_id) { return Ok(()); }
    let Some(effect) = registry::resolve(world.sde(), effect_id) else {
        return Err(effects::unsupported::refusal(Some(effect_id), world.sde().dogma.effect(effect_id).and_then(|row| row.name.as_deref()).unwrap_or("unknown"), current.spec.type_id));
    };
    let kinematics = effects::propulsion::kinematics(&fit, Key::Drone(id));
    let ship_revision = fit.projection_revision();
    let drone_revision = fit.drone_projection_revision();
    let guard = SourceGuard::Drone { source: id, target, generation };
    let Some((source, victim, current)) = world.with(system, move |s| {
        if !s.drone_current(id, generation) { return None; }
        let d = s.drone(id)?;
        if invalid_delegation(s, &d) {
            s.drone_idle(id, generation);
            return None;
        }
        if !guard.valid(s) { return None; }
        if d.controller != Some(controller) { return None; }
        if ship_revision != Some(s.projection_revision(controller)) || drone_revision != Some(s.projection_revision(id)) {
            s.refresh_projection_target(id);
            return None;
        }
        if !s.drone_refresh(id, generation, refreshed, kinematics, ship_revision.map(|revision| (controller, revision))) || !s.drone_valid(id, target, generation) { return None; }
        Some((s.sampled_ball(id)?, s.sampled_ball(target)?, s.drone(id)?))
    }).await.flatten() else { return Ok(()) };
    if attempt.as_ref().is_some_and(|attempt| attempt.facts().effect_id != Some(effect.id)) { return Ok(()); }
    let cx = SourceContext {
        db, world, system, source: &source, target: &victim, module: id, effect: &effect,
        duration_ms: current.spec.duration_ms, fit: &fit, key: Key::Drone(id),
        guard: SourceGuard::Drone { source: id, target, generation }, attacker: Some(current.owner),
        drone: Some(&current), phase,
    };
    let result = effects::space_source::apply(&cx, attempt).await;
    let owner = current.owner;
    world.with(system, move |s| {
        if s.drone_current(id, generation)
            && s.drone(id).is_some_and(|d| d.owner == owner && d.controller == Some(controller))
            && ship_revision == Some(s.projection_revision(controller))
            && drone_revision == Some(s.projection_revision(id)) {
            s.replace_fitted_cache(id, actor_generation, fit_revision, fit, Key::Drone(id), drone_revision);
        }
    }).await;
    result
}

#[cfg(test)] mod tests;
