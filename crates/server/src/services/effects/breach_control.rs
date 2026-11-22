//! Breach Control: the breacher pod ship's disposable damage control (SDE
//! effect `moduleBonusBreacherPodDamageControl`, 12916). Mirrors the client's
//! `eve/common/script/dogma/pythonEffects/breacherPodDamageControl.py`: one
//! accepted activation burns the module to its effective hit points and
//! offlines it in the same transaction, then lands the module's
//! `breacherPodActivatedDamageReceivedPercentage` as a timed runtime source on
//! the ship's dbuff collection 2600 for the effective duration.
//! Effective parameters come from the actor-resident `Fit`; condition and
//! online state persist through `game.items` and `game.item_attributes`. The
//! landed source is actor-transient: it expires on its own deadline, leaves
//! with the receiver and never survives a restart.
//! The shared heat-damage authority stays energizer-only and the overheat
//! batch keeps sharing one before-fit across modules; this one-module burn,
//! offline and fitting boundary composes the shared DAO, online and fitting
//! primitives instead, without copying the batch transaction or changing the
//! energizer's never-offline contract.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use infinity_core::consts::{CATEGORY_MODULE, CATEGORY_SHIP,
    dogma::{ATTR_BREACHER_POD_ACTIVATED_DAMAGE_RECEIVED_PERCENTAGE, ATTR_HULL_DAMAGE, ATTR_STRUCTURE_HP,
        DBUFF_BPDC_DAMAGE_RECEIVED_PERCENTAGE, EFFECT_MODULE_BONUS_BREACHER_POD_DAMAGE_CONTROL}};
use marshal::Value;
use tracing::warn;
use crate::dao::items::{dogma, loading::ItemQuery};
use crate::db::outbox::Outbox;
use crate::services::{CallError, combat::{self, Ctx}, db_err, fit::{Fit, Key}, items::load_items, market};
use crate::services::fit::runtime::{BREACH_CONTROL_NAMESPACE, RuntimeReceiverKey, RuntimeSourceKey};
use crate::space::dbuff::TimedRuntimeInput;
use super::lifecycle::{Cycle, CycleOutcome};
use super::online::{self, OnlineChanged, OnlinePolicy};
use super::parameters::MasterPacket;

/// One activation's frozen inputs captured from the resident fit: the
/// effective duration, the received-damage percentage the landed source
/// carries and the module's effective hit points and hull damage.
#[derive(Clone, Copy)]
pub(crate) struct BreachControlInputs {
    pub(crate) duration_ms: f64,
    pub(crate) received_percentage: f64,
    pub(crate) hp: f64,
    pub(crate) damage: f64,
}

fn stale() -> CallError { CallError::Internal(anyhow::anyhow!("breach control context changed")) }

/// Read the burn and dbuff operands from the resident fit's module node.
/// `duration_ms` is the effective cycle duration the caller already resolved;
/// a non-finite value, a sub-millisecond or unrepresentable duration, a
/// non-positive hit-point budget or an already-exhausted module refuses the
/// capture. A negative received percentage is legitimate.
pub(super) fn capture(fit: &Fit, module: i64, duration_ms: f64) -> Result<BreachControlInputs, CallError> {
    if !duration_ms.is_finite() || duration_ms < 1.0 || duration_ms >= i64::MAX as f64 {
        return Err(CallError::Internal(anyhow::anyhow!("breach control duration invalid")));
    }
    let key = Key::Module(module);
    let inputs = BreachControlInputs {
        duration_ms,
        received_percentage: fit.value(key, ATTR_BREACHER_POD_ACTIVATED_DAMAGE_RECEIVED_PERCENTAGE),
        hp: fit.value(key, ATTR_STRUCTURE_HP),
        damage: fit.value(key, ATTR_HULL_DAMAGE),
    };
    if !inputs.received_percentage.is_finite() || !inputs.hp.is_finite() || inputs.hp <= 0.0
        || !inputs.damage.is_finite() || inputs.damage >= inputs.hp {
        return Err(CallError::Internal(anyhow::anyhow!("breach control inputs invalid")));
    }
    Ok(inputs)
}

