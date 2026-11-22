//! Master-only typed packet capture for cycle inputs.
//! Effective values come from the resident fit evaluated against the in-memory
//! SDE; the capture has no database access and no execution side effects.

use crate::services::{CallError, combat::ATTR_DAMAGE, fit::{Fit, Key}};
use crate::services::inventory::hold_storage::HoldCapacity;
use super::super::{EffectKind, lifecycle::{self, Cycle}};
use super::{capture_ecm, capacitor, mining, repair,
    CloakInputs, CompressionInputs, EntosisInputs, MasterPacket,
    ReactiveArmorInputs, SalvageInputs, ScanInputs, SuperWeaponInputs, TractorInputs};
use crate::services::fit::{reactive_armor::ARMOR_RESONANCE, PERCENT_SCALE};
use infinity_core::consts::{dogma::ATTR_ACCESS_DIFFICULTY_BONUS, dogma::ATTR_DAMAGE_DELAY_DURATION,
    dogma::ATTR_DOOMSDAY_AOE_DURATION, dogma::ATTR_DOOMSDAY_DAMAGE_CYCLE_TIME,
    dogma::ATTR_DOOMSDAY_DAMAGE_RADIUS, dogma::ATTR_DOOMSDAY_DAMAGE_DURATION, dogma::ATTR_DOOMSDAY_ENERGY_NEUT_AMOUNT,
    dogma::ATTR_DOOMSDAY_ENERGY_NEUT_RADIUS, dogma::ATTR_DOOMSDAY_ENERGY_NEUT_RESISTANCE_ID, dogma::ATTR_DOOMSDAY_ENERGY_NEUT_SIGNATURE_RADIUS,
    dogma::ATTR_DOOMSDAY_IMMOBILITY_DURATION, dogma::ATTR_DOOMSDAY_NO_JUMP_OR_CLOAK_DURATION, dogma::ATTR_DOOMSDAY_WARNING_DURATION,
    dogma::ATTR_MAX_RANGE, dogma::ATTR_MAX_TRACTOR_VELOCITY, dogma::ATTR_REACTIVATION_DELAY, dogma::ATTR_SIGNATURE_RADIUS,
    dogma::ATTR_TARGETING_DELAY, FLAG_CARGO};
use infinity_core::consts::dogma::{ATTR_RESISTANCE_SHIFT_AMOUNT, EFFECT_ADAPTIVE_ARMOR_HARDENER};

