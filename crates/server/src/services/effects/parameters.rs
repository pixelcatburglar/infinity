//! Typed per-cycle and per-source dogma parameters captured from the actor's resident fit.
//!
//! Effect families consume the same evaluator and formulas they used while
//! holding a `Fit`; these packets carry only the scalars, fixed arrays and real
//! projection operands one capture needs. No fit, key, database handle, live
//! target state, guard, attempt or authorization snapshot is stored. An
//! uncaptured `CycleInputs` holds only unallocated buffers; a consumer finding
//! it has hit a wiring error, never a successful capture. Command bursts
//! capture the module's effective warfare-buff collections, duration and
//! range into the same master packet.
use std::sync::Arc;
use crate::services::{CallError, combat::{ATTR_DAMAGE, targeting_charges::requires_loaded_charge}, fit::{Fit, Key}};
use crate::services::inventory::hold_storage::HoldCapacity;
use super::{EffectKind, lifecycle::{self, Cycle, Module}, registry::{Effect, EffectAttribute}};
use super::spool::{SpoolParameters, SpoolState};
use crate::space::combat::inputs::{ChargeToken, FitStamp};
use crate::space::combat::loaded::LoadedPermit;
use crate::space::V3;
use infinity_core::consts::{dogma::ATTR_ACCESS_DIFFICULTY_BONUS, dogma::ATTR_AOE_CLOUD_SIZE, dogma::ATTR_AOE_DAMAGE_REDUCTION_FACTOR,
    dogma::ATTR_AOE_VELOCITY, dogma::ATTR_CAPACITOR_BONUS, dogma::ATTR_CAPACITOR_CAPACITY, dogma::ATTR_CAPACITOR_NEED,
    dogma::ATTR_DAMAGE_MULTIPLIER,
    dogma::ATTR_EMP_FIELD_RANGE, dogma::ATTR_REMOTE_RESISTANCE_ID,
    dogma::ATTR_ENERGY_NEUTRALIZER_AMOUNT, dogma::ATTR_ENTITY_MISSILE_TYPE, dogma::ATTR_EXPLOSION_DELAY, dogma::ATTR_FALLOFF,
    dogma::ATTR_MAX_RANGE, dogma::ATTR_MAX_VELOCITY, dogma::ATTR_MISSILE_DAMAGE_MULTIPLIER,
    dogma::ATTR_MISSILE_ENTITY_AOE_CLOUD_SIZE_MULTIPLIER, dogma::ATTR_MISSILE_ENTITY_VELOCITY_MULTIPLIER,
    dogma::ATTR_MISSILE_ENTITY_FLIGHT_TIME_MULTIPLIER, dogma::ATTR_MISSILE_ENTITY_AOE_VELOCITY_MULTIPLIER,
    dogma::ATTR_NOS_OVERRIDE, dogma::ATTR_OPTIMAL_SIG_RADIUS, dogma::ATTR_POWER_TRANSFER_AMOUNT, dogma::ATTR_RECHARGE_RATE,
    dogma::ATTR_DAMAGE_MULTIPLIER_BONUS_PER_CYCLE, dogma::ATTR_DAMAGE_MULTIPLIER_BONUS_MAX, dogma::EFFECT_TARGET_DISINTEGRATOR_ATTACK,
    dogma::EFFECT_FOCUSED_WARP_DISRUPTION_SCRIPT, dogma::EFFECT_FOCUSED_WARP_SCRAMBLING_SCRIPT,
    dogma::ATTR_TRACKING_SPEED};

mod master;

/// Fitted optimal and falloff pair shared by typed packets.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Ranges {
    pub(crate) optimal: f64,
    pub(crate) falloff: f64,
}

/// Effect range and falloff evaluated the way every family reads them.
fn ranges(fit: &Fit, key: Key, effect: &Effect) -> Ranges {
    Ranges {
        optimal: effect.attribute(EffectAttribute::Range).map_or(0.0, |attr| fit.value(key, attr)),
        falloff: effect.attribute(EffectAttribute::Falloff).map_or(0.0, |attr| fit.value(key, attr)),
    }
}

fn resistance_attribute(fit: &Fit, key: Key, effect: &Effect) -> Option<i32> {
    effect.attribute(EffectAttribute::Resistance).or_else(|| {
        let value = fit.value(key, ATTR_REMOTE_RESISTANCE_ID);
        (value.is_finite() && value > 0.0).then_some(value as i32)
    })
}

/// A turret shot's fitted parameters and unscaled typed damage.
pub(crate) struct TurretInputs {
    pub(crate) parameters: (f64, f64, f64, f64),
    pub(crate) raw: [f64; 4],
    /// The member's raw damage is module-owned and its cycle needs no charge.
    /// False for source-neutral NPC/drone packets; only player member loops
    /// consult this charge-bypass marker.
    pub(crate) direct_damage: bool,
    /// The member's spool ramp values when the firing effect is SDE 6995
    /// (targetDisintegratorAttack); `None` for every other turret.
    pub(crate) spool: Option<SpoolParameters>,
}

