//! Superweapon lifecycle phases driven by the typed packet captured at
//! admission. After the single fuel commit the accepted firing arms its
//! source lockouts, warmup neutralization, warning visuals and
//! generation-guarded damage pulses inside the system actor; every delayed
//! hit is an actor timer carrying only captured values, never a database
//! read. The normal doomsday keeps its captured target id and membership
//! generation: a lost target suppresses the hit, never the cooldown.
use super::behaviors::Activation;
use super::lifecycle::{Cycle, CycleOutcome};
use super::parameters::{MasterPacket, SuperWeaponAim, SuperWeaponInputs};
use crate::sde::super_weapons::{Classification, GuidFamily, Mode};
use crate::services::combat::groups_health::{self, NpcCapacitorGuard};
use crate::services::combat::Ctx;
use crate::services::fit::{Fit, Key};
use crate::services::{notify_error, user_error, CallError};
use crate::sde::Sde;
use crate::space::system::super_weapons::{self, Aim, FiringSpec, SuperWeaponFxFamily};
use crate::space::{cross, dist, dot, norm, special_fx, sub, FxModule, V3, MODE_WARP};
use infinity_core::consts::dogma::{ATTR_IS_CAPITAL_SIZE, ATTR_MAX_RANGE};
use infinity_core::messages;
use marshal::Value;

fn packet(cycle: &Cycle) -> Result<SuperWeaponInputs, CallError> {
    let Some(frame) = cycle.inputs.frame.as_ref() else {
        return Err(CallError::Internal(anyhow::anyhow!("superweapon cycle inputs uncaptured")));
    };
    let MasterPacket::SuperWeapon(inputs) = &frame.packet else {
        return Err(CallError::Internal(anyhow::anyhow!("superweapon packet missing")));
    };
    Ok(*inputs)
}

/// The damage pulse window in milliseconds: the fitted doomsdayDamageDuration,
/// or the family's fitted doomsdayAOEDuration when the former is absent.
fn window_ms(inputs: &SuperWeaponInputs) -> f64 {
    if inputs.duration_ms > 0.0 { inputs.duration_ms } else { inputs.aoe_duration_ms }.max(0.0)
}

/// The shared GUID family of a catalogue row's visual.
fn guid(family: GuidFamily, race: Option<i32>) -> Option<&'static str> {
    super_weapons::super_weapon_guid(match family {
        GuidFamily::DamageBeam => SuperWeaponFxFamily::DamageBeam,
        GuidFamily::SlashBeam => SuperWeaponFxFamily::SlashBeam,
        GuidFamily::Cone => SuperWeaponFxFamily::Cone,
    }, race)
}

/// The slash sweep's unit axis from the client's two hull-anchor points; a
/// degenerate arc falls back to any perpendicular of `d0`.
fn slash_axis(d0: V3, d1: V3) -> V3 {
    let c = cross(d0, d1);
    if dot(c, c) > 1e-18 { norm(c) } else {
        let fallback = if d0[1].abs() < 0.9 { cross(d0, [0.0, 1.0, 0.0]) } else { cross(d0, [1.0, 0.0, 0.0]) };
        norm(fallback)
    }
}

pub(super) async fn validate(cx: &Ctx, fit: &Fit, activation: &Activation) -> Result<(), CallError> {
    let ship = cx.ship_id;
    let in_warp = cx.world.with(cx.system_id, move |s| {
        s.ball(ship).is_some_and(|ball| ball.mode == MODE_WARP)
    }).await.unwrap_or(true);
    if in_warp { return Err(user_error(messages::MSG_DENIED_ACTIVATE_IN_WARP, vec![])); }
    let sde = cx.world.sde().clone();
    match activation.aim {
        Some(SuperWeaponAim::Point { destination }) => {
            let Some(definition) = sde.super_weapons.get(activation.module.type_id) else {
                return Err(notify_error(messages::super_weapon::UNSUPPORTED_TYPE));
            };
            if definition.mode != Mode::Point { return Err(notify_error(messages::super_weapon::INVALID_REQUEST)); }
            point_in_range(cx, fit, activation.module.id, destination).await
        }
        Some(SuperWeaponAim::Slash { p0, p1 }) => {
            let Some(definition) = sde.super_weapons.get(activation.module.type_id) else {
                return Err(notify_error(messages::super_weapon::UNSUPPORTED_TYPE));
            };
            if definition.mode != Mode::Slash { return Err(notify_error(messages::super_weapon::INVALID_REQUEST)); }
            point_in_range(cx, fit, activation.module.id, p0).await?;
            point_in_range(cx, fit, activation.module.id, p1).await
        }
        Some(SuperWeaponAim::NormalTarget { id, .. }) => capital_target(cx, &sde, id).await,
        None => {
            let Some(target) = activation.target else {
                return Err(user_error(messages::MSG_DENIED_ACTIVATE_TARGET_NOT_PRESENT, vec![]));
            };
            capital_target(cx, &sde, target).await
        }
    }
}

