//! Durable module heat injuries and burnout.
//! Damage probability follows EVE University's published overheating research;
//! effective attributes come from Fit and condition is stored in game.item_attributes.
//! Burnout publishes one final fitting after all damage writes and offlines.
//! Rack heat itself is transient; reset epochs discard already-covered captures.
//! Paste repair and structure repair are unsupported.

use std::sync::Arc;
use infinity_core::consts::{dogma::ATTR_HULL_DAMAGE, dogma::ATTR_STRUCTURE_HP, dogma::ATTR_LOW_SLOTS, dogma::ATTR_MED_SLOTS, dogma::ATTR_HI_SLOTS, dogma::ATTR_RIG_SLOTS, dogma::EFFECT_EMERGENCY_HULL_ENERGIZER, CATEGORY_MODULE, CATEGORY_SHIP};
use crate::dao::items::{dogma, loading::ItemQuery};
use crate::db::outbox::Outbox;
use infinity_core::messages;
use crate::services::{combat::{self, Ctx}, db_err, fit::{Fit, Key}, items::load_items, market, notify_error, rand01, CallError};
use crate::services::effects::online::{self, OnlineChanged, OnlinePolicy};
use marshal::Value;

fn exhausted(hp: f64, damage: f64) -> bool {
    hp.is_finite() && hp > 0.0 && damage >= hp
}


fn injury(damage: f64, hp: f64, amount: f64, chance: f64, roll: f64) -> f64 {
    if !damage.is_finite() || damage < 0.0 || !hp.is_finite() || hp <= 0.0
        || !amount.is_finite() || amount <= 0.0 || exhausted(hp, damage) || !(roll < chance) {
        return damage;
    }
    (damage + amount).min(hp)
}

fn accumulate(
    sources: &[super::SourceSnapshot],
    damage: &mut [(i64, i32, Option<crate::dao::items::dogma::OverrideState>, f64, f64)],
    rolls: &[(i64, i64, f64)],
    total: usize,
) -> Result<(), CallError> {
    let mut online_count = damage.len();
    for source in sources {
        if !source.heat_damage.is_finite() || source.heat_damage <= 0.0 { continue; }
        for entry in &mut *damage {
            if super::rack_index(source.flag) != super::rack_index(entry.1) || exhausted(entry.4, entry.3) || !entry.4.is_finite() || entry.4 <= 0.0 || !entry.3.is_finite() || entry.3 < 0.0 { continue; }
            let chance = super::rack::damage_chance(source.heat, source.capacity, online_count, total, source.attenuation, source.flag, entry.1, true);
            let roll = rolls.iter().find(|(src, item, _)| *src == source.module && *item == entry.0).ok_or_else(stale)?.2;
            entry.3 = injury(entry.3, entry.4, source.heat_damage, chance, roll);
            if exhausted(entry.4, entry.3) { online_count -= 1; }
        }
    }
    Ok(())
}
pub(crate) fn burnt(fit: &Fit, module: i64) -> bool {
    exhausted(fit.value(Key::Module(module), ATTR_STRUCTURE_HP), fit.value(Key::Module(module), ATTR_HULL_DAMAGE))
}

pub(crate) fn validate(fit: &Fit, module: i64) -> Result<(), CallError> {
    if burnt(fit, module) { Err(notify_error(messages::heat::BURNT_OUT)) } else { Ok(()) }
}

fn stale() -> CallError { CallError::Internal(anyhow::anyhow!("heat damage context changed")) }