/// A missile's charge, typed damage and flight values.
pub(crate) struct MissileInputs {
    pub(crate) charge: Option<i32>,
    pub(crate) raw: [f64; 4],
    pub(crate) aoe_cloud: f64,
    pub(crate) aoe_velocity: f64,
    pub(crate) drf: f64,
    pub(crate) velocity: f64,
    pub(crate) flight_time_ms: f64,
    /// Breacher pod DOT payload when the loaded charge carries dotMissileLaunching.
    pub(crate) dot: Option<super::breacher::DotPayload>,
}

/// Flight values of one admitted festival shot. No damage, application or credit fields.
#[derive(Clone, Copy)]
pub(crate) struct FestivalInputs {
    pub(crate) charge: i32,
    pub(crate) velocity: f64,
    pub(crate) lifetime_ms: f64,
}
/// A smartbomb's fitted radius and typed damage.
pub(crate) struct AreaInputs {
    pub(crate) radius: f64,
    pub(crate) raw: [f64; 4],
}

/// A bomb charge's flight and damage values.
pub(crate) struct BombInputs {
    pub(crate) charge: Option<i32>,
    pub(crate) velocity: f64,
    pub(crate) delay_ms: f64,
    pub(crate) radius: f64,
    pub(crate) raw: [f64; 4],
}

/// A Vorton's charge-raw typed damage, effective module missile-application
/// operands and fitted arc geometry. Vortons use missile damage mechanics
/// (EVE University, Vorton Projectors); no turret parameters are carried.
#[derive(Clone, Copy)]
pub(crate) struct VortonInputs {
    pub(crate) raw: [f64; 4],
    pub(crate) aoe_cloud: f64,
    pub(crate) aoe_velocity: f64,
    pub(crate) drf: f64,
    pub(crate) arc_range: f64,
    pub(crate) arc_targets: f64,
}

/// Frozen repair amounts: the uncharged and charged values the charge
/// preflight later selects between, plus the fitted ranges.
pub(crate) struct RepairInputs {
    pub(crate) base: f64,
    pub(crate) charged: f64,
    pub(crate) resistance_attribute: Option<i32>,
    pub(crate) ranges: Ranges,
}

/// Capacitor family values: ranges, the kind's transfer or bonus amount, the
/// nosferatu override flag and the source's own type for the legality check.
pub(crate) struct CapacitorInputs {
    pub(crate) ranges: Ranges,
    pub(crate) amount: f64,
    pub(crate) nos_override: bool,
    pub(crate) source_type: Option<i32>,
    pub(crate) resistance_attribute: Option<i32>,
}

/// Jammer values: sensor strengths in primary-sensor order, the selected jam
/// duration, ranges and the captured projection revisions used by source guards.
pub(crate) struct EcmInputs {
    pub(crate) source_type: Option<i32>,
    pub(crate) ranges: Ranges,
    pub(crate) strengths: [f64; 4],
    pub(crate) duration_ms: f64,
    pub(crate) projection_revision: Option<u64>,
    /// The real owner hull and its ship-domain projection revision; `None` for
    /// non-drone sources. Never the delegated controller's hull.
    pub(crate) owner_projection: Option<(i64, u64)>,
    pub(crate) resistance_attribute: Option<i32>,
}

/// Projected-effect operands kept separate for module and charge domains.
pub(crate) struct ProjectionInputs {
    pub(crate) ranges: Ranges,
    pub(crate) charge_type: Option<i32>,
    pub(crate) module_values: Vec<(i32, f64, i32)>,
    pub(crate) charge_values: Vec<(i32, f64, i32)>,
    pub(crate) resistance_attribute: Option<i32>,
}

/// Player mining values; the destination is evaluated against the target type
/// bound to the cycle and consumed at the original settlement position.
pub(crate) struct MiningInputs {
    pub(crate) range: f64,
    pub(crate) values: [f64; 5],
    pub(crate) destination: Result<Option<(i32, HoldCapacity)>, CallError>,
}

/// Drone mining yield evaluated per phase on the drone's own key.
pub(crate) struct DroneMiningInputs {
    pub(crate) values: [f64; 5],
}

/// Salvage range, access bonus and the captured cargo-hold limit; the capacity
/// result stays unevaluated until the original success settlement position.
pub(crate) struct SalvageInputs {
    pub(crate) range: f64,
    pub(crate) bonus: f64,
    pub(crate) capacity: Result<HoldCapacity, CallError>,
}

/// Tractor range and velocity.
pub(crate) struct TractorInputs {
    pub(crate) range: f64,
    pub(crate) velocity: f32,
}

/// Scanner fitted range.
pub(crate) struct ScanInputs {
    pub(crate) range: f64,
}

/// Cloak reactivation and targeting delays in milliseconds.
pub(crate) struct CloakInputs {
    pub(crate) reactivation_ms: i64,
    pub(crate) targeting_ms: i64,
}

/// Compression facility list, range and the effective industrial-core
/// requirement captured from the resident fit.
pub(crate) struct CompressionInputs {
    pub(crate) list: i64,
    pub(crate) range: f64,
    /// Effective `activationRequiresActiveIndustrialCore` (attribute 3265).
    /// A positive value requires a live industrial or compact core.
    pub(crate) core: f64,
}

