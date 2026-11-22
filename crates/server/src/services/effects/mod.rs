//! Numeric classification of dogma effects for the shared effects lifecycle.
//!
//! IDs and modifier rows come from the SDE; client dogma effect constants and
//! `eve/client/script/dogma/clientDogmaLocation.py` supply behavior references;
//! pythonEffects supply corresponding behavior names. Supported families,
//! including projected effects, ECM, command bursts and breach control, use
//! the shared lifecycle and actor state; unsupported families remain visibly refused.
//! Cycle and source phases read typed parameter packets captured from the
//! actor-resident fit. An admitted ordinary festival launcher keeps the
//! missile kind and flies a harmless native projectile; direct snowball
//! launching stays refused. Wreck recovery is shared between the salvager module
//! and salvage drones. Micro jump relocation reuses one queued SetBallPosition
//! guard for the drive and the field generator's carried ships.
pub(crate) mod registry;
pub(crate) mod space_source;
pub(crate) mod parameters;
pub(crate) mod projection;
pub(crate) mod projected;
pub(crate) mod range;
pub(crate) mod environment;
pub(crate) mod notify;
pub(crate) mod heat;
pub(crate) mod unsupported;
pub(crate) mod propulsion;
pub(crate) mod weapons;
mod festival;
pub(crate) mod spool;
pub(crate) mod breacher;
pub(crate) mod breach_control;
pub(in crate::services::effects) mod fx;
pub(crate) mod hacking;
pub(crate) mod lifecycle;
pub(crate) mod active_cycle;
mod fuel;
mod entosis;
mod behaviors;
pub(crate) mod online;
pub(crate) mod fitting;
pub(crate) mod aoe;
pub(crate) mod defenders;
pub(crate) mod super_weapon;
mod bombs;
pub(crate) mod vorton;
pub(crate) mod cloak;
mod mjd;
mod mjfg;
mod compression;
mod tractor;
mod scan;
pub(in crate::services) mod salvage;
mod targeting;
pub(crate) mod repair;
mod mining;
mod capacitor;
pub(crate) mod npc_capacitor;
pub(crate) mod npc_repair;
pub(crate) mod ecm;
pub(crate) mod command_burst;
mod interdiction;
pub(crate) mod probes;
pub(in crate::services::effects) mod command_burst_fx;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum JammingType {
    Webify, Scramble, Disrupt, Paint, Damp, TrackingDisrupt, GuidanceDisrupt,
    SensorBoost, TrackingBoost,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EffectKind {
    Modifiers, Turret, Missile, Smartbomb, Vorton, SuperWeapon,
    LocalRepair { layer: usize, amount_attr: i32 },
    FueledLocalRepair { layer: usize, amount_attr: i32, charge_bonus_attr: Option<i32> },
    RemoteRepair { layer: usize },
    CapTransfer, Neutralizer, Nosferatu, CapBooster, Propulsion, Mining,
    Projected { jamming: JammingType }, Ecm, BurstJammer, Cloak,
    MicroJumpDrive, MicroJumpField, Salvage, Tractor, CargoScan, ShipScan, SurveyScan,
    AutoTarget, PassiveTarget, Bomb, Compression, Entosis, ProbeLaunch, Hacking, Online,
    Interdiction,
    CommandBurst { family: command_burst::CommandBurstFamily },
    BreachControl,
}

#[cfg(all(test, any(feature = "tests-combat", feature = "tests-content", feature = "tests-effects", feature = "tests-ships", feature = "tests-slash")))] pub(in crate::services) mod tests;
