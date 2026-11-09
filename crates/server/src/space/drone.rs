//! Manages drone balls in a system: orders, control, the drone brain and automatic engagement.
//! Client commands are in eve/client/script/ui/services/menuSvcExtras/droneFunctions.py.
//! Uses system actor state, drone specs and shared Sde; no tables directly.
//! Published category 18 drone types have no maxTargetRange (76) or
//! scanResolution (564) rows; both dogma defaults are zero. Zero range adds
//! no drone-local lock limit, and zero scan resolution locks immediately in
//! action range. Owner control and action range remain enforced.
//! Undelivered mining intents are transient actor state.
//! Automatic aggression is server policy: focus fire selects the lowest eligible
//! aggressor; otherwise sorted idle drones distribute round-robin by target ID.
//! The client names these preferences but does not specify a selection algorithm.
//! Salvage and reconnect commands are not supported by the drone services.

use super::*;
use crate::services::mining::rounding::{MiningDraws, MiningInputs};
mod core;
pub(super) mod automatic;
pub(super) use core::{DroneBrain, Reservation};

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum DroneOrder { Idle, Engage(i64), Mine(i64), ReturnHome, ReturnBay, Assist(i64), Guard(i64) }

#[derive(Clone)]
pub(crate) struct DroneSpec {
    pub type_id: i32,
    pub bandwidth: f64,
    pub scan_resolution: f64,
    pub control_range: f64,
    pub max_target_range: f64,
    pub optimal_range: f64,
    pub orbit_range: f64,
    pub duration_ms: f64,
    pub effect_id: Option<i32>,
}

pub(crate) struct PreparedDrone {
    pub(crate) ball: Ball,
    pub(crate) spec: DroneSpec,
    pub(crate) condition: crate::services::combat::ConditionSeed,
    pub(crate) fit: crate::services::fit::Fit,
    pub(crate) key: crate::services::fit::Key,
}

#[derive(Clone, Debug)]
pub(crate) struct PendingMining {
    pub rock: i64,
    pub type_id: i32,
    pub inputs: MiningInputs,
    pub unit_volume: f64,
    pub draws: MiningDraws,
}

#[derive(Clone)]
pub(crate) struct DroneRecord {
    pub id: i64,
    pub owner: i64,
    pub controller: Option<i64>,
    pub controller_owner: Option<i64>,
    pub generation: u64,
    pub order: DroneOrder,
    pub target: Option<i64>,
    pub state: i32,
    pub spec: DroneSpec,
    pub pending: Option<PendingMining>,
}

#[derive(Clone, Copy)]
pub(crate) enum DronePhase { Begin, End }

#[derive(Clone)]
pub(crate) enum DroneAction {
    Cycle { drone: DroneRecord, target: i64, phase: DronePhase },
    Contact { drone: DroneRecord, return_to_bay: bool },
}

pub(crate) type DroneEffects = Arc<dyn Fn(&mut System, DroneAction) -> Option<NpcFuture> + Send + Sync>;
