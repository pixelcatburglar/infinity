//! Propulsion kinematics applied through the system's destiny command queue.
//! Client destiny updates are consumed by `eve/client/script/remote/michelle.py`.
//! Values come from the fitted ship's SDE and item attributes; changes are transient
//! and do not model native propulsion physics beyond the fitted attributes.
use std::sync::{Arc, atomic::AtomicBool};

use crate::consts::{dogma::ATTR_MASS, dogma::ATTR_MAX_VELOCITY, dogma::ATTR_SIGNATURE_RADIUS};
use crate::services::{combat::{self, Ctx}, fit::{Fit, Key}};
use crate::space::{event, Ball, System};
use marshal::Value;

#[derive(Clone, Copy, PartialEq)]
pub(crate) struct Kinematics { speed: f64, mass: f64, agility: f64, signature: f64 }

pub(crate) fn kinematics(fit: &Fit, key: Key) -> Kinematics {
    Kinematics { speed: fit.value(key, ATTR_MAX_VELOCITY), mass: fit.value(key, ATTR_MASS),
        agility: fit.value(key, crate::consts::dogma::ATTR_AGILITY), signature: fit.value(key, ATTR_SIGNATURE_RADIUS) }
}

pub(crate) fn queue_kinematics(system: &mut System, ship: i64, values: Kinematics) {
    system.command(ship, move |b, stamp, _| {
        let mut events = propulsion_transition(b, stamp, values.speed, values.mass, values.signature);
        if b.set_ball_agility(values.agility) {
            events.push(event(stamp, "SetBallAgility", vec![Value::Int(b.id), Value::Float(values.agility)]));
        }
        events
    });
}

/// The ship's current fitted kinematics for everyone's destiny.
pub(super) async fn apply_propulsion(cx: &Ctx, master: i64, generation: Arc<AtomicBool>, active: bool) {
    let Ok(fit) = Fit::load_in_space(&cx.db, &cx.world, cx.system_id, cx.char_id, cx.ship_id).await else { return };
    let Some(revision) = fit.projection_revision() else { return };
    let values = kinematics(&fit, Key::Ship);
    let ship = cx.ship_id;
    cx.world.post(cx.system_id, move |s| {
        let mut st = combat::state();
        if !st.activation_generation.get(&master).is_some_and(|current| Arc::ptr_eq(current, &generation))
            || st.active.contains_key(&master) != active { return; }
        if s.projection_revision(ship) == revision {
            s.queue_projection_kinematics(ship, revision, values);
        } else {
            s.refresh_projection_target(ship);
        }
        if !active { st.activation_generation.remove(&master); }
    });
}

fn propulsion_transition(b: &mut Ball, stamp: i64, speed: f64, mass: f64, signature: f64) -> Vec<Value> {
    let speed_accepted = b.set_max_speed(speed);
    let mass_accepted = b.set_ball_mass(mass);
    b.signature_radius = Some(signature);
    let mut events = Vec::new();
    if speed_accepted {
        events.push(event(stamp, "SetMaxSpeed", vec![Value::Int(b.id), Value::Float(speed)]));
    }
    if mass_accepted {
        events.push(event(stamp, "SetBallMass", vec![Value::Int(b.id), Value::Float(mass)]));
    }
    events
}

#[cfg(test)] mod tests;
