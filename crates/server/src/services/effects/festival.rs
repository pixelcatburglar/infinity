//! Ordinary festival and display launcher admission, flight capture and launch.
//!
//! Client `shipmodulebutton.py` selects targeting from the loaded charge and
//! still calls the module's default `useMissiles` effect; `godma.py` sends
//! `dogmaIM.Activate`. This module is the only place that recognises a
//! published group-501 launcher loaded with a published group-500 charge
//! whose default effect is snowBallLaunching. The accepted activation keeps
//! effect 101 and `EffectKind::Missile`, carries a private full charge-token
//! admission, and launches through the existing projectile ball and
//! `System::launch_missile`. Direct snowBallLaunching stays refused.
//!
//! An accepted launch pays one loaded unit and inserts one native flight in
//! the same actor turn. It creates no combat attempt, damage, repair,
//! retaliation, Crimewatch, hit or miss. The terminal callback is
//! intentionally empty: the actor already owns contact, expiry and removal.
//! Structure launchers, unpublished types and melted charges are outside
//! this path. Festival rewards and client rendering are not covered here.
use super::lifecycle::{source_subject_current, Cycle, Module};
use super::parameters::MemberPacket;
use super::registry::{Effect, EffectFlag};
use super::weapons::{consume_charge_in_actor, missile::build_projectile_ball};
use crate::services::combat::{quantity_change, targeting_charges::{compatible_charge, has_target}, Ctx};
use crate::services::{notify_error, user_error, CallError};
use crate::sde::Sde;
use crate::space::combat::inputs::ChargeToken;
use crate::space::combat::loaded::LoadedPermit;
use crate::space::{System, MODE_WARP};
use infinity_core::consts::{dogma::{ATTR_EXPLOSION_DELAY, ATTR_MAX_VELOCITY, EFFECT_SNOWBALL_LAUNCHING, EFFECT_USE_MISSILES}, GROUP_FESTIVAL_MISSILE, CATEGORY_MODULE};
use infinity_core::messages;
use marshal::Value;
/// Client inventorycommon/const.py: groupMissileLauncherFestival.
pub(super) const GROUP_MISSILE_LAUNCHER_FESTIVAL: i64 = 501;

/// The admitted charge and a later token name the same resident node.
fn same_charge(left: ChargeToken, right: ChargeToken) -> bool {
    left.ship == right.ship && left.membership == right.membership && left.module == right.module
        && left.incarnation == right.incarnation && left.type_id == right.type_id
}


/// One accepted singleton, bound to the module type and the full charge token
/// admitted before aggression. A reload or replacement cannot spend through it.
#[derive(Clone, Copy)]
pub(super) struct Admission {
    module_type: i32,
    token: ChargeToken,
}

/// Published ordinary module group 501 whose default effect is useMissiles.
pub(super) fn launcher_in_scope(sde: &Sde, module_type: i32) -> bool {
    let Some(module) = sde.types.get(module_type) else { return false };
    module.published == Some(true)
        && module.category_id == Some(CATEGORY_MODULE)
        && module.group_id == Some(GROUP_MISSILE_LAUNCHER_FESTIVAL as i32)
        && module.effects().iter().any(|row| row.is_default && row.effect_id == EFFECT_USE_MISSILES)
}

/// Published ordinary group-500 charge whose default effect is snowBallLaunching
/// and whose effect metadata is neither offensive nor assistance.
fn compatible_firework(sde: &Sde, charge_type: i32) -> bool {
    let Some(charge) = sde.types.get(charge_type) else { return false };
    charge.published == Some(true)
        && charge.group_id == Some(GROUP_FESTIVAL_MISSILE)
        && charge.effects().iter().any(|row| row.is_default && row.effect_id == EFFECT_SNOWBALL_LAUNCHING)
        && sde.dogma.effect(EFFECT_SNOWBALL_LAUNCHING).is_some_and(|effect| !effect.is_offensive && !effect.is_assistance)
}