/// Entosis link duration, range and the scoped would-be-active fact.
pub(crate) struct EntosisInputs {
    pub(crate) parameters: Option<(f64, f64)>,
    pub(crate) would_be_active: bool,
}

/// The duration-bounded consumer's fitted packet: the module's reactivation
/// delay in milliseconds and its effective hit points (the energizer burn).
#[derive(Clone, Copy)]
pub(crate) struct ActiveCycleInputs {
    /// Frozen moduleReactivationDelay (attribute 669) in milliseconds.
    pub(crate) delay_ms: i64,
    /// Frozen effective module hit points read at cycle capture.
    pub(crate) hp: f64,
}

/// The adaptive armor hardener's captured packet: the clean first-cycle
/// operand seed and the effective per-cycle resistance shift fraction.
/// Repeats re-evaluate the shift but never re-seed.
#[derive(Clone, Copy)]
pub(crate) struct ReactiveArmorInputs {
    /// The module's four raw armor-resonance operands captured with the old
    /// generation's overlay already removed; `None` on every repeat capture.
    pub(crate) seed: Option<[f64; 4]>,
    /// Effective resistanceShiftAmount (SDE 1849) as a whole fraction.
    pub(crate) shift: f64,
}

/// The per-member closed packet of families that execute every fitted member.
pub(crate) enum MemberPacket {
    Turret(TurretInputs),
    Missile(MissileInputs),
    Festival(FestivalInputs),
    Smartbomb(AreaInputs),
    Bomb(BombInputs),
    Vorton(VortonInputs),
    Projected(Arc<ProjectionInputs>),
    /// A master-only family or a kind with no captured member values.
    None,
}

/// A member's resolved volley outcome carried out of the actor turn in the
/// same slot the attempt arrived in; one scalar report and loss receipt, no
/// separate report collection.
pub(crate) struct MemberShot {
    /// The rolled hit multiplier (0 is a genuine miss).
    pub(crate) multiplier: f64,
    /// Damage the application committed (0 on a miss or refused application).
    pub(crate) dealt: f64,
    /// A ship-loss settlement receipt to await outside the actor.
    pub(crate) receipt: Option<tokio::sync::oneshot::Receiver<Result<(), CallError>>>,
    /// The shooter pilot and victim pilot the report names.
    pub(crate) shooter: Option<i64>,
    pub(crate) victim: Option<i64>,
}

/// One turret bank member's per-round state inside the reused member buffer:
/// the charge this round consumed (`None` for direct-damage members), the
/// member's admitted attempt and the volley outcome the actor turn produced.
/// `None` is no candidate or a cleared slot; the next capture refuses a slot
/// that is still armed rather than overwriting it.
pub(crate) struct TurretRound {
    pub(crate) charge: Option<i32>,
    /// Master-attributed for a normal bank volley, self-attributed for a
    /// mismatch fallback; the replay owner is always the real member.
    pub(crate) attempt: Option<super::space_source::PreparedCombatAttempt>,
    /// The resolved shot waiting to be reported, when the member's attempt
    /// produced one; refused or canceled rounds leave it `None`.
    pub(crate) shot: Option<MemberShot>,
    /// This round's armed member ramp bonus captured inside the final actor
    /// turn; only a really fired volley advances the member's stored state and
    /// writes `MemberInputs::spool_applied`. A rejected round drops it with
    /// the slot — it is not a second permanent spool.
    pub(crate) spool_bonus: Option<f64>,
}

/// One member of the captured cycle, in `Cycle::modules` order.
pub(crate) struct MemberInputs {
    pub(crate) module: i64,
    /// The fitted charge type for this member's consume preflight.
    pub(crate) charge_type: Option<i32>,
    /// The resident charge identity captured in the same gate; a committed
    /// final consumption removes exactly this node through the actor.
    pub(crate) charge_token: Option<ChargeToken>,
    /// The member's admitted finite loaded use for this round, bound to the
    /// slot incarnation the same capture observed; the consume pays it and
    /// the stop path releases it unpaid.
    pub(crate) permit: Option<LoadedPermit>,
    /// Rack index, absorption and heat damage for members in a rack slot.
    pub(crate) heat: Option<(usize, f64, f64)>,
    /// The member's armed turret-bank round, set after capture by the bank
    /// executor and cleared before the next capture or at stop.
    pub(crate) turret_round: Option<TurretRound>,
    /// The member's bound spool ramp, living in the reused member buffer for
    /// the life of the activation; `None` for non-6995 turrets.
    pub(crate) spool: Option<SpoolState>,
    /// The ramp bonus this member's last accepted shot applied; rewritten to
    /// `None` at every capture and set only by a really fired shot.
    pub(crate) spool_applied: Option<f64>,
    pub(crate) packet: MemberPacket,
}

impl MemberInputs {
    /// Take this round's admitted finite use and resident charge identity for
    /// the consume; a member without either is an ordinary refusal.
    pub(crate) fn take_charge(&mut self) -> (Option<LoadedPermit>, Option<ChargeToken>) {
        (self.permit.take(), self.charge_token)
    }
}

