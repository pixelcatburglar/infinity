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
//! Salvage orders run the shared wreck recovery and carry real cargo home;
//! reconnect commands are not supported by the drone services.

use super::*;
use crate::services::mining::rounding::{MiningDraws, MiningInputs};
use std::sync::atomic::AtomicBool;
mod core;
pub(super) mod automatic;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum DroneOrder { Idle, Engage(i64), Mine(i64), Salvage(Option<i64>), ReturnHome, ReturnBay, Assist(i64), Guard(i64) }

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
    /// Real owner's persisted NPC-ignore flag, installed onto the brain at registration.
    pub(crate) npc_ignored: bool,
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
    Contact { drone: DroneRecord, return_to_bay: bool, carried: bool },
}

pub(crate) type DroneEffects = Arc<dyn Fn(&mut System, DroneAction) -> Option<NpcFuture> + Send + Sync>;

pub(in crate::space) struct Reservation { owner: i64, controller: i64, bandwidths: Vec<f64> }
/// Transient salvage work state owned by the brain; the durable cargo itself
/// is never mirrored here and is re-read inside every custody transaction.
#[derive(Default)]
struct DroneSalvage {
    /// Standing intent: reselect the owner's nearest wreck at event boundaries.
    auto: bool,
    /// A committed recovery is aboard and flies home for delivery.
    carried: bool,
    /// Delivery was refused: hold position until the next player command.
    blocked: bool,
    /// One durable end settlement is in flight; no further phase queues.
    in_flight: bool,
    /// An unproven settlement closed the work; only cold restore lifts it.
    unknown: bool,
    /// The work's effect and settlement identity, allocated only when work or
    /// target changes; `true` once salvage work has left and vetoes the
    /// in-flight durable write.
    fx: Option<std::sync::Arc<AtomicBool>>,
}
pub(in crate::space) struct DroneBrain {
    record: DroneRecord,
    effects: DroneEffects,
    lock_at: i64,
    locked: bool,
    cycle_at: i64,
    end_at: Option<i64>,
    combat_attempt: Option<crate::services::effects::space_source::PreparedCombatAttempt>,
    contact_queued: bool,
    contact_claimed: Option<u64>,
    kinematics: Option<crate::services::effects::propulsion::Kinematics>,
    announced: bool,
    npc_ignored: bool,
    salvage: DroneSalvage,
}