/// Admit one festival launcher inside the activation turn. A nonfestival module
/// returns `Ok(None)`. A festival candidate requires the resident module type,
/// online state and current pilot, plus a positive committed compatible charge.
/// Missing ammo is the ordinary empty-launcher error and a wrong charge is the
/// ordinary ammunition refusal.
pub(super) fn admit(s: &mut System, ship: i64, owner: i64, module: &Module, effect: &Effect) -> Result<Option<(Admission, i32)>, CallError> {
    if !launcher_in_scope(s.sde().ok_or_else(|| CallError::Internal(anyhow::anyhow!("festival admission without SDE")))?, module.type_id) {
        return Ok(None);
    }
    let empty = || user_error(messages::MSG_NO_CHARGES, vec![("launcher".into(), Value::tuple([Value::Int(super::lifecycle::UE_TYPEID), Value::Int(i64::from(module.type_id))]))]);
    if effect.id != i64::from(EFFECT_USE_MISSILES) {
        return Err(notify_error(messages::INSUFFICIENT_AMMUNITION));
    }
    if !s.ball(ship).is_some_and(|ball| ball.owner_id == owner && ball.char_id == Some(owner)) {
        return Err(notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION));
    }
    let resident = s.with_combat_fit(ship, |fit, _key, _stamp| {
        fit.source_type(crate::services::fit::Key::Module(module.id)) == Some(module.type_id)
            && fit.module_state(module.id).is_some_and(|(online, _)| online)
    }).unwrap_or(false);
    if !resident { return Err(notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION)); }
    let Some(token) = s.fitted_charge_token(ship, module.id) else { return Err(empty()); };
    let Some((_, type_id, quantity, pending)) = s.loaded_effective(ship, module.id) else { return Err(empty()); };
    let sde = s.sde().ok_or_else(|| CallError::Internal(anyhow::anyhow!("festival admission without SDE")))?;
    if type_id != token.type_id || quantity <= 0 || pending || !compatible_charge(sde, module.type_id, type_id) || !compatible_firework(sde, type_id) {
        return Err(notify_error(messages::INSUFFICIENT_AMMUNITION));
    }
    Ok(Some((Admission { module_type: module.type_id, token }, type_id)))
}

impl Admission {
    /// Confirm this activation's captured token and read the current flight
    /// values. This is an identity check, not a second classifier.
    pub(super) fn capture(self, fit: &crate::services::fit::Fit, module: &Module, token: Option<ChargeToken>) -> Result<super::parameters::FestivalInputs, CallError> {
        let Some(token) = token else { return Err(notify_error(messages::INSUFFICIENT_AMMUNITION)); };
        if module.type_id != self.module_type || !same_charge(token, self.token) {
            return Err(notify_error(messages::INSUFFICIENT_AMMUNITION));
        }
        let key = crate::services::fit::Key::Charge(module.id);
        let velocity = fit.value(key, ATTR_MAX_VELOCITY);
        let lifetime_ms = fit.value(key, ATTR_EXPLOSION_DELAY);
        if !velocity.is_finite() || velocity <= 0.0 || !lifetime_ms.is_finite() || lifetime_ms <= 0.0 {
            return Err(notify_error(messages::INSUFFICIENT_AMMUNITION));
        }
        Ok(super::parameters::FestivalInputs { charge: token.type_id, velocity, lifetime_ms })
    }
}

/// Launch one admitted shot: validate the still-current source and locked
/// target, build the real ball, pay the captured token, then insert the native
/// flight without yielding. A refusal releases the permit unpaid.
pub(super) async fn start(cx: &Ctx, cycle: &mut Cycle) -> Result<super::lifecycle::CycleOutcome, CallError> {
    let Some(admission) = cycle.effect.festival else {
        return Err(CallError::Internal(anyhow::anyhow!("festival cycle without admission")));
    };
    let module = cycle.modules.first().cloned().ok_or_else(|| CallError::Internal(anyhow::anyhow!("festival cycle without a module")))?;
    let (permit, token, flight) = {
        let member = cycle.inputs.members.first_mut().ok_or_else(|| CallError::Internal(anyhow::anyhow!("festival cycle without member inputs")))?;
        let MemberPacket::Festival(flight) = member.packet else {
            return Err(CallError::Internal(anyhow::anyhow!("festival cycle captured without festival inputs")));
        };
        let (permit, token) = member.take_charge();
        (permit, token, flight)
    };
    let target = cycle.target.unwrap_or(0);
    if !has_target(cx.char_id, target) {
        release(cx, permit);
        return Ok(super::lifecycle::target_loss(cycle).await);
    }
    let (ship, character, system, membership) = (cx.ship_id, cx.char_id, cx.system_id, cycle.source_membership);
    let generation = cycle.stop.clone();
    let presented = cycle.presented_effect;
    let flag = module.flag;
    let stopped = cycle.stop.load(std::sync::atomic::Ordering::Relaxed);
    let sde = cx.world.sde().clone();
    let launched = cx.world.with(system, move |s| launch_in_actor(
        s, &sde, ship, character, membership, &module, presented, &generation, stopped, target, admission, flight, permit, token,
    )).await.flatten();
    let Some(paid) = launched else {
        return Ok(super::lifecycle::CycleOutcome::Stop("festival launch refused"));
    };
    cx.notify("OnModuleAttributeChanges", vec![quantity_change(cx.char_id, cx.ship_id, paid.0, paid.1, paid.2, paid.2 + 1)]);
    cycle.charge_type = Some(paid.1);
    cycle.other = Some(crate::services::combat::targeting_charges::charge_key(cx.ship_id, flag, paid.1));
    Ok(super::lifecycle::CycleOutcome::Continue)
}