/// Frozen activation aim carried through every cycle-input buffer reuse.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum SuperWeaponAim {
    NormalTarget { id: i64, ball_generation: u64 },
    Point { destination: V3 },
    Slash { p0: V3, p1: V3 },
}

/// Fitted values consumed by the superweapon actor phases.
#[derive(Clone, Copy, Debug)]
pub(crate) struct SuperWeaponInputs {
    /// `doomsdayWarningDuration`: time before the positional pulse window.
    pub(crate) warning_ms: f64,
    /// `damageDelayDuration`: the normal-target hit delay.
    pub(crate) damage_delay_ms: f64,
    /// `doomsdayDamageDuration`: the pulse window's own length.
    pub(crate) duration_ms: f64,
    /// `doomsdayDamageCycleTime`: pulse cadence.
    pub(crate) cycle_ms: f64,
    /// `doomsdayDamageRadius`: beam half-width and slash beam thickness.
    pub(crate) radius: f64,
    /// `maxRange`: beam/cone reach and slash sweep extent.
    pub(crate) range: f64,
    /// `signatureRadius`: the weapon signature for the scaling factor.
    pub(crate) signature_radius: f64,
    pub(crate) damage: [f64; 4],
    /// `doomsdayAOEDuration`: fallback pulse window when `duration_ms` is unset.
    pub(crate) aoe_duration_ms: f64,
    /// `doomsdayEnergyNeutRadius`/`Amount`/`SignatureRadius`: warmup neut.
    pub(crate) neut_radius: f64,
    pub(crate) neut_amount: f64,
    pub(crate) neut_signature_radius: f64,
    /// `doomsdayImmobilityDuration` / `doomsdayNoJumpOrCloakDuration`: the
    /// firing ship's lockout durations armed once at accepted firing.
    pub(crate) immobility_ms: f64,
    pub(crate) no_jump_or_cloak_ms: f64,
    /// `doomsdayEnergyNeutResistanceID`: the victim resistance attribute id.
    pub(crate) resistance_id: i32,
}

/// The single closed packet of master-only families.
pub(crate) enum MasterPacket {
    Repair(RepairInputs),
    Capacitor(CapacitorInputs),
    Ecm(EcmInputs),
    Mining(MiningInputs),
    Salvage(SalvageInputs),
    Tractor(TractorInputs),
    Scan(ScanInputs),
    Cloak(CloakInputs),
    SuperWeapon(SuperWeaponInputs),
    /// Compression values stay a `Result` so attribute validation keeps its
    /// original position inside the family start.
    Compression(Result<CompressionInputs, CallError>),
    Entosis(EntosisInputs),
    /// Duration-bounded pure-modifier consumer values.
    ActiveCycle(ActiveCycleInputs),
    CommandBurst(super::command_burst::CommandBurstInputs),
    BreachControl(super::breach_control::BreachControlInputs),
    /// Adaptive armor hardener cycle inputs.
    ReactiveArmor(ReactiveArmorInputs),
    /// The unscripted warp-disruption sphere's fitted field radius, consumed
    /// once when the field is raised.
    Field { radius: f64 },
    /// Frozen interdiction-probe payload captured from the pinned definitions.
    Probe(super::probes::ProbeInputs),
    /// A family with no extra captured values or a member-packet family.
    None,
}

/// Values shared by every member of one capture.
pub(crate) struct CycleFrame {
    /// Raw fitted effect duration; the lifecycle applies its minimum at use.
    pub(crate) duration_ms: f64,
    /// Effect discharge attribute summed over the members in their order.
    pub(crate) discharge: f64,
    /// Ship capacitor capacity and recharge rate for the drain consumer.
    pub(crate) capacitor: (f64, f64),
    /// The master's fitted charge type for the charge-sensitive preflight.
    pub(crate) master_charge_type: Option<i32>,
    /// Low, medium and high rack capacity, dissipation, attenuation and
    /// generation multiplier, evaluated with the original rack formula.
    pub(crate) heat_racks: [(f64, f64, f64, f64); 3],
    pub(crate) packet: MasterPacket,
}

/// One capture's frame, member buffer, fuel aggregation and family packet.
///
/// `frame: None` is the explicit uncaptured state used for the first fill and
/// for move-out buffer reuse; it carries no forged stamp, damage or range.
pub(crate) struct CycleInputs {
    pub(crate) frame: Option<CycleFrame>,
    pub(crate) members: Vec<MemberInputs>,
    /// Positive fuel requirements aggregated by type, or the original
    /// validation failure consumed at the fuel stage.
    pub(crate) fuel: Result<Vec<(i32, i64)>, CallError>,
    pub(crate) aim: Option<SuperWeaponAim>,
    /// Immutable probe payload definitions pinned at activation. `None` for
    /// every other family and for a reused buffer that has not been filled.
    pub(crate) probes: Option<std::sync::Arc<crate::lua::content::fields_deployables::Definitions>>,
}

impl Default for CycleInputs {
    fn default() -> Self {
        Self { frame: None, members: Vec::new(), fuel: Ok(Vec::new()), aim: None, probes: None }
    }
}

