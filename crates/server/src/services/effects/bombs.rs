//! Bomb charges fly straight before applying area damage at their sampled position.
//! Mirrors tactical.py's charge explosionDelay, maxVelocity and empFieldRange;
//! flight is transient actor state. Dynamic ship orientation is not represented.
use super::aoe;
use super::lifecycle::{Cycle, CycleOutcome, Module};
use super::registry::Effect;
use super::weapons::{consume_charge, next_missile_id};
use infinity_core::consts::{dogma::ATTR_EMP_FIELD_RANGE, dogma::ATTR_EXPLOSION_DELAY, dogma::ATTR_MAX_VELOCITY};
use crate::services::combat::{self, Ctx, ATTR_DAMAGE};
use crate::services::fit::{Fit, Key};
use crate::services::CallError;
use crate::space::{Ball, MODE_STOP, MODE_WARP};
use marshal::Value;
use std::sync::{Arc, atomic::AtomicBool};

fn generation_valid(master: i64, generation: &Arc<AtomicBool>) -> bool {
    combat::state().active.get(&master).is_some_and(|active| Arc::ptr_eq(&active.stop, generation))
}

pub(super) async fn validate(cx: &Ctx, fit: &Fit, module: &Module, effect: &Effect, target: Option<i64>) -> Result<(), CallError> {
    aoe::validate(cx, fit, module, effect, target).await
}

pub(super) async fn start(cx: &Ctx, fit: &Fit, cycle: &mut Cycle) -> Result<CycleOutcome, CallError> {
    let master = cycle.modules[0].id;
    for index in 0..cycle.modules.len() {
        let module = cycle.modules[index].clone();
        let key = Key::Charge(module.id);
        let Some(expected) = fit.source_type(key) else { return Ok(CycleOutcome::Stop("out of charges")); };
        let Some(attempt) = super::lifecycle::prepare_combat_source(cycle, module.id, Some(module.type_id)).await? else {
            return Ok(CycleOutcome::Stop("source lost or activation replaced"));
        };
        let velocity = fit.value(key, ATTR_MAX_VELOCITY);
        let delay = fit.value(key, ATTR_EXPLOSION_DELAY);
        let radius = fit.value(key, ATTR_EMP_FIELD_RANGE);
        let raw = ATTR_DAMAGE.map(|attr| fit.value(key, attr).max(0.0));
        let Some(size) = cx.world.sde().types.get(expected) else {
            return Err(CallError::Internal(anyhow::anyhow!("unknown bomb type {expected}")));
        };
        let (mass, ball_radius) = (size.mass.unwrap_or(1.0), size.radius.unwrap_or(1.0) as f32);
        if !velocity.is_finite() || velocity <= 0.0 || !delay.is_finite() || delay < 0.0 || !radius.is_finite() || radius < 0.0 {
            return Err(CallError::Internal(anyhow::anyhow!("invalid fitted bomb flight attributes")));
        }
        let ship_id = cx.ship_id;
        let generation = cycle.stop.clone();
        let Some((source, direction)) = cx.world.with(cx.system_id, move |s| {
            if !generation_valid(master, &generation) { return None; }
            let source = s.sampled_ball(ship_id).filter(|ship| ship.mode != MODE_WARP)?;
            Some((source, s.heading(ship_id)?))
        }).await.flatten() else { return Ok(CycleOutcome::Stop("source lost, in warp or activation replaced")); };
        let Some(charge) = consume_charge(cx, &module, Some(expected)).await? else { return Ok(CycleOutcome::Stop("out of charges")); };
        cycle.charge_type = Some(charge);
        let id = next_missile_id();
        let launch_ms = crate::space::now_ms();
        let bomb = Ball {
            id, type_id: charge, owner_id: cx.char_id, corp_id: source.corp_id, alliance_id: -1,
            char_id: None, signature_radius: None, mode: MODE_STOP, radius: ball_radius,
            mass, max_vel: velocity as f32, agility: 1.0, speed_fraction: 0.0,
            pos: source.pos, vel: [0.0; 3], old_pos: source.pos, old_vel: [0.0; 3], segment_acceleration: None,
            goto: source.pos, follow_id: 0, follow_range: 0.0, free: true, global: false,
            massive: false, interactive: false, warp: crate::space::WarpState::default(), 
            last_ms: launch_ms, interpolation_anchor_ms: launch_ms, effect_stamp: 0, cloak: 0, cloak_until: 0,
            health: [1.0; 3], launcher: 0,
            slim: vec![("sourceShipID", Value::Int(ship_id)), ("launchModules", Value::list([Value::Int(module.id)]))],
        };
        let delay_ms = crate::space::seconds_to_ms(crate::space::ms_to_seconds(delay));
        let weapon = module.type_id;
        let context = cx.clone();
        cx.world.with(cx.system_id, move |s| {
            s.launch_bomb(bomb, direction, delay_ms, move |s| {
                let Some(center) = s.sample_position(id) else { return };
                let victims = aoe::victims(s, id, center, radius);
                s.destroy_ball(id);
                s.defer(async move {
                    match aoe::apply_damage_guarded(&context.db, &context.world, context.system_id, weapon, victims, raw, None, attempt, None).await {
                        Ok(()) => {}
                        Err(CallError::Internal(error)) => tracing::warn!(%error, bomb = id, "bomb damage failed"),
                        Err(CallError::User { msg, dict }) if msg == infinity_core::messages::MSG_CUSTOM_NOTIFY
                            && dict == vec![("notify".into(), Value::unicode(infinity_core::messages::effects::COMBAT_CAPACITY_FULL))] => {}
                        Err(CallError::User { msg, dict }) => {
                            context.notify("OnRemoteMessage", vec![Value::str(msg), Value::dict(dict)]);
                        }
                    }
                });
            });
        }).await.ok_or_else(|| CallError::Internal(anyhow::anyhow!("system actor stopped")))?;
    }
    Ok(CycleOutcome::Continue)
}

#[cfg(test)] mod tests;
