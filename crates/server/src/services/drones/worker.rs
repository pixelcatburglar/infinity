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

/// The first supported effect on the drone's type, from its fitted data.
fn type_effect(fit: &Fit, type_id: i32) -> Option<registry::Effect> {
    let sde = fit.sde();
    sde.types.effects(type_id).iter().filter_map(|row| registry::resolve(sde, row.effect_id)).find(|effect| supported(effect.kind))
}

/// The drone effect's cycle duration: its Duration attribute or rate of fire.
fn duration_ms(fit: &Fit, key: Key, effect: &registry::Effect) -> f64 {
    let attr = effect.attribute(EffectAttribute::Duration).unwrap_or(ATTR_RATE_OF_FIRE);
    fit.value(key, attr)
}

pub(crate) fn spec(fit: &Fit, key: Key, type_id: i32) -> Result<DroneSpec, CallError> {
    let effect = type_effect(fit, type_id);
    let duration_ms = effect.as_ref().map_or(0.0, |effect| duration_ms(fit, key, effect));
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

/// One phase's captured effect and inputs from the prepared resident fit.
struct CapturedPhase {
    effect: registry::Effect,
    duration_ms: f64,
    inputs: Result<crate::services::effects::parameters::SourceInputs, CallError>,
}

/// Borrow the resident fit once for this phase: the drone type's first
/// supported effect and its current duration select the real capture. The
/// outer `None` is a missing or stale actor record; the inner `None` is a
/// phase with no runnable effect this turn.
fn capture_phase_inputs(s: &mut crate::space::System, id: i64, type_id: i32) -> Option<Option<CapturedPhase>> {
    s.with_combat_fit(id, move |fit, key, _| {
        let effect = type_effect(fit, type_id)?;
        let duration_ms = duration_ms(fit, key, &effect);
        if !(duration_ms.is_finite() && duration_ms >= 1.0) { return None; }
        let inputs = effects::parameters::capture_source(fit, key, &effect, duration_ms);
        Some(CapturedPhase { effect, duration_ms, inputs })
    })
}


pub(crate) fn effects(db: Db, world: Weak<World>, system: i64) -> DroneEffects {
    Arc::new(move |s, action| -> Option<crate::space::NpcFuture> {
        let mut membership = None;
        let attempt = if let DroneAction::Cycle { drone, target, phase } = &action {
            let id = drone.id;
            let generation = drone.generation;
            membership = s.combat_membership_generation(id);
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
                DroneAction::Cycle { drone, target, phase } => cycle(&db, &world, system, drone, target, phase, membership, attempt).await,
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
    membership: Option<u64>, attempt: Option<effects::space_source::PreparedCombatAttempt>) -> Result<(), CallError> {
    let id = drone.id;
    let generation = drone.generation;
    let guard = SourceGuard::Drone { source: id, target, generation };
    enum PhaseGate {
        Unavailable,
        Ready { captured: CapturedPhase, source: crate::space::Ball, victim: crate::space::Ball },
    }
    let Some((current, gate)) = world.with(system, move |s| {
        if !s.drone_current(id, generation) || s.combat_membership_generation(id) != membership { return None; }
        let before = s.drone(id)?;
        if invalid_delegation(s, &before) {
            s.drone_idle(id, generation);
            return None;
        }
        if !guard.valid(s) { return None; }
        let type_id = before.spec.type_id;
        // The gate's prepare publishes expiry outputs first; the phase effect
        // and its duration are evaluated on the prepared fit, never on the
        // possibly stale record values held by the queued action.
        let captured = capture_phase_inputs(s, id, type_id);
        let current = s.drone(id)?;
        if !s.drone_current(id, generation) || s.combat_membership_generation(id) != membership
            || invalid_delegation(s, &current) || !guard.valid(s)
            || current.owner != before.owner || current.controller != before.controller { return None; }
        let Some(captured) = captured else { return None };
        let Some(phase) = captured else { return Some((current, PhaseGate::Unavailable)) };
        match current.spec.effect_id {
            Some(spec_id) if spec_id != phase.effect.id as i32 => return None,
            Some(_) => {}
            None => return Some((current, PhaseGate::Unavailable)),
        }
        let (source, victim) = s.sampled_ball(id).zip(s.sampled_ball(target))?;
        Some((current, PhaseGate::Ready { captured: phase, source, victim }))
    }).await.flatten() else { return Ok(()) };
    let PhaseGate::Ready { captured: CapturedPhase { effect, duration_ms, inputs }, source, victim } = gate else {
        return Err(crate::services::notify_error(infinity_core::messages::drones::NOT_AVAILABLE));
    };
    if drone.spec.effect_id != Some(effect.id as i32) { return Ok(()); }
    let inputs = inputs?;
    if attempt.as_ref().is_some_and(|attempt| attempt.facts().effect_id != Some(effect.id)) { return Ok(()) }
    let cx = SourceContext {
        db, world, system, source: &source, target: &victim, module: id, effect: &effect,
        duration_ms, inputs: &inputs,
        guard: SourceGuard::Drone { source: id, target, generation }, attacker: Some(current.owner),
        drone: Some(&current), phase,
    };
    effects::space_source::apply(&cx, attempt).await
}

#[cfg(all(test, feature = "tests-combat"))] mod tests;