/// The closed set of inputs an actor-owned source phase consumes.
pub(crate) enum SourceInputs {
    Turret(TurretInputs),
    Missile(MissileInputs),
    Vorton(VortonInputs),
    Projected(Arc<ProjectionInputs>),
    Ecm(EcmInputs),
    Capacitor(CapacitorInputs),
    RemoteRepair(RepairInputs),
    DroneMining(DroneMiningInputs),
    /// Salvage drone inputs: the effect range attribute and the drone's own
    /// access difficulty bonus, both read from the drone's key.
    DroneSalvage { range: f64, bonus: f64 },
}

/// A fitted weapon that fires without a loaded charge: an active Turret
/// effect on a module type declaring no charge group or size. Activation
/// admission and member capture share this one classification; NPC and drone
/// source captures keep their own damage path and never consult it.
pub(super) fn direct_damage_weapon(fit: &Fit, module: &Module, effect: &Effect) -> bool {
    effect.kind == EffectKind::Turret && !requires_loaded_charge(fit.sde(), module.type_id)
}

/// Module turret parameters via the shared evaluator helper; `direct_damage`
/// selects the module itself as the raw-damage node for a chargeless turret.
/// The spool ramp values are captured only for SDE 6995 so the ordinary
/// attribute default can never arm an unrelated gun.
fn module_turret(fit: &Fit, module: &Module, effect: &Effect, direct_damage: bool) -> TurretInputs {
    let (parameters, raw) = super::weapons::turret_parameters(fit, module, direct_damage);
    let key = Key::Module(module.id);
    let spool = (effect.id == i64::from(EFFECT_TARGET_DISINTEGRATOR_ATTACK))
        .then(|| SpoolParameters {
            step: fit.value(key, ATTR_DAMAGE_MULTIPLIER_BONUS_PER_CYCLE),
            maximum: fit.value(key, ATTR_DAMAGE_MULTIPLIER_BONUS_MAX),
        });
    TurretInputs { parameters, raw, direct_damage, spool }
}

/// Source turret parameters: the source's own key damage times its multiplier.
fn source_turret(fit: &Fit, key: Key) -> TurretInputs {
    TurretInputs {
        parameters: (fit.value(key, ATTR_MAX_RANGE), fit.value(key, ATTR_FALLOFF),
            fit.value(key, ATTR_TRACKING_SPEED), fit.value(key, ATTR_OPTIMAL_SIG_RADIUS)),
        raw: ATTR_DAMAGE.map(|id| fit.value(key, id) * fit.value(key, ATTR_DAMAGE_MULTIPLIER).max(0.0)),
        direct_damage: false,
        spool: None,
    }
}

/// Player missile values: charge attributes times the character multiplier.
fn module_missile(fit: &Fit, module: &Module) -> MissileInputs {
    let key = Key::Charge(module.id);
    MissileInputs {
        charge: fit.source_type(key),
        raw: ATTR_DAMAGE.map(|id| fit.value(key, id) * fit.value(Key::Char, ATTR_MISSILE_DAMAGE_MULTIPLIER).max(0.0)),
        aoe_cloud: fit.value(key, ATTR_AOE_CLOUD_SIZE),
        aoe_velocity: fit.value(key, ATTR_AOE_VELOCITY),
        drf: fit.value(key, ATTR_AOE_DAMAGE_REDUCTION_FACTOR),
        velocity: fit.value(key, ATTR_MAX_VELOCITY).max(1.0),
        flight_time_ms: fit.value(key, ATTR_EXPLOSION_DELAY),
        dot: super::breacher::capture_dot(fit, module),
    }
}

/// Entity missile values: SDE charge attributes times the entity multipliers.
/// An unknown entity charge stays `None`; the fire phase keeps its refusal.
fn entity_missile(fit: &Fit, key: Key) -> MissileInputs {
    let sde = fit.sde();
    let raw_charge = fit.value(key, ATTR_ENTITY_MISSILE_TYPE) as i32;
    let charge = (raw_charge > 0 && sde.types.get(raw_charge).is_some()).then_some(raw_charge);
    let attr = |id| sde.types.attribute(raw_charge, id).or_else(|| sde.dogma.attribute(id).and_then(|a| a.default_value)).unwrap_or(0.0);
    MissileInputs {
        charge,
        raw: ATTR_DAMAGE.map(|id| attr(id) * fit.value(key, ATTR_MISSILE_DAMAGE_MULTIPLIER)),
        aoe_cloud: attr(ATTR_AOE_CLOUD_SIZE) * fit.value(key, ATTR_MISSILE_ENTITY_AOE_CLOUD_SIZE_MULTIPLIER),
        aoe_velocity: attr(ATTR_AOE_VELOCITY) * fit.value(key, ATTR_MISSILE_ENTITY_AOE_VELOCITY_MULTIPLIER),
        drf: attr(ATTR_AOE_DAMAGE_REDUCTION_FACTOR),
        velocity: (attr(ATTR_MAX_VELOCITY) * fit.value(key, ATTR_MISSILE_ENTITY_VELOCITY_MULTIPLIER)).max(1.0),
        flight_time_ms: attr(ATTR_EXPLOSION_DELAY) * fit.value(key, ATTR_MISSILE_ENTITY_FLIGHT_TIME_MULTIPLIER),
        dot: None,
    }
}

