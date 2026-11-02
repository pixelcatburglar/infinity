//! Physical mining intents and post-contact extraction for drones.
use std::sync::Arc;
use crate::db::Db;
use crate::services::{CallError, fit::{Fit, Key}, mining};
use crate::space::{dist, drone::{DroneOrder, DroneRecord}, World};

pub(crate) async fn finish_cycle(_db: &Db, world: &Arc<World>, system: i64, drone: DroneRecord, target: i64, fit: &Fit) -> Result<(), CallError> {
    let Some(type_id) = mining::rock_type(target, system) else {
        world.with(system, move |s| s.drone_idle(drone.id, drone.generation)).await;
        return Ok(());
    };
    let volume = world.sde().types.get(type_id).ok_or_else(|| CallError::Internal(anyhow::anyhow!("missing type {type_id}")))?.volume.unwrap_or(1.0).max(0.01);
    let range = drone.spec.optimal_range.max(0.0);
    let id = drone.id;
    let generation = drone.generation;
    let values = mining::yield_values(fit, Key::Drone(id));
    world.with(system, move |s| {
        if !s.drone_valid(id, target, generation) || !matches!(s.drone(id).map(|d| d.order), Some(DroneOrder::Mine(t)) if t == target) { return; }
        let Some((source, rock)) = s.ball(id).zip(s.ball(target)) else { return };
        let Some((a, b)) = s.sample_position(id).zip(s.sample_position(target)) else { return };
        if dist(a, b) - f64::from(source.radius) - f64::from(rock.radius) > range { return; }
        let pending = mining::yield_intent(values, target, type_id, volume);
        s.drone_pending(id, generation, pending);
    }).await;
    Ok(())
}