/// The captured payload of master-only families, or `None` when the kind has
/// no additional begin parameters.
pub(super) fn master_packet(fit: &Fit, cycle: &Cycle, duration_ms: f64, probes: Option<&crate::lua::content::fields_deployables::Definitions>) -> Result<MasterPacket, CallError> {
    let master = cycle.modules[0].id;
    let key = Key::Module(master);
    Ok(match cycle.effect.kind {
        EffectKind::LocalRepair { .. } | EffectKind::FueledLocalRepair { .. } | EffectKind::RemoteRepair { .. } => {
            MasterPacket::Repair(repair(fit, cycle))
        }
        EffectKind::CapTransfer | EffectKind::Neutralizer | EffectKind::Nosferatu | EffectKind::CapBooster => {
            MasterPacket::Capacitor(capacitor(fit, key, Some(Key::Charge(master)), &cycle.effect, Key::Ship))
        }
        EffectKind::Ecm | EffectKind::BurstJammer => {
            MasterPacket::Ecm(capture_ecm(fit, key, &cycle.effect, duration_ms.max(lifecycle::MIN_MODULE_DURATION_MS)))
        }
        EffectKind::Mining => MasterPacket::Mining(mining(fit, cycle)),
        EffectKind::Salvage => MasterPacket::Salvage(SalvageInputs {
            range: super::super::tractor::range(fit, &cycle.modules[0], &cycle.effect),
            bonus: fit.value(key, ATTR_ACCESS_DIFFICULTY_BONUS),
            capacity: HoldCapacity::capture(fit, FLAG_CARGO),
        }),
        EffectKind::Tractor => MasterPacket::Tractor(TractorInputs {
            range: super::super::tractor::range(fit, &cycle.modules[0], &cycle.effect),
            velocity: fit.value(key, ATTR_MAX_TRACTOR_VELOCITY) as f32,
        }),
        EffectKind::CargoScan | EffectKind::ShipScan => {
            MasterPacket::Scan(ScanInputs { range: super::super::tractor::range(fit, &cycle.modules[0], &cycle.effect) })
        }
        EffectKind::Cloak => MasterPacket::Cloak(CloakInputs {
            reactivation_ms: fit.value(key, ATTR_REACTIVATION_DELAY).max(0.0) as i64,
            targeting_ms: fit.value(key, ATTR_TARGETING_DELAY).max(0.0) as i64,
        }),
        EffectKind::SuperWeapon => MasterPacket::SuperWeapon(SuperWeaponInputs {
            warning_ms: fit.value(key, ATTR_DOOMSDAY_WARNING_DURATION),
            damage_delay_ms: fit.value(key, ATTR_DAMAGE_DELAY_DURATION),
            duration_ms: fit.value(key, ATTR_DOOMSDAY_DAMAGE_DURATION),
            cycle_ms: fit.value(key, ATTR_DOOMSDAY_DAMAGE_CYCLE_TIME),
            radius: fit.value(key, ATTR_DOOMSDAY_DAMAGE_RADIUS),
            range: fit.value(key, ATTR_MAX_RANGE),
            signature_radius: fit.value(key, ATTR_SIGNATURE_RADIUS),
            damage: ATTR_DAMAGE.map(|attr| fit.value(key, attr).max(0.0)),
            aoe_duration_ms: fit.value(key, ATTR_DOOMSDAY_AOE_DURATION),
            neut_radius: fit.value(key, ATTR_DOOMSDAY_ENERGY_NEUT_RADIUS),
            neut_amount: fit.value(key, ATTR_DOOMSDAY_ENERGY_NEUT_AMOUNT),
            neut_signature_radius: fit.value(key, ATTR_DOOMSDAY_ENERGY_NEUT_SIGNATURE_RADIUS),
            immobility_ms: fit.value(key, ATTR_DOOMSDAY_IMMOBILITY_DURATION),
            no_jump_or_cloak_ms: fit.value(key, ATTR_DOOMSDAY_NO_JUMP_OR_CLOAK_DURATION),
            resistance_id: fit.value(key, ATTR_DOOMSDAY_ENERGY_NEUT_RESISTANCE_ID) as i32,
        }),
        EffectKind::Compression => {
            MasterPacket::Compression(super::super::compression::values(fit, master).map(|(list, range, core)| CompressionInputs { list, range, core }))
        }
        EffectKind::Entosis => MasterPacket::Entosis(EntosisInputs {
            parameters: super::super::entosis::parameters(fit, &cycle.effect, master),
            would_be_active: fit.module_state(master).is_some_and(|(_, active)| active),
        }),
        EffectKind::CommandBurst { family } => MasterPacket::CommandBurst(super::super::command_burst::capture(fit, cycle, family)?),
        EffectKind::BreachControl => MasterPacket::BreachControl(super::super::breach_control::capture(fit, master, duration_ms)?),
        EffectKind::ProbeLaunch => MasterPacket::Probe(super::super::probes::capture(fit, cycle, probes)?),
        EffectKind::Hacking | EffectKind::Online => {
            return Err(CallError::Internal(anyhow::anyhow!("unsupported cycle kind {:?}", cycle.effect.kind)));
        }
        EffectKind::Interdiction => {
            MasterPacket::Field { radius: super::super::tractor::range(fit, &cycle.modules[0], &cycle.effect) }
        }
        EffectKind::Modifiers if super::super::active_cycle::tracked(cycle.effect.id) => {
            MasterPacket::ActiveCycle(super::super::active_cycle::capture(fit, master))
        }
        EffectKind::Modifiers if cycle.effect.id == i64::from(EFFECT_ADAPTIVE_ARMOR_HARDENER) => {
            // The first capture reads clean seed operands: the old
            // generation's overlay was retired before this capture ran.
            MasterPacket::ReactiveArmor(ReactiveArmorInputs {
                seed: (cycle.cycles == 0).then(|| ARMOR_RESONANCE.map(|attr| fit.modifier_value(key, attr))),
                shift: fit.value(key, ATTR_RESISTANCE_SHIFT_AMOUNT) / PERCENT_SCALE,
            })
        }
        _ => MasterPacket::None,
    })
}