fn area(fit: &Fit, key: Key) -> AreaInputs {
    AreaInputs { radius: fit.value(key, ATTR_EMP_FIELD_RANGE), raw: ATTR_DAMAGE.map(|attr| fit.value(key, attr).max(0.0)) }
}

fn bomb(fit: &Fit, module: &Module) -> BombInputs {
    let key = Key::Charge(module.id);
    BombInputs {
        charge: fit.source_type(key),
        velocity: fit.value(key, ATTR_MAX_VELOCITY),
        delay_ms: fit.value(key, ATTR_EXPLOSION_DELAY),
        radius: fit.value(key, ATTR_EMP_FIELD_RANGE),
        raw: ATTR_DAMAGE.map(|attr| fit.value(key, attr).max(0.0)),
    }
}

/// Shared Vorton operand capture: raw typed damage on `damage_key` times the
/// effective multiplier on `module_key`, the module's effective missile
/// operands and its fitted arc geometry.
fn vorton_values(fit: &Fit, module_key: Key, damage_key: Key) -> VortonInputs {
    VortonInputs {
        raw: ATTR_DAMAGE.map(|id| fit.value(damage_key, id) * fit.value(module_key, ATTR_DAMAGE_MULTIPLIER).max(0.0)),
        aoe_cloud: fit.value(module_key, ATTR_AOE_CLOUD_SIZE),
        aoe_velocity: fit.value(module_key, ATTR_AOE_VELOCITY),
        drf: fit.value(module_key, ATTR_AOE_DAMAGE_REDUCTION_FACTOR),
        arc_range: fit.value(module_key, super::vorton::ATTR_VORTON_ARC_RANGE),
        arc_targets: fit.value(module_key, super::vorton::ATTR_VORTON_ARC_TARGETS),
    }
}

/// Player member capture: the module node's effective operands (its resident
/// charge topology already contributes charge modifiers) and the charge
/// node's raw typed damage.
fn vorton(fit: &Fit, module: &Module) -> VortonInputs {
    vorton_values(fit, Key::Module(module.id), Key::Charge(module.id))
}

/// Entity hull capture: the NPC hull supplies every operand at the source key,
/// so the module and damage keys are the same ship node and there is no charge.
fn vorton_source(fit: &Fit, key: Key) -> VortonInputs {
    vorton_values(fit, key, key)
}

fn repair(fit: &Fit, cycle: &Cycle) -> RepairInputs {
    let (amount_attr, bonus_attr) = match lifecycle::charge_kind(cycle.effect.id, cycle.effect.kind) {
        EffectKind::LocalRepair { amount_attr, .. } => (amount_attr, None),
        EffectKind::FueledLocalRepair { amount_attr, charge_bonus_attr, .. } => (amount_attr, charge_bonus_attr),
        EffectKind::RemoteRepair { layer } => (super::repair::amount_attribute(layer), None),
        _ => unreachable!("not a repair effect kind"),
    };
    let master = cycle.modules[0].id;
    RepairInputs {
        base: super::repair::amount(fit, master, amount_attr),
        charged: super::repair::fueled(fit, master, amount_attr, bonus_attr, true),
        resistance_attribute: resistance_attribute(fit, Key::Module(master), &cycle.effect),
        ranges: ranges(fit, Key::Module(master), &cycle.effect),
    }
}

/// The amount attribute each capacitor kind actually consumes.
fn capacitor(fit: &Fit, key: Key, charge_key: Option<Key>, effect: &Effect, source_key: Key) -> CapacitorInputs {
    let amount = match effect.kind {
        EffectKind::Neutralizer => fit.value(key, ATTR_ENERGY_NEUTRALIZER_AMOUNT),
        EffectKind::CapBooster => charge_key.map_or(0.0, |key| fit.value(key, ATTR_CAPACITOR_BONUS)),
        _ => fit.value(key, ATTR_POWER_TRANSFER_AMOUNT),
    };
    CapacitorInputs {
        ranges: ranges(fit, key, effect),
        amount,
        nos_override: fit.value(key, ATTR_NOS_OVERRIDE) > 0.0,
        source_type: fit.source_type(source_key),
        resistance_attribute: resistance_attribute(fit, key, effect),
    }
}

pub(super) fn capture_ecm(fit: &Fit, key: Key, effect: &Effect, duration_ms: f64) -> EcmInputs {
    let drone = matches!(key, Key::Drone(_));
    let mut ranges = ranges(fit, key, effect);
    if matches!(effect.kind, EffectKind::BurstJammer) {
        ranges.optimal = fit.value(key, super::ecm::BURST_RANGE);
    }
    EcmInputs {
        source_type: fit.source_type(if drone { key } else { Key::Ship }),
        ranges,
        strengths: super::ecm::JAM_STRENGTH.map(|attr| fit.value(key, attr)),
        duration_ms: super::ecm::selected_duration(fit, key, duration_ms),
        projection_revision: if drone { fit.drone_projection_revision() } else { fit.projection_revision() },
        owner_projection: if drone { fit.ship_id().zip(fit.projection_revision()) } else { None },
        resistance_attribute: resistance_attribute(fit, key, effect),
    }
}