/// Run the one accepted activation: burn the module to its captured hit
/// points, offline it and publish the fitting boundary in one transaction,
/// then land the damage-received collection on the ship after commit. A
/// committed burn never rolls back: a missing system, a replaced ship
/// membership or a different pilot only skips the landing with a warning.
pub(super) async fn start(cx: &Ctx, cycle: &mut Cycle) -> Result<CycleOutcome, CallError> {
    let Some(MasterPacket::BreachControl(inputs)) = cycle.inputs.frame.as_ref().map(|frame| &frame.packet) else {
        return Err(CallError::Internal(anyhow::anyhow!("breach control cycle captured without inputs")));
    };
    let inputs = *inputs;
    let module = &cycle.modules[0];
    let (module_id, module_type, module_flag) = (module.id, module.type_id, module.flag);
    let (ship, owner, system) = (cx.ship_id, cx.char_id, cx.system_id);
    let membership = cycle.source_membership
        .ok_or_else(|| CallError::Internal(anyhow::anyhow!("breach control captured before membership binding")))?;
    let incarnation = crate::space::targeting::next_generation();
    let generation = cycle.stop.clone();
    let _lock = market::lock().await;
    let policy = OnlinePolicy { owner, active_ship: Some(ship), station: None, notify: cx.notify.clone() };
    let fences = online::OnlineFences::single(system, ship, Some(membership));
    let sde = cx.world.sde().clone();
    cx.db.transaction(
        |tx, outbox: &mut Outbox<OnlineChanged>| {
            let (policy, fences, sde, generation) = (policy.clone(), fences.clone(), sde.clone(), generation.clone());
            Box::pin(async move {
                if !crate::dao::items::ship::lock_reload_ship(tx, ship).await.map_err(db_err)? { return Err(stale()); }
                let hull = load_items(tx, &sde, ItemQuery::Id(ship)).await.map_err(db_err)?.into_iter().next().ok_or_else(stale)?;
                if hull.owner_id != owner || !hull.singleton || hull.category_id != i64::from(CATEGORY_SHIP) || hull.location_id != system { return Err(stale()); }
                let item = load_items(tx, &sde, ItemQuery::Id(module_id)).await.map_err(db_err)?.into_iter().next().ok_or_else(stale)?;
                if item.owner_id != owner || item.location_id != ship || !item.singleton || item.type_id != module_type
                    || item.category_id != i64::from(CATEGORY_MODULE) || item.flag_id != module_flag
                    || !crate::services::inventory::is_fitting_flag(item.flag_id) { return Err(stale()); }
                if !super::lifecycle::has_effect(sde.types.effects(item.type_id), EFFECT_MODULE_BONUS_BREACHER_POD_DAMAGE_CONTROL) { return Err(stale()); }
                if !dogma::online(tx, ship).await.map_err(db_err)?.iter().any(|entry| entry.item_id == module_id) { return Err(stale()); }
                {
                    let state = combat::state();
                    if !state.active.get(&module_id).is_some_and(|active| active.char_id == owner
                        && !active.forced_inactive && !active.stop.load(Ordering::Relaxed)
                        && Arc::ptr_eq(&active.stop, &generation)) { return Err(stale()); }
                }
                let overrides = dogma::overrides_for(tx, &[module_id]).await.map_err(db_err)?;
                let old = overrides.iter().find(|v| v.item_id == module_id && v.attribute_id == ATTR_HULL_DAMAGE).map(|v| v.state());
                let damage = old.map(|state| state.value).unwrap_or(0.0);
                if damage >= inputs.hp { return Err(stale()); }
                // The terminal publication is measured against the state
                // before the damage write; the online mutation supplies the
                // final fit.
                let before = online::fitting::capture(tx, &sde, &fences, owner, ship).await?;
                if !dogma::set_override_if(tx, module_id, ATTR_HULL_DAMAGE, old, inputs.hp).await.map_err(db_err)? { return Err(stale()); }
                let attr = super::notify::attribute_change(owner, Value::Int(module_id), ATTR_HULL_DAMAGE, Value::Float(inputs.hp), Value::Float(damage));
                outbox.notify_sender(policy.notify.clone(), "OnModuleAttributeChanges", vec![Value::list([attr])]);
                let (_, final_fit) = online::set_online_forced(tx, &sde, &policy, outbox, &fences, ship, module_id, false).await?;
                online::fitting::finish(tx, before, final_fit, outbox).await
            })
        },
        |value, outbox| async move { online::dispatch_changed_world(value, outbox, &cx.world) },
    ).await?;
    // The committed offline stopped the module cycle, so the landing admits
    // only the original ship membership and pilot, never a module state.
    let accepted = cx.world.with(system, move |s| {
        (s.combat_membership_generation(ship) == Some(membership)
            && s.ball(ship).is_some_and(|ball| ball.char_id == Some(owner)))
            .then(|| s.land_runtime_inputs(&[TimedRuntimeInput {
                receiver: RuntimeReceiverKey::Ship(ship),
                collection_id: DBUFF_BPDC_DAMAGE_RECEIVED_PERCENTAGE,
                source: RuntimeSourceKey { producer: BREACH_CONTROL_NAMESPACE, source_id: module_id, incarnation },
                value: inputs.received_percentage,
                expires_at_ms: s.now_ms().saturating_add(inputs.duration_ms as i64),
            }]))
    }).await.flatten();
    if accepted != Some(1) {
        warn!(system, ship, module = module_id, incarnation, "breach control landing refused after committed burnout");
    }
    Ok(CycleOutcome::Stop("burnt out"))
}

#[cfg(all(test, feature = "tests-effects"))]
mod tests;
