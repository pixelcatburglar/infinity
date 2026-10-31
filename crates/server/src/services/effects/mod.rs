//! Numeric classification of dogma effects for the shared effects lifecycle.
//!
//! IDs and modifier rows come from the SDE; client dogma effect constants and
//! pythonEffects supply corresponding behavior names. Projected effects use the
//! shared lifecycle and transient system-owned projections; later families are refused.
pub(crate) mod registry;
pub(crate) mod projection;
pub(crate) mod projected;
pub(crate) mod range;
pub(crate) mod environment;
pub(crate) mod notify;
pub(crate) mod unsupported;
pub(crate) mod propulsion;
pub(crate) mod weapons;
pub(crate) mod hacking;
pub(crate) mod lifecycle;
pub(crate) mod online;
pub(crate) mod fitting;
mod repair;
mod mining;
mod capacitor;
pub(crate) mod npc_capacitor;
pub(crate) mod npc_repair;
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
    AutoTarget, PassiveTarget, Bomb, ProbeLaunch, Hacking, Online,
}