fn fill_projection(fit: &Fit, key: Key, effect: &Effect, charge_type: Option<i32>, inputs: &mut ProjectionInputs) {
    inputs.ranges = ranges(fit, key, effect);
    // A charge type that itself declares the running effect, like a focused
    // warp script, already contributes its targetID rows in the module
    // domain; snapshotting them again with the charge as source applies them
    // twice and leaves the receiver an extra charge operand set.
    let charge_type = charge_type.filter(|ty| !super::lifecycle::has_effect(fit.sde().types.effects(*ty), effect.id as i32));
    inputs.charge_type = charge_type;
    super::projected::snapshot(fit, key, effect, charge_type, &mut inputs.module_values, &mut inputs.charge_values);
}

fn projection(fit: &Fit, key: Key, effect: &Effect, charge_type: Option<i32>) -> ProjectionInputs {
    let mut inputs = ProjectionInputs {
        ranges: Ranges { optimal: 0.0, falloff: 0.0 },
        charge_type: None,
        module_values: Vec::new(),
        charge_values: Vec::new(),
        resistance_attribute: resistance_attribute(fit, key, effect),
    };
    fill_projection(fit, key, effect, charge_type, &mut inputs);
    inputs
}

fn mining(fit: &Fit, cycle: &Cycle) -> MiningInputs {
    let master = Key::Module(cycle.modules[0].id);
    MiningInputs {
        range: fit.value(master, ATTR_MAX_RANGE),
        values: crate::services::mining::yield_values(fit, master),
        destination: match cycle.target_type {
            Some(type_id) => crate::services::mining::destination(fit, type_id).map(|capacity| Some((type_id, capacity))),
            None => Ok(None),
        },
    }
}

/// The captured payload of one cycle member; `existing` keeps the projection
/// operand buffers of a repeating cycle. `token` is the charge identity this
/// same capture assigned to the member.
fn member_packet(fit: &Fit, cycle: &Cycle, module: &Module, token: Option<ChargeToken>, existing: MemberPacket) -> Result<MemberPacket, CallError> {
    let key = Key::Module(module.id);
    Ok(match cycle.effect.kind {
        EffectKind::Turret => MemberPacket::Turret(module_turret(fit, module, &cycle.effect, direct_damage_weapon(fit, module, &cycle.effect))),
        EffectKind::Missile if cycle.effect.festival.is_some() || super::festival::launcher_in_scope(fit.sde(), module.type_id) => {
            let admission = cycle.effect.festival.ok_or_else(|| CallError::Internal(anyhow::anyhow!("festival launcher captured without admission")))?;
            let flight = admission.capture(fit, module, token)?;
            MemberPacket::Festival(flight)
        }
        EffectKind::Missile => MemberPacket::Missile(module_missile(fit, module)),
        EffectKind::Smartbomb => MemberPacket::Smartbomb(area(fit, key)),
        EffectKind::Bomb => MemberPacket::Bomb(bomb(fit, module)),
        EffectKind::Vorton => MemberPacket::Vorton(vorton(fit, module)),
        EffectKind::Projected { .. } => {
            let charge_type = fit.source_type(Key::Charge(module.id));
            let mut inputs = match existing {
                MemberPacket::Projected(inputs) => inputs,
                _ => Arc::new(ProjectionInputs {
                    ranges: ranges(fit, key, &cycle.effect),
                    charge_type: None,
                    module_values: Vec::new(),
                    charge_values: Vec::new(),
                    resistance_attribute: resistance_attribute(fit, key, &cycle.effect),
                }),
            };
            // The buffer is uniquely owned at capture: a cloned handle kept
            // elsewhere is a holder-lifetime bug, not a reason to copy operands.
            let Some(inner) = Arc::get_mut(&mut inputs) else {
                return Err(CallError::Internal(anyhow::anyhow!("projection operand buffer is not uniquely owned")));
            };
            fill_projection(fit, key, &cycle.effect, charge_type, inner);
            MemberPacket::Projected(inputs)
        }
        _ => MemberPacket::None,
    })
}


