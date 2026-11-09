//! Transaction-local fitting snapshots and terminal publications. Effective values
//! use the dogma Fit evaluator; stored condition is sampled from game.items.
//! Live condition is owned by the system actor, not sampled here.

use std::sync::Arc;
use infinity_core::consts::dogma::{ATTR_ARMOR_DAMAGE, ATTR_ARMOR_HP, ATTR_CAPACITOR_CAPACITY, ATTR_CHARGE, ATTR_HULL_DAMAGE, ATTR_RECHARGE_RATE, ATTR_SHIELD_CAPACITY, ATTR_SHIELD_CHARGE, ATTR_SHIELD_RECHARGE_RATE, ATTR_STRUCTURE_HP};
use crate::db::{executor::Executor, outbox::Outbox};
use crate::sde::Sde;
use crate::services::{CallError, combat::{self, ConditionSeed}, fit::{Fit, Key}};
use super::{OnlineChanged, OnlineFences};

pub(in crate::services) struct FittingBefore {
    owner: i64,
    location: Option<i64>,
    ship: i64,
    before: Fit,
    condition: Option<ConditionSeed>,
    membership: Option<(i64, u64)>,
}

pub(in crate::services) async fn capture(ex: &(impl Executor + ?Sized), sde: &Arc<Sde>, fences: &OnlineFences, owner: i64, ship: i64) -> Result<FittingBefore, CallError> {
    let before = Fit::load(ex, sde.clone(), owner, ship).await?;
    let membership = fences.membership(ship);
    let (condition, location) = if membership.is_none() {
        let hull = crate::services::items::load_items(ex, sde, crate::dao::items::loading::ItemQuery::Id(ship)).await
            .map_err(crate::services::db_err)?.into_iter().next()
            .ok_or_else(|| crate::services::notify_error(infinity_core::messages::ITEM_NOT_AT_ORIGINAL_LOCATION))?;
        (Some(combat::prepare_condition_with_fit(ex, ship, &before).await?), Some(hull.location_id))
    } else { (None, None) };
    Ok(FittingBefore { owner, ship, location, before, condition, membership })
}
pub(in crate::services) async fn finish<E: From<OnlineChanged>>(ex: &(impl Executor + ?Sized), mut boundary: FittingBefore, final_fit: Fit, outbox: &mut Outbox<E>) -> Result<(), CallError> {
    if let Some((system, generation)) = boundary.membership {
        let patch = final_fit.into_committed_fitting(&boundary.before);
        outbox.operation(OnlineChanged::Fitting { system, ship: boundary.ship, owner: boundary.owner, generation, patch }.into());
        return Ok(());
    }
    let condition = boundary.condition.as_mut().expect("stored fitting condition");
    let old_condition = condition.scalars();
    condition.rebase_with_fit(&final_fit, Key::Ship)?;
    let shield_changed = boundary.before.value(Key::Ship, ATTR_SHIELD_CAPACITY) != final_fit.value(Key::Ship, ATTR_SHIELD_CAPACITY)
        || boundary.before.value(Key::Ship, ATTR_SHIELD_RECHARGE_RATE) != final_fit.value(Key::Ship, ATTR_SHIELD_RECHARGE_RATE);
    let new_condition = condition.scalars();
    let candidates = boundary.before.fitting_candidates(&final_fit);
    if shield_changed {
        let state = boundary.condition.take().expect("stored fitting condition").into_condition();
        let values = [new_condition.0, new_condition.1, new_condition.2];
        let saved = crate::dao::items::dogma::save_conditions(ex, boundary.location.expect("stored hull location"),
            &[ATTR_SHIELD_CHARGE, ATTR_ARMOR_DAMAGE, ATTR_HULL_DAMAGE], &[(boundary.ship, values, state.shield_at)]).await.map_err(crate::services::db_err)?;
        if !saved.contains(&boundary.ship) { return Err(crate::services::notify_error(infinity_core::messages::ITEM_NOT_AT_ORIGINAL_LOCATION)); }
    }
    let changes = final_fit.fitting_changes(&boundary.before.fitting_values(&candidates));
    let old_cap = boundary.before.value(Key::Ship, ATTR_CAPACITOR_CAPACITY);
    let new_cap = final_fit.value(Key::Ship, ATTR_CAPACITOR_CAPACITY);
    let mut shield_pair = false;
    let mut cap_pair = false;
    let mut notices = Vec::new();
    for (key, attr, old, new) in changes {
        let item = match key { Key::Ship => boundary.ship, Key::Module(id) => id, _ => continue };
        notices.push(super::attribute_change(boundary.owner, marshal::Value::Int(item), attr, marshal::Value::Float(new), marshal::Value::Float(old)));
        if key != Key::Ship { continue; }
        let pair = match attr {
            ATTR_SHIELD_CAPACITY => { shield_pair = true; Some((ATTR_SHIELD_CHARGE, old_condition.0, new_condition.0)) },
            ATTR_ARMOR_HP => Some((ATTR_ARMOR_DAMAGE, old_condition.1, new_condition.1)),
            ATTR_STRUCTURE_HP => Some((ATTR_HULL_DAMAGE, old_condition.2, new_condition.2)),
            ATTR_CAPACITOR_CAPACITY => { cap_pair = true; Some((ATTR_CHARGE, old_cap, new_cap)) },
            _ => None,
        };
        if let Some((id, old, new)) = pair { notices.push(super::attribute_change(boundary.owner, marshal::Value::Int(boundary.ship), id, marshal::Value::Float(new), marshal::Value::Float(old))); }
    }
    if !shield_pair && boundary.before.value(Key::Ship, ATTR_SHIELD_RECHARGE_RATE) != final_fit.value(Key::Ship, ATTR_SHIELD_RECHARGE_RATE) {
        notices.push(super::attribute_change(boundary.owner, marshal::Value::Int(boundary.ship), ATTR_SHIELD_CHARGE, marshal::Value::Float(new_condition.0), marshal::Value::Float(old_condition.0)));
    }
    if !cap_pair && boundary.before.value(Key::Ship, ATTR_RECHARGE_RATE) != final_fit.value(Key::Ship, ATTR_RECHARGE_RATE) {
        notices.push(super::attribute_change(boundary.owner, marshal::Value::Int(boundary.ship), ATTR_CHARGE, marshal::Value::Float(new_cap), marshal::Value::Float(old_cap)));
    }
    outbox.operation(OnlineChanged::StoredFitting { owner: boundary.owner, changes: notices }.into());
    Ok(())
}

#[cfg(test)] mod tests;
