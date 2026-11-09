//! Numeric classification of dogma effects for the shared effects lifecycle.
//!
//! IDs and modifier rows come from the SDE; client dogma effect constants and
//! `eve/client/script/dogma/clientDogmaLocation.py` supply behavior references;
//! pythonEffects supply corresponding behavior names. Supported families,
//! including projected effects and ECM, use the shared lifecycle and actor state;
//! unsupported families remain visibly refused.
pub(crate) mod registry;
pub(crate) mod space_source;
pub(crate) mod projection;
pub(crate) mod projected;
pub(crate) mod range;
pub(crate) mod environment;
pub(crate) mod notify;
pub(crate) mod heat;
pub(crate) mod unsupported;
pub(crate) mod propulsion;
pub(crate) mod weapons;
pub(crate) mod hacking;
pub(crate) mod lifecycle;
mod fuel;
mod entosis;
mod behaviors;
pub(crate) mod online;
pub(crate) mod fitting;
pub(crate) mod aoe;
mod bombs;
mod vorton;
pub(crate) mod cloak;
mod mjd;
mod compression;
mod tractor;
mod scan;
mod salvage;
mod targeting;
pub(crate) mod repair;
mod mining;
mod capacitor;
pub(crate) mod npc_capacitor;
pub(crate) mod npc_repair;
pub(crate) mod ecm;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum JammingType {
    Webify, Scramble, Disrupt, Paint, Damp, TrackingDisrupt, GuidanceDisrupt,
    SensorBoost, TrackingBoost,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EffectKind {
    Modifiers, Turret, Missile, Smartbomb, Vorton,
    LocalRepair { layer: usize, amount_attr: i32 },
    FueledLocalRepair { layer: usize, amount_attr: i32, charge_bonus_attr: Option<i32> },
    RemoteRepair { layer: usize },
    CapTransfer, Neutralizer, Nosferatu, CapBooster, Propulsion, Mining,
    Projected { jamming: JammingType }, Ecm, BurstJammer, Cloak,
    MicroJumpDrive, Salvage, Tractor, CargoScan, ShipScan, SurveyScan,
    AutoTarget, PassiveTarget, Bomb, Compression, Entosis, ProbeLaunch, Hacking, Online,
}

#[cfg(test)] pub(in crate::services) mod tests;