/// Capture one cycle's real inputs from the resident fit, reusing the prior
/// buffer's member and operand capacity.
pub(super) fn capture_cycle(fit: &Fit, cycle: &Cycle, stamp: FitStamp, mut reuse: CycleInputs) -> Result<CycleInputs, CallError> {
    let master = cycle.modules[0].id;
    let duration_ms = cycle.effect.attribute(EffectAttribute::Duration)
        .map(|attr| fit.value(Key::Module(master), attr)).unwrap_or(cycle.duration_ms);
    let discharge = {
        let attr = cycle.effect.attribute(EffectAttribute::Discharge).unwrap_or(ATTR_CAPACITOR_NEED);
        cycle.modules.iter().map(|module| {
            let key = Key::Module(module.id);
            // Client dogma/effects/environment.py:65-69: a focused script runs
            // its own effect, whose discharge attribute the module type does
            // not declare; the cycle cost then falls back to the module's
            // capacitorNeed, already reduced by the script's otherID rows.
            // Explicit type or override values, zero included, are honoured.
            let attr = if matches!(cycle.effect.id as i32, EFFECT_FOCUSED_WARP_SCRAMBLING_SCRIPT | EFFECT_FOCUSED_WARP_DISRUPTION_SCRIPT)
                && !fit.type_has_attribute(key, attr)
                && !fit.overrides(key).iter().any(|(attribute, _)| *attribute == attr) {
                ATTR_CAPACITOR_NEED
            } else {
                attr
            };
            fit.value(key, attr)
        }).sum()
    };
    let fuel = super::fuel::requirements(fit, &cycle.modules, std::mem::replace(&mut reuse.fuel, Ok(Vec::new())).unwrap_or_default());
    reuse.members.truncate(cycle.modules.len());
    while reuse.members.len() < cycle.modules.len() {
        reuse.members.push(MemberInputs { module: 0, charge_type: None, charge_token: None, permit: None, heat: None, turret_round: None, spool: None, spool_applied: None, packet: MemberPacket::None });
    }
    for (member, module) in reuse.members.iter_mut().zip(&cycle.modules) {
        if member.turret_round.is_some() {
            // An armed round token must be finished by the bank executor or
            // the stop path before this capture; silently overwriting it
            // would leak the replay reservation.
            return Err(CallError::Internal(anyhow::anyhow!("turret bank round slot retained across capture")));
        }
        let key = Key::Module(module.id);
        member.module = module.id;
        member.spool_applied = None;
        member.charge_token = fit.charge_incarnation(module.id)
            .map(|(incarnation, type_id)| ChargeToken { ship: cycle.cx.ship_id, membership: stamp.membership, module: module.id, incarnation, type_id });
        member.charge_type = member.charge_token.map(|token| token.type_id);
        member.heat = super::heat::rack_index(module.flag)
            .map(|rack| (rack, fit.value(key, super::heat::ABSORPTION), fit.value(key, super::heat::HEAT_DAMAGE)));
        member.packet = member_packet(fit, cycle, module, member.charge_token, std::mem::replace(&mut member.packet, MemberPacket::None))?;
    }
    reuse.frame = Some(CycleFrame {
        duration_ms,
        discharge,
        capacitor: (fit.value(Key::Ship, ATTR_CAPACITOR_CAPACITY), fit.value(Key::Ship, ATTR_RECHARGE_RATE)),
        master_charge_type: reuse.members[0].charge_token.map(|token| token.type_id),
        heat_racks: [0, 1, 2].map(|rack| (
            fit.value(Key::Ship, super::heat::CAPACITY[rack]),
            fit.value(Key::Ship, super::heat::DISSIPATION[rack]),
            fit.value(Key::Ship, super::heat::ATTENUATION[rack]),
            fit.value(Key::Ship, super::heat::GENERATION),
        )),
        packet: master::master_packet(fit, cycle, duration_ms, reuse.probes.as_deref())?,
    });
    reuse.fuel = fuel;
    reuse.probes = cycle.inputs.probes.clone();
    Ok(reuse)
}

/// Capture one in-space source phase's real inputs from the fit the gate
/// supplies: the drone's own domain, an owner hull, or the NPC's in-memory fit.
pub(crate) fn capture_source(fit: &Fit, key: Key, effect: &Effect, duration_ms: f64) -> Result<SourceInputs, CallError> {
    Ok(match effect.kind {
        EffectKind::Turret => SourceInputs::Turret(source_turret(fit, key)),
        EffectKind::Missile => SourceInputs::Missile(entity_missile(fit, key)),
        EffectKind::Vorton => SourceInputs::Vorton(vorton_source(fit, key)),
        EffectKind::Projected { .. } => SourceInputs::Projected(Arc::new(projection(fit, key, effect, None))),
        EffectKind::Ecm => SourceInputs::Ecm(capture_ecm(fit, key, effect, duration_ms)),
        EffectKind::Neutralizer | EffectKind::Nosferatu | EffectKind::CapTransfer => {
            SourceInputs::Capacitor(capacitor(fit, key, None, effect, key))
        }
        EffectKind::RemoteRepair { layer } => {
            let base = fit.value(key, super::repair::amount_attribute(layer));
            SourceInputs::RemoteRepair(RepairInputs {
                base,
                charged: base,
                resistance_attribute: resistance_attribute(fit, key, effect),
                ranges: ranges(fit, key, effect),
            })
        }
        EffectKind::Mining => SourceInputs::DroneMining(DroneMiningInputs { values: crate::services::mining::yield_values(fit, key) }),
        EffectKind::Salvage => SourceInputs::DroneSalvage {
            range: fit.value(key, effect.attribute(EffectAttribute::Range).unwrap_or(ATTR_MAX_RANGE)),
            bonus: fit.value(key, ATTR_ACCESS_DIFFICULTY_BONUS),
        },
        _ => return Err(CallError::Internal(anyhow::anyhow!("unsupported source effect kind {:?}", effect.kind))),
    })
}

#[cfg(all(test, feature = "tests-effects"))]
mod tests;
