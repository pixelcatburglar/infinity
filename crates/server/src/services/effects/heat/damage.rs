//! Durable module heat injuries and burnout.
//! Damage probability follows EVE University's published overheating research;
//! effective attributes come from Fit and condition is stored in game.item_attributes.
//! Burnout publishes one final fitting after all damage writes and offlines.
//! Rack heat itself is transient; paste repair and structure repair are unsupported.

use std::sync::Arc;
use infinity_core::consts::{dogma::ATTR_HULL_DAMAGE, dogma::ATTR_STRUCTURE_HP, dogma::ATTR_LOW_SLOTS, dogma::ATTR_MED_SLOTS, dogma::ATTR_HI_SLOTS, dogma::ATTR_RIG_SLOTS, CATEGORY_MODULE, CATEGORY_SHIP};
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
    damage: &mut [(i64, i32, Option<f64>, f64, f64)],
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
    for source in &sources {
        if !super::generation_valid(&cx.world, cx.system_id, cx.ship_id, cx.char_id, source).await { return Err(stale()); }
    }
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
                    let old = overrides.iter().find(|v| v.item_id == id && v.attribute_id == ATTR_HULL_DAMAGE).map(|v| v.value);
                    (id, flag, old, old.unwrap_or(0.0), fit.value(Key::Module(id), ATTR_STRUCTURE_HP))
                }).collect();
                accumulate(&sources, &mut damage, &rolls, total)?;
                for &(id, _, old, new, _) in &damage {
                    if old.unwrap_or(0.0) == new { continue; }
                    if !dogma::set_override_if(tx, id, ATTR_HULL_DAMAGE, old, new).await.map_err(db_err)? { return Err(stale()); }
                    let attr = super::super::notify::attribute_change(owner, Value::Int(id), ATTR_HULL_DAMAGE, Value::Float(new), Value::Float(old.unwrap_or(0.0)));
                    outbox.notify_sender(policy.notify.clone(), "OnModuleAttributeChanges", vec![Value::list([attr])]);
                }
                let fences = online::OnlineFences::single(system, ship, membership);
                let mut before = None;
                let mut final_fit = None;
                for &(id, _, old, new, hp) in &damage {
                    if old.unwrap_or(0.0) != new && exhausted(hp, new) {
                        if before.is_none() { before = Some(online::fitting::capture(tx, &sde, &fences, owner, ship).await?); }
                        let (_, fit) = online::set_online_core(tx, &sde, &policy, outbox, &fences, ship, id, false).await?;
                        final_fit = Some(fit);
                    }
                }
                if let (Some(before), Some(fit)) = (before, final_fit) {
                    online::fitting::finish(tx, before, fit, outbox).await?;
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

#[cfg(test)] mod tests;