/// Apply every source in one completed cycle, with one captured roll per eligible pair.
pub(in crate::services::effects) async fn apply(cx: &Ctx, membership: Option<u64>, mut sources: Vec<super::SourceSnapshot>) -> Result<(), CallError> {
    if sources.is_empty() { return Ok(()); }
    sources.sort_unstable_by_key(|source| source.module);
    let _lock = market::lock().await;
    let mut write = 0;
    for read in 0..sources.len() {
        match super::damage_disposition(&cx.world, cx.system_id, cx.ship_id, cx.char_id, &sources[read]).await {
            super::DamageDisposition::Apply => {
                if write != read { sources.swap(write, read); }
                write += 1;
            }
            super::DamageDisposition::ResetCovered => {}
            super::DamageDisposition::Stale => return Err(stale()),
        }
    }
    sources.truncate(write);
    if sources.is_empty() { return Ok(()); }
    let ship = cx.ship_id;
    let owner = cx.char_id;
    let system = cx.system_id;
    let rows = crate::dao::items::fit::modules(&cx.db, ship).await.map_err(db_err)?;
    let mut eligible: Vec<_> = rows.into_iter().filter(|m| m.online && super::rack_index(m.flag_id).is_some()).map(|m| (m.item_id, m.flag_id)).collect();
    eligible.sort_unstable_by_key(|m| m.0);
    let rolls: Vec<_> = sources.iter().flat_map(|source| eligible.iter().filter_map(move |&(item, flag)| {
        (super::rack_index(source.flag) == super::rack_index(flag)).then(|| (source.module, item, rand01()))
    })).collect();
    let projections = cx.world.with(cx.system_id, move |s| s.projections_on(ship)).await.ok_or_else(stale)?;
    let policy = OnlinePolicy { owner, active_ship: Some(ship), station: None, notify: cx.notify.clone() };
    let sde = cx.world.sde().clone();
    cx.db.transaction(
        |tx, outbox: &mut Outbox<OnlineChanged>| {
            let sources = sources.clone();
            let eligible = eligible.clone();
            let rolls = rolls.clone();
            let projections = projections.clone();
            let policy = policy.clone();
            let sde = sde.clone();
            Box::pin(async move {
                if !crate::dao::items::ship::lock_reload_ship(tx, ship).await.map_err(db_err)? { return Err(stale()); }
                let hull = load_items(tx, &sde, ItemQuery::Id(ship)).await.map_err(db_err)?.into_iter().next().ok_or_else(stale)?;
                if hull.owner_id != owner || !hull.singleton || hull.category_id != i64::from(CATEGORY_SHIP) || hull.location_id != system { return Err(stale()); }
                let modules = crate::dao::items::fit::modules(tx, ship).await.map_err(db_err)?;
                let mut current: Vec<_> = modules.iter().filter(|m| m.online && super::rack_index(m.flag_id).is_some()).map(|m| (m.item_id, m.flag_id)).collect();
                current.sort_unstable_by_key(|m| m.0);
                if current != eligible { return Err(stale()); }
                let ids: Vec<_> = current.iter().map(|m| m.0).collect();
                let items = load_items(tx, &sde, ItemQuery::Ids(&ids)).await.map_err(db_err)?;
                if items.len() != ids.len() || items.iter().any(|item| item.owner_id != owner || item.location_id != ship || !item.singleton || item.category_id != i64::from(CATEGORY_MODULE)) { return Err(stale()); }
                for source in &sources {
                    if !current.contains(&(source.module, source.flag)) { return Err(stale()); }
                }
                let fit = Fit::load_projected(tx, sde.clone(), owner, ship, projections).await?;
                let slots = [ATTR_LOW_SLOTS, ATTR_MED_SLOTS, ATTR_HI_SLOTS, ATTR_RIG_SLOTS].into_iter().map(|attr| fit.value(Key::Ship, attr)).sum::<f64>();
                let total = if slots.is_finite() && slots > 0.0 { slots as usize } else { 0 };
                let overrides = dogma::overrides_for(tx, &ids).await.map_err(db_err)?;
                let mut damage: Vec<_> = current.iter().map(|&(id, flag)| {
                    let old = overrides.iter().find(|v| v.item_id == id && v.attribute_id == ATTR_HULL_DAMAGE).map(|v| v.state());
                    (id, flag, old, old.map(|state| state.value).unwrap_or(0.0), fit.value(Key::Module(id), ATTR_STRUCTURE_HP))
                }).collect();
                accumulate(&sources, &mut damage, &rolls, total)?;
                let fences = online::OnlineFences::single(system, ship, membership);
                // A burnout publishes one final fit measured against the state
                // before any damage write, so the already-loaded fit moves into
                // the before boundary now instead of being captured too late.
                let mut before = None;
                if damage.iter().any(|&(_, _, old, new, hp)| old.map(|state| state.value).unwrap_or(0.0) != new && exhausted(hp, new)) {
                    before = Some(online::fitting::capture_from_fit(tx, &sde, &fences, owner, ship, fit).await?);
                }
                let mut damaged = Vec::new();
                for &(id, _, old, new, _) in &damage {
                    if old.map(|state| state.value).unwrap_or(0.0) == new { continue; }
                    if !dogma::set_override_if(tx, id, ATTR_HULL_DAMAGE, old, new).await.map_err(db_err)? { return Err(stale()); }
                    damaged.push((Key::Module(id), ATTR_HULL_DAMAGE, Some(new)));
                    let attr = super::super::notify::attribute_change(owner, Value::Int(id), ATTR_HULL_DAMAGE, Value::Float(new), Value::Float(old.map(|state| state.value).unwrap_or(0.0)));
                    outbox.notify_sender(policy.notify.clone(), "OnModuleAttributeChanges", vec![Value::list([attr])]);
                }
                let mut final_fit = None;
                for &(id, _, old, new, hp) in &damage {
                    if old.map(|state| state.value).unwrap_or(0.0) != new && exhausted(hp, new) {
                        let (_, fit) = online::set_online_core(tx, &sde, &policy, outbox, &fences, ship, id, false).await?;
                        final_fit = Some(fit);
                    }
                }
                if let (Some(before), Some(fit)) = (before, final_fit) {
                    online::fitting::finish(tx, before, fit, outbox).await?;
                } else if !damaged.is_empty() {
                    // Without a burnout the committed damage values publish as a
                    // single field-delta patch; nothing reloads or rewrites them.
                    if let Some((system, generation)) = fences.membership(ship) {
                        let patch = crate::services::fit::attributes::CommittedFitting::overridden_attributes(damaged);
                        outbox.operation(OnlineChanged::Fitting { system, ship, owner, generation, patch }.into());
                    }
                }
                {
                    let state = combat::state();
                    if sources.iter().any(|source| !state.active.get(&source.master).is_some_and(|active| active.char_id == owner && Arc::ptr_eq(&active.stop, &source.generation))) { return Err(stale()); }
                }
                Ok(())
            })
        },
        |value, outbox| async move { online::dispatch_changed_world(value, outbox, &cx.world) },
    ).await
}