fn release(cx: &Ctx, permit: Option<LoadedPermit>) {
    if let Some(permit) = permit {
        cx.world.post(cx.system_id, move |s| s.release_loaded_use(permit));
    }
}

fn launch_in_actor(
    s: &mut System, sde: &Sde, ship: i64, owner: i64, membership: Option<u64>, module: &Module, presented: i64,
    generation: &std::sync::Arc<std::sync::atomic::AtomicBool>, stopped: bool, target: i64, admission: Admission, flight: super::parameters::FestivalInputs,
    permit: Option<LoadedPermit>, token: Option<ChargeToken>,
) -> Option<(i32, i32, i64)> {
    let refuse = |s: &mut System, permit: Option<LoadedPermit>| {
        if let Some(permit) = permit { s.release_loaded_use(permit); }
        None
    };
    let Some(membership) = membership else { return refuse(s, permit) };
    if stopped || generation.load(std::sync::atomic::Ordering::Relaxed) { return refuse(s, permit); }
    if !source_subject_current(s, ship, owner, membership, module, presented, generation) { return refuse(s, permit); }
    if !has_target(owner, target) { return refuse(s, permit); }
    let Some(source) = s.sampled_ball(ship).filter(|ball| ball.mode != MODE_WARP && ball.cloak == 0) else { return refuse(s, permit) };
    let Some(victim) = s.sampled_ball(target).filter(|ball| ball.mode != MODE_WARP && ball.cloak == 0) else { return refuse(s, permit) };
    if !token.is_some_and(|token| same_charge(token, admission.token)) || flight.charge != admission.token.type_id
        || !flight.velocity.is_finite() || flight.velocity <= 0.0
        || !flight.lifetime_ms.is_finite() || flight.lifetime_ms <= 0.0
        || flight.lifetime_ms.ceil() >= i64::MAX as f64
        || crate::space::damageable::DamageableProjectile::from_sde(sde, flight.charge).is_none() { return refuse(s, permit); }
    let ball = match build_projectile_ball(sde, &source, &victim, owner, Some(module.id), flight.charge, flight.velocity) {
        Ok(ball) => ball,
        Err(_) => return refuse(s, permit),
    };
    let Some(paid) = consume_charge_in_actor(s, permit, token) else { return None };
    // The actor already consumes the flight terminal. This callback has no
    // gameplay work and must not capture combat, damage or reward state.
    s.launch_missile(ball, target, ship, flight.lifetime_ms, |_s, _contact| {});
    Some(paid)
}

/// Install the accepted admission and the charge's presentation flags on the
/// resolved useMissiles descriptor. The SDE effect itself is unchanged.
pub(super) fn bind(effect: &mut Effect, sde: &Sde, admission: Admission) -> Result<(), CallError> {
    let row = sde.dogma.effect(EFFECT_SNOWBALL_LAUNCHING)
        .ok_or_else(|| CallError::Internal(anyhow::anyhow!("snowball launching effect missing")))?;
    if row.is_offensive || row.is_assistance {
        return Err(CallError::Internal(anyhow::anyhow!("snowball launching metadata is not harmless")));
    }
    effect.guid = row.guid.clone().filter(|guid| !guid.is_empty());
    effect.set_flag(EffectFlag::Offensive, false);
    effect.set_flag(EffectFlag::Assistance, false);
    effect.festival = Some(admission);
    Ok(())
}

#[cfg(all(test, feature = "tests-effects"))]
mod tests;