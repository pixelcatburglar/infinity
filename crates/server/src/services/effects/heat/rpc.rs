//! Overload requests from dogmaIM; godma.py consumes rack results as module IDs.
//! Module identities and online status come from game items; effects and attributes
//! come from the SDE and Fit. Unsupported module behaviors cannot be overloaded.
use crate::client::Client;
use crate::consts::{dogma::ATTR_HULL_DAMAGE, dogma::ATTR_STRUCTURE_HP, dogma::ATTR_SKILL_LEVEL, dogma::EFFECT_MUTADAPTIVE_REMOTE_ARMOR_REPAIRER, FLAG_HANGAR};
use crate::dao::items::loading::{Item, ItemQuery};
use crate::messages;
use crate::services::{combat, db_err, fit::{Fit, Key}, items::load_items, market, notify_error, Call, CallError, CallResult};
use crate::services::effects::{lifecycle::{self, Module}, registry, unsupported};
use crate::services::effects::online::OnlinePolicy;
use crate::services::identity::Identity;
use marshal::Value;
use std::collections::BTreeSet;

/// SDE dogma effect category overload.
pub(crate) const OVERLOAD_CATEGORY: i32 = 5;
/// SDE type Thermodynamics and requiredThermoDynamicsSkill attribute.
const THERMODYNAMICS: i32 = 28164;
const REQUIRED_THERMODYNAMICS: i32 = 1212;

fn arguments(args: &[Value], rack: bool) -> Option<(i64, Option<i32>)> {
    if args.len() != if rack { 1 } else { 2 } { return None; }
    let Value::Int(id) = args[0] else { return None };
    if id <= 0 { return None; }
    if rack { return Some((id, None)); }
    let Value::Int(effect) = args[1] else { return None };
    let effect = i32::try_from(effect).ok().filter(|id| *id > 0)?;
    Some((id, Some(effect)))
}

fn overload_effect(c: &Client, item: &Item) -> Option<i32> {
    c.sde.types.effects(item.type_id).iter().find_map(|e| {
        c.sde.dogma.effect(e.effect_id).filter(|row| row.effect_category == Some(OVERLOAD_CATEGORY)).map(|_| e.effect_id)
    })
}

fn result_value(rack: bool, modules: &[(Module, i32)]) -> Value {
    if rack { Value::list(modules.iter().map(|(module, _)| Value::Int(module.id))) } else { Value::None }
}

fn supported(c: &Client, item: &Item) -> bool {
    c.sde.types.effects(item.type_id).iter().any(|entry| {
        entry.effect_id != EFFECT_MUTADAPTIVE_REMOTE_ARMOR_REPAIRER && registry::resolve(&c.sde, entry.effect_id)
            .is_some_and(|effect| effect.manually_activatable() && registry::activation_in_scope(item.category_id, item.group_id, effect.kind)
                && c.sde.dogma.effect(entry.effect_id).is_some_and(|row| matches!(row.effect_category, Some(1 | 2))))
    })
}

fn check_start(c: &Client, item: &Item, fit: &Fit, rack: usize) -> Result<(), CallError> {
    if !supported(c, item) {
        return Err(unsupported::refusal(None, "overload", item.type_id));
    }
    let key = Key::Module(item.item_id);
    if fit.value(key, ATTR_HULL_DAMAGE) >= fit.value(key, ATTR_STRUCTURE_HP) {
        return Err(notify_error(messages::heat::BURNT_OUT));
    }
    if fit.value(Key::Skill(THERMODYNAMICS), ATTR_SKILL_LEVEL) < fit.value(key, REQUIRED_THERMODYNAMICS) {
        return Err(notify_error(messages::heat::THERMODYNAMICS_REQUIRED));
    }
    if fit.value(Key::Ship, super::CAPACITY[rack]) <= 0.0 {
        return Err(notify_error(messages::heat::INVALID_RACK));
    }
    Ok(())
}

pub(crate) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    let (rack, start) = match call.method {
        "Overload" => (false, true), "StopOverload" => (false, false),
        "OverloadRack" => (true, true), "StopOverloadRack" => (true, false),
        _ => return None,
    };
    Some(handle(c, call.args, rack, start).await)
}

async fn handle(c: &mut Client, args: &[Value], rack: bool, start: bool) -> CallResult {
    let (anchor, requested) = arguments(args, rack).ok_or_else(|| notify_error(messages::heat::INVALID_OVERLOAD))?;
    let cx = combat::ctx(c).ok_or_else(|| notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION))?;
    let _lock = market::lock().await;
    let anchor_item = load_items(&c.db, &c.sde, ItemQuery::Id(anchor)).await.map_err(db_err)?.into_iter().next()
        .ok_or_else(|| notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION))?;
    let anchor_effect = overload_effect(c, &anchor_item).filter(|id| requested.is_none_or(|requested| requested == *id))
        .ok_or_else(|| notify_error(messages::heat::INVALID_OVERLOAD))?;
    let policy = OnlinePolicy::from_identity(&Identity::of_client(c), c.notify_tx.clone())?;
    lifecycle::subject(&c.db, &c.sde, &policy, cx.ship_id, anchor, anchor_effect, start).await?;
    let family = super::rack_index(anchor_item.flag_id).ok_or_else(|| notify_error(messages::heat::INVALID_RACK))?;
    let fit = Fit::load_in_space(&cx.db, &cx.world, cx.system_id, cx.char_id, cx.ship_id).await?;
    if start { check_start(c, &anchor_item, &fit, family)?; }
    let online = crate::dao::items::dogma::online(&c.db, cx.ship_id).await.map_err(db_err)?;
    let mut candidates = BTreeSet::from([anchor]);
    if rack {
        for item in load_items(&c.db, &c.sde, ItemQuery::LocationExceptFlag(cx.ship_id, FLAG_HANGAR)).await.map_err(db_err)? {
            if item.owner_id != cx.char_id || !item.singleton || super::rack_index(item.flag_id) != Some(family) || overload_effect(c, &item).is_none() { continue; }
            if start && (!online.iter().any(|row| row.item_id == item.item_id) || check_start(c, &item, &fit, family).is_err()) { continue; }
            candidates.insert(item.item_id);
        }
    }
    let mut selected = BTreeSet::new();
    for id in candidates {
        let master = combat::targeting_charges::master_for_member(&c.db, cx.ship_id, id).await?;
        selected.extend(combat::targeting_charges::bank(&c.db, cx.ship_id, master).await?);
    }
    let mut modules = Vec::with_capacity(selected.len());
    for id in selected {
        let item = load_items(&c.db, &c.sde, ItemQuery::Id(id)).await.map_err(db_err)?.into_iter().next()
            .ok_or_else(|| notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION))?;
        let effect = overload_effect(c, &item).ok_or_else(|| notify_error(messages::heat::INVALID_OVERLOAD))?;
        let item = lifecycle::subject(&c.db, &c.sde, &policy, cx.ship_id, id, effect, start).await?;
        if super::rack_index(item.flag_id) != Some(family) { return Err(notify_error(messages::heat::INVALID_RACK)); }
        if start { check_start(c, &item, &fit, family)?; }
        modules.push((Module { id, type_id: item.type_id, flag: item.flag_id }, effect));
    }
    let result = result_value(rack, &modules);
    super::request(&cx, modules, start).await?;
    Ok(result)
}

#[cfg(test)] mod tests;