/// The energizer's one pilot-initiated activation write: module damage becomes
/// exactly the module's captured effective hit points
/// (`eve/common/script/dogma/pythonEffects/emergencyHullEnergizer.py:22-24`).
/// This is not a recurring-cycle write: it runs once per accepted activation.
/// Shares `apply`'s market-lock policy, `lock_reload_ship` and subject/identity
/// revalidation, but never rolls damage, never calls `set_online_core` and
/// never offlines: the same Running activation keeps the burned module alive
/// until its accepted duration ends. A refused transaction emits no write.
pub(in crate::services::effects) async fn burn_for_activation(
    cx: &Ctx, module: i64, hp: f64, membership: Option<u64>,
) -> Result<(), CallError> {
    if !hp.is_finite() || hp <= 0.0 { return Err(stale()); }
    let ship = cx.ship_id;
    let owner = cx.char_id;
    let system = cx.system_id;
    let _lock = market::lock().await;
    let policy = OnlinePolicy { owner, active_ship: Some(ship), station: None, notify: cx.notify.clone() };
    let sde = cx.world.sde().clone();
    cx.db.transaction(
        |tx, outbox: &mut Outbox<OnlineChanged>| {
            let policy = policy.clone();
            let sde = sde.clone();
            Box::pin(async move {
                if !crate::dao::items::ship::lock_reload_ship(tx, ship).await.map_err(db_err)? { return Err(stale()); }
                let hull = load_items(tx, &sde, ItemQuery::Id(ship)).await.map_err(db_err)?.into_iter().next().ok_or_else(stale)?;
                if hull.owner_id != owner || !hull.singleton || hull.category_id != i64::from(CATEGORY_SHIP) || hull.location_id != system { return Err(stale()); }
                let item = load_items(tx, &sde, ItemQuery::Id(module)).await.map_err(db_err)?.into_iter().next().ok_or_else(stale)?;
                if item.owner_id != owner || item.location_id != ship || !item.singleton
                    || item.category_id != i64::from(CATEGORY_MODULE)
                    || !crate::services::inventory::is_fitting_flag(item.flag_id) { return Err(stale()); }
                // Only the energizer may burn on activation; any other caller
                // or a type that no longer declares the effect is a stale write.
                if !super::super::lifecycle::has_effect(sde.types.effects(item.type_id), EFFECT_EMERGENCY_HULL_ENERGIZER) { return Err(stale()); }
                if !crate::dao::items::dogma::online(tx, ship).await.map_err(db_err)?.iter().any(|entry| entry.item_id == module) { return Err(stale()); }
                {
                    let state = combat::state();
                    if !state.active.get(&module).is_some_and(|active| active.char_id == owner) { return Err(stale()); }
                }
                let overrides = dogma::overrides_for(tx, &[module]).await.map_err(db_err)?;
                let old = overrides.iter().find(|v| v.item_id == module && v.attribute_id == ATTR_HULL_DAMAGE).map(|v| v.state());
                let damage = old.map(|state| state.value).unwrap_or(0.0);
                // An already-exhausted module was refused by the admission's
                // burnt validation; a racy change inside the lock is stale.
                if exhausted(hp, damage) { return Err(stale()); }
                if !dogma::set_override_if(tx, module, ATTR_HULL_DAMAGE, old, hp).await.map_err(db_err)? { return Err(stale()); }
                let attr = super::super::notify::attribute_change(owner, Value::Int(module), ATTR_HULL_DAMAGE, Value::Float(hp), Value::Float(damage));
                outbox.notify_sender(policy.notify.clone(), "OnModuleAttributeChanges", vec![Value::list([attr])]);
                let fences = online::OnlineFences::single(system, ship, membership);
                // The damage-only committed patch updates the resident fit;
                // nothing reloads or rewrites the rest of the module node.
                if let Some((system, generation)) = fences.membership(ship) {
                    let patch = crate::services::fit::attributes::CommittedFitting::overridden_attributes(vec![(Key::Module(module), ATTR_HULL_DAMAGE, Some(hp))]);
                    outbox.operation(OnlineChanged::Fitting { system, ship, owner, generation, patch }.into());
                }
                Ok(())
            })
        },
        |value, outbox| async move { online::dispatch_changed_world(value, outbox, &cx.world) },
    ).await
}

#[cfg(all(test, feature = "tests-effects"))] mod tests;