/// One positional aim endpoint must sit inside the module's fitted max range
/// of the firing ship, sampled in the same actor turn.
async fn point_in_range(cx: &Ctx, fit: &Fit, module: i64, point: V3) -> Result<(), CallError> {
    let range = fit.value(Key::Module(module), ATTR_MAX_RANGE);
    let ship = cx.ship_id;
    let admitted = range.is_finite() && range > 0.0
        && cx.world.with(cx.system_id, move |s| {
            s.sampled_ball(ship).is_some_and(|ball| dist(ball.pos, point) <= range)
        }).await.unwrap_or(false);
    if !admitted { return Err(notify_error(messages::super_weapon::INVALID_REQUEST)); }
    Ok(())
}

/// Normal doomsdays accept capital hulls only, read from the target's SDE
/// `isCapitalSize` attribute (EVE University Titans: DDs target capitals).
async fn capital_target(cx: &Ctx, sde: &Sde, target: i64) -> Result<(), CallError> {
    let type_id = cx.world.with(cx.system_id, move |s| {
        s.ball(target).map(|ball| ball.type_id)
    }).await.flatten();
    let Some(type_id) = type_id else {
        return Err(user_error(messages::MSG_DENIED_ACTIVATE_TARGET_NOT_PRESENT, vec![]));
    };
    if sde.types.attribute(type_id, ATTR_IS_CAPITAL_SIZE).unwrap_or(0.0) <= 0.0 {
        return Err(notify_error(messages::super_weapon::UNSUPPORTED_TYPE));
    }
    Ok(())
}

/// The one accepted firing of this cycle: source lockouts, warmup neut and
/// the pulse window armed inside a single actor turn after the fuel commit.
pub(super) async fn start(cx: &Ctx, cycle: &mut Cycle) -> Result<CycleOutcome, CallError> {
    let inputs = packet(cycle)?;
    let aim = cycle.inputs.aim.ok_or_else(|| CallError::Internal(anyhow::anyhow!("superweapon aim missing")))?;
    let module = cycle.modules[0].clone();
    let sde = cx.world.sde().clone();
    let (ship, character, system, master, generation) =
        (cx.ship_id, cx.char_id, cx.system_id, cycle.owner.master, cycle.stop.clone());
    let (effect, weapon, illegal, cycle_duration_ms) =
        (cycle.effect.id, module.type_id, cycle.illegal, cycle.duration_ms);
    let offensive = cycle.effect.has(super::registry::EffectFlag::Offensive);
    // Single doomsdays keep their SDE effect GUID; positional families send
    // the shared directional/slash graphicInfo from the frozen aim.
    let target_guid = cycle.effect.guid.as_deref().and_then(|g| g.split(',').next()).map(str::to_owned);
    let single = matches!(aim, SuperWeaponAim::NormalTarget { .. });
    // Each pulse reserves its own combat-facts sequence on the member's
    // native generation; reserve the whole window's range up-front.
    let pulses = if single { 1u64 } else {
        (window_ms(&inputs) / inputs.cycle_ms.max(1.0)).floor() as u64 + 1
    };
    let Some(member) = cycle.owner.combat_generations.iter_mut().find(|m| m.module_id == module.id) else {
        return Err(CallError::Internal(anyhow::anyhow!("superweapon combat generation missing")));
    };
    let combat_generation = member.generation;
    let sequence = member.sequence.checked_add(1)
        .ok_or_else(|| CallError::Internal(anyhow::anyhow!("combat activation sequence exhausted")))?;
    member.sequence = sequence.checked_add(pulses.saturating_sub(1))
        .ok_or_else(|| CallError::Internal(anyhow::anyhow!("combat activation sequence exhausted")))?;
    let armed = cx.world.with(system, move |s| {
        let Some(source) = s.sampled_ball(ship) else { return false };
        // Freeze the hit shape and the firing visuals in the same turn.
        enum Visual { Target { id: i64 }, Directional { guid: &'static str, offset: V3 }, Slash { guid: &'static str, p0: V3, p1: V3 } }
        let (aim_shape, visual) = match aim {
            SuperWeaponAim::NormalTarget { id, ball_generation } => (
                Aim::Target { id, ball_generation },
                Visual::Target { id }),
            SuperWeaponAim::Point { destination } => {
                let direction = norm(sub(destination, source.pos));
                let (shape, guid) = match sde.super_weapons.get(module.type_id) {
                    Some(def) => match def.classification {
                        Classification::Cylinder => (Aim::Cylinder { origin: source.pos, direction }, guid(def.family, def.race_id)),
                        Classification::Cone => (Aim::Cone {
                            origin: source.pos, direction,
                            cos_half: (sde.super_weapons.arc_path_half_angle_proxy() / 180.0 * std::f64::consts::PI).cos(),
                        }, guid(def.family, def.race_id)),
                        _ => return false,
                    },
                    None => return false,
                };
                let Some(guid) = guid else { return false };
                (shape, Visual::Directional { guid, offset: sub(destination, source.pos) })
            }
            SuperWeaponAim::Slash { p0, p1 } => {
                let d0 = norm(sub(p0, source.pos));
                let d1 = norm(sub(p1, source.pos));
                let guid = match sde.super_weapons.get(module.type_id) {
                    Some(def) if def.classification == Classification::Slice => guid(def.family, def.race_id),
                    _ => return false,
                };
                let Some(guid) = guid else { return false };
                (Aim::Slash {
                    origin: source.pos, d0, d1,
                    axis: slash_axis(d0, d1),
                    angle: dot(d0, d1).clamp(-1.0, 1.0).acos(),
                }, Visual::Slash { guid, p0: sub(p0, source.pos), p1: sub(p1, source.pos) })
            }
        };
        let spec = FiringSpec {
            ship, character, master, weapon_type: weapon, effect,
            generation: generation.clone(), combat_generation, sequence,
            aim: aim_shape,
            hit_delay_ms: if single { inputs.damage_delay_ms.max(0.0) as i64 } else { inputs.warning_ms.max(0.0) as i64 },
            window_ms: window_ms(&inputs) as i64,
            cycle_ms: inputs.cycle_ms,
            radius: inputs.radius, range: inputs.range,
            signature_radius: inputs.signature_radius, damage: inputs.damage,
            immobility_ms: inputs.immobility_ms.max(0.0) as i64,
            no_jump_or_cloak_ms: inputs.no_jump_or_cloak_ms.max(0.0) as i64,
            single, illegal,
        };
        if !s.arm_super_weapon_firing(spec) { return false; }
        // The one warmup neutralization of this firing, through the shared
        // actor capacitor authority and only after the firing above was armed.
        if inputs.neut_amount > 0.0 && inputs.neut_radius > 0.0 {
            let guard = NpcCapacitorGuard::SuperWeapon { source: ship, character, module: master, generation: generation.clone() };
            groups_health::drain_area_in_actor(s, &sde, ship, source.pos,
                inputs.neut_radius, inputs.neut_amount, inputs.neut_signature_radius, inputs.resistance_id, &guard);
        }
        match visual {
            Visual::Target { id } => {
                if let Some(guid) = target_guid.clone() {
                    s.publish_destiny(ship, move |stamp| vec![special_fx(
                        stamp, ship, Some(FxModule { item: Some(master), type_id: weapon }), Some(id), None,
                        &guid, offensive, 1, 1, Value::Float(cycle_duration_ms), 0, None, 0, Value::None)]);
                }
            }
            Visual::Directional { guid, offset } => {
                let info = super_weapons::directional_weapon_graphic_info(offset, window_ms(&inputs));
                s.publish_destiny(ship, move |stamp| vec![special_fx(
                    stamp, ship, Some(FxModule { item: Some(master), type_id: weapon }), None, None,
                    guid, offensive, 1, 1, Value::Float(window_ms(&inputs)), 0, None, 0, info)]);
            }
            Visual::Slash { guid, p0, p1 } => {
                let info = super_weapons::slash_weapon_graphic_info(p0, p1, window_ms(&inputs));
                s.publish_destiny(ship, move |stamp| vec![special_fx(
                    stamp, ship, Some(FxModule { item: Some(master), type_id: weapon }), None, None,
                    guid, offensive, 1, 1, Value::Float(window_ms(&inputs)), 0, None, 0, info)]);
            }
        }
        true
    }).await.unwrap_or(false);
    Ok(if armed { CycleOutcome::Continue } else { CycleOutcome::Stop("source lost or activation replaced") })
}

pub(super) async fn end(_cx: &Ctx, _cycle: &mut Cycle) -> Result<CycleOutcome, CallError> {
    Ok(CycleOutcome::Continue)
}

pub(super) fn stop(s: &mut crate::space::System, _cx: &Ctx, cycle: &Cycle) {
    s.end_super_weapon_firing(cycle.owner.master, &cycle.stop);
}

#[cfg(all(test, feature = "tests-effects"))]
mod tests;
