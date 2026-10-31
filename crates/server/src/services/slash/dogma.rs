//! GM per-item dogma base overrides. The slash role gate is owned by the parent service.
//! Mirrors godma item attributes and the client brain's hull preAssignment effects;
//! overrides live in game.item_attributes and follow singleton item identity.
//! Owned drone queries evaluate the owner's fit; no drone gameplay RPC is provided.
//! Incoming drone projections require the in-space drone runtime.
//! Hull and fitted-module overrides reconcile online resources atomically.
//! Charges, characters, structures, NPCs and stacked items are not supported.
use super::{slash_error, Client, CallError};
use crate::{dao::items::{dogma as dao, fit as fit_dao, loading::{Item, ItemQuery}}, messages};
use crate::consts::{CATEGORY_CHARGE, CATEGORY_DRONE, CATEGORY_ENTITY, CATEGORY_MODULE, CATEGORY_OWNER};
use crate::services::{combat, db_err, fit::{self, Fit, Key}, items::load_items, effects::{notify, propulsion}};
use crate::db::outbox::Outbox;
use crate::services::effects::online::{self, OnlineChanged};
use marshal::Value;

/// SDE invCategories: Starbase.
const CATEGORY_STARBASE: i64 = 23;
/// SDE invCategories: Infrastructure Upgrade.
const CATEGORY_INFRASTRUCTURE_UPGRADE: i64 = 39;
/// SDE invCategories: Sovereignty Structures.
const CATEGORY_SOVEREIGNTY_STRUCTURES: i64 = 40;
/// SDE invCategories: Structure.
const CATEGORY_STRUCTURE: i64 = 65;
/// SDE invCategories: Structure Module.
const CATEGORY_STRUCTURE_MODULE: i64 = 66;
/// Categories that are not override targets.
const EXCLUDED_CATEGORIES: [i64; 8] = [
    CATEGORY_OWNER as i64, CATEGORY_CHARGE as i64, CATEGORY_ENTITY as i64,
    CATEGORY_STARBASE, CATEGORY_INFRASTRUCTURE_UPGRADE, CATEGORY_SOVEREIGNTY_STRUCTURES,
    CATEGORY_STRUCTURE, CATEGORY_STRUCTURE_MODULE,
];
/// SDE dgmAttributeTypes: the five primary character attributes.
const CHARACTER_ATTRIBUTES: std::ops::RangeInclusive<i32> = 164..=168;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Target { Ship, Item(i64), Flag(i32) }
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Action<'a> { List, Query(&'a str), Reset(&'a str), Set(&'a str, f64) }

pub(super) fn parse(args: &[String]) -> Option<(Target, Action<'_>)> {
    let target = match args.first()?.as_str() {
        "ship" => Target::Ship,
        s if s.starts_with("flag:") => Target::Flag(s[5..].parse::<i32>().ok().filter(|v| *v >= 0)?),
        s => Target::Item(s.parse::<i64>().ok().filter(|v| *v > 0)?),
    };
    let action = match &args[1..] {
        [attr] if attr == "list" => Action::List,
        [attr] if !attr.is_empty() => Action::Query(attr),
        [attr, value] if !attr.is_empty() && attr != "list" && value == "reset" => Action::Reset(attr),
        [attr, value] if !attr.is_empty() && attr != "list" => {
            let value = value.parse::<f64>().ok().filter(|v| v.is_finite())?;
            Action::Set(attr, value)
        }
        _ => return None,
    };
    Some((target, action))
}

fn attribute(c: &Client, name: &str) -> Result<i32, CallError> {
    if let Ok(id) = name.parse::<i32>() {
        return c.sde.dogma.attribute(id).map(|_| id).ok_or_else(|| slash_error(messages::gm::gm_dogma_attribute(name)));
    }
    let mut ids: Vec<_> = c.sde.dogma.attributes_by_name(name).map(|a| a.attribute_id).collect();
    ids.sort_unstable();
    match ids.as_slice() {
        [id] => Ok(*id),
        [] => Err(slash_error(messages::gm::gm_dogma_attribute(name))),
        _ => Err(slash_error(messages::gm::gm_dogma_ambiguous(name, &ids.iter().map(ToString::to_string).collect::<Vec<_>>().join(", ")))),
    }
}

pub(super) fn denied(id: i32, matches_runtime: impl Fn(&str, i32) -> bool) -> Option<&'static str> {
    if CHARACTER_ATTRIBUTES.contains(&id) { return Some(messages::gm::GM_DOGMA_CHARACTER); }
    let runtime = ["isOnline", "damage", "charge", "skillPoints", "skillLevel", "armorDamage", "shieldCharge", "isIncapacitated", "quantity"];
    runtime.iter().any(|name| matches_runtime(name, id)).then_some(messages::gm::GM_DOGMA_RUNTIME)
}

async fn item(c: &Client, id: i64) -> Result<Item, CallError> {
    load_items(&c.db, &c.sde, ItemQuery::Id(id)).await.map_err(db_err)?.into_iter().next()
        .ok_or_else(|| slash_error(messages::gm::gm_dogma_invalid_target(&id.to_string())))
}

fn drone_query_key(item: &Item, action: Action<'_>, char_id: Option<i64>) -> Option<Key> {
    (matches!(action, Action::Query(_))
        && item.category_id == i64::from(CATEGORY_DRONE)
        && char_id == Some(item.owner_id)).then_some(Key::Drone(item.item_id))
}

fn current_projection(s: &mut crate::space::System, ship: i64, revision: u64) -> bool {
    if s.projection_revision(ship) == revision { return true; }
    s.refresh_projection_target(ship);
    false
}

pub(super) async fn command(c: &mut Client, args: &[String]) -> Result<String, CallError> {
    let (target, action) = parse(args).ok_or_else(|| slash_error(messages::gm::GM_DOGMA_USAGE.into()))?;
    let _lock = if matches!(action, Action::Query(_) | Action::List) { None } else { Some(crate::services::market::lock().await) };
    let ship = c.session.int("shipid");
    let modules = if let Some(ship) = ship { fit_dao::modules(&c.db, ship).await.map_err(db_err)? } else { Vec::new() };
    let id = match target {
        Target::Ship => ship.ok_or_else(|| slash_error(messages::gm::GM_DOGMA_NO_SHIP.into()))?,
        Target::Item(id) => id,
        Target::Flag(flag) => modules.iter().find(|m| m.flag_id == flag).map(|m| m.item_id)
            .ok_or_else(|| slash_error(messages::gm::gm_dogma_invalid_target(&flag.to_string())))?,
    };
    let item = item(c, id).await?;
    if EXCLUDED_CATEGORIES.contains(&item.category_id) {
        return Err(slash_error(messages::gm::GM_DOGMA_FORBIDDEN_TARGET.into()));
    }
    if !item.singleton { return Err(slash_error(messages::gm::GM_DOGMA_SINGLETON.into())); }
    if action == Action::List {
        let mut items = vec![item];
        if matches!(target, Target::Ship) {
            for module in &modules { items.push(self::item(c, module.item_id).await?); }
        }
        let ids: Vec<_> = items.iter().map(|i| i.item_id).collect();
        let rows = dao::overrides_for(&c.db, &ids).await.map_err(db_err)?;
        let mut lines = Vec::new();
        for i in &items {
            let name = c.sde.types.get(i.type_id).and_then(|t| t.name.as_deref()).unwrap_or("");
            lines.push(messages::gm::gm_dogma_list_item(i.flag_id, name, i.item_id));
            let mut found = false;
            for row in rows.iter().filter(|r| r.item_id == i.item_id) {
                found = true;
                lines.push(messages::gm::gm_dogma_list_entry(row.attribute_id, row.value));
            }
            if !found { lines.push(messages::gm::GM_DOGMA_EMPTY.into()); }
        }
        return Ok(lines.join("\n"));
    }
    let attr_name = match action { Action::Query(a) | Action::Reset(a) | Action::Set(a, _) => a, Action::List => unreachable!() };
    let attr = attribute(c, attr_name)?;
    if let Some(reason) = denied(attr, |name, id| c.sde.dogma.attributes_by_name(name).any(|a| a.attribute_id == id)) {
        return Err(slash_error(reason.into()));
    }
    let drone_key = drone_query_key(&item, action, c.session.int("charid"));
    let mut fit_ship = ship;
    let mut fit_pilot = c.session.int("charid").unwrap_or(0);
    let mut fit_system = c.session.int("solarsystemid");
    let mut key = if matches!(action, Action::Query(_)) && item.category_id == i64::from(CATEGORY_DRONE) {
        drone_key
    } else if Some(id) == ship { Some(Key::Ship) } else if modules.iter().any(|m| m.item_id == id) { Some(Key::Module(id)) } else { None };
    if key.is_none() && item.category_id != i64::from(CATEGORY_DRONE) {
        let candidate = if item.category_id == i64::from(crate::consts::CATEGORY_SHIP) { id }
            else if item.category_id == i64::from(CATEGORY_MODULE) { item.location_id } else { 0 };
        let hull = if candidate == id { Some(item.clone()) } else {
            load_items(&c.db, &c.sde, ItemQuery::Id(candidate)).await.map_err(db_err)?.into_iter().next()
        };
        if let Some(hull) = hull {
            let system = hull.location_id;
            let fitted = candidate == id || fit_dao::modules(&c.db, candidate).await.map_err(db_err)?.iter().any(|m| m.item_id == id);
            if fitted && c.sde.map.system(system).is_some() {
                if let Some(pilot) = c.world.with(system, move |s| s.ball(candidate).and_then(|b| b.char_id)).await.flatten() {
                    fit_ship = Some(candidate);
                    fit_pilot = pilot;
                    fit_system = Some(system);
                    key = Some(if candidate == id { Key::Ship } else { Key::Module(id) });
                }
            }
        }
    }
    if !matches!(action, Action::Query(_)) {
        let cycle = ["duration", "speed", "capacitorNeed", "reloadTime"];
        if combat::active_modules().contains(&id)
            && cycle.iter().any(|name| c.sde.dogma.attributes_by_name(name).any(|a| a.attribute_id == attr)) {
            return Err(slash_error(messages::gm::GM_DOGMA_ACTIVE_CYCLE.into()));
        }
    }
    let cascade_ship = if matches!(action, Action::Query(_)) { None }
        else if item.category_id == i64::from(crate::consts::CATEGORY_SHIP) { Some(id) }
        else if item.category_id == i64::from(CATEGORY_MODULE) && crate::services::inventory::is_fitting_flag(item.flag_id) {
            let hull = load_items(&c.db, &c.sde, ItemQuery::Id(item.location_id)).await.map_err(db_err)?.into_iter().next();
            hull.filter(|h| h.owner_id == item.owner_id && h.category_id == i64::from(crate::consts::CATEGORY_SHIP) && h.singleton).map(|h| h.item_id)
        } else { None };
    let rows = dao::overrides(&c.db, id).await.map_err(db_err)?;
    let old_override = rows.iter().find(|r| r.attribute_id == attr).map(|r| r.value);
    if matches!(action, Action::Reset(_)) && old_override.is_none() { return Ok(messages::gm::gm_dogma_noop(id, attr)); }
    let fallback = fit::base_attribute(c.sde.types.get(item.type_id), c.sde.dogma.attribute(attr), attr);
    let char_id = c.session.int("charid").unwrap_or(0);
    let old_fit = if drone_key.is_some() {
        Some(Fit::drone(&c.db, c.sde.clone(), char_id, id).await?)
    } else if key.is_some() {
        Some(if let Some(system) = fit_system {
            Fit::load_in_space(&c.db, &c.world, system, fit_pilot, fit_ship.unwrap()).await?
        } else {
            Fit::load(&c.db, c.sde.clone(), fit_pilot, fit_ship.unwrap()).await?
        })
    } else { None };
    let old_base = old_fit.as_ref().zip(key).map(|(fit, key)| fit.base(key, attr)).unwrap_or(old_override.unwrap_or(fallback));
    let old_effective = old_fit.as_ref().zip(key).map(|(fit, key)| fit.value(key, attr));
    if !matches!(action, Action::Query(_)) {
        let db = c.db.clone();
        let sde = c.sde.clone();
        let owner = item.owner_id;
        let value = if let Action::Set(_, value) = action { Some(value) } else { None };
        let client = &*c;
        db.transaction(|tx, outbox: &mut Outbox<OnlineChanged>| {
            let sde = sde.clone();
            Box::pin(async move {
                if let Some(hull) = cascade_ship {
                    if !crate::dao::items::ship::lock_reload_ship(tx, hull).await.map_err(db_err)? {
                        return Err(slash_error(messages::gm::GM_DOGMA_FORBIDDEN_TARGET.into()));
                    }
                    let target = load_items(tx, &sde, ItemQuery::Id(id)).await.map_err(db_err)?.into_iter().next()
                        .ok_or_else(|| slash_error(messages::gm::GM_DOGMA_FORBIDDEN_TARGET.into()))?;
                    if target.owner_id != owner || !target.singleton || (id != hull && (target.location_id != hull || !crate::services::inventory::is_fitting_flag(target.flag_id))) {
                        return Err(slash_error(messages::gm::GM_DOGMA_FORBIDDEN_TARGET.into()));
                    }
                }
                if let Some(value) = value {
                    if !dao::set_override(tx, id, attr, value).await.map_err(db_err)? {
                        return Err(slash_error(messages::gm::GM_DOGMA_SINGLETON.into()));
                    }
                } else {
                    dao::clear_override(tx, id, attr).await.map_err(db_err)?;
                }
                if let Some(hull) = cascade_ship { online::reconcile_core(tx, &sde, outbox, owner, hull).await?; }
                Ok(())
            })
        }, |(), outbox| async move { online::dispatch_changed((), outbox, client) }).await?;
    }
    let changed = !matches!(action, Action::Query(_));
    let new_fit = if changed && key.is_some() {
        Some(if let Some(system) = fit_system {
            Fit::load_in_space(&c.db, &c.world, system, fit_pilot, fit_ship.unwrap()).await?
        } else {
            Fit::load(&c.db, c.sde.clone(), fit_pilot, fit_ship.unwrap()).await?
        })
    } else { None };
    let current_fit = new_fit.as_ref().or(old_fit.as_ref());
    let new_base = current_fit.zip(key).map(|(fit, key)| fit.base(key, attr)).unwrap_or(match action { Action::Set(_, value) => value, Action::Reset(_) => fallback, _ => old_base });
    let new_effective = current_fit.zip(key).map(|(fit, key)| fit.value(key, attr));
    if changed {
        if let (Some(old), Some(new), Some(key)) = (old_fit.as_ref(), new_fit.as_ref(), key) {
            let before = propulsion::kinematics(old);
            let after = propulsion::kinematics(new);
            if before != after {
                if let (Some(system), Some(ship), Some(revision)) = (fit_system, fit_ship, new.projection_revision()) {
                    c.world.post(system, move |s| {
                        if current_projection(s, ship, revision) {
                            s.queue_projection_kinematics(ship, revision, after);
                        }
                    });
                }
            }
            let entry = notify::attribute_change(fit_pilot, Value::Int(id), attr, Value::Float(new.value(key, attr)), Value::Float(old.value(key, attr)));
            c.world.notify_pilot(fit_pilot, "OnModuleAttributeChanges", vec![Value::list([entry])]);
            if fit_pilot == char_id && key == Key::Ship && new.type_has_attribute(key, attr) { crate::services::dogma::brain::update(c).await?; }
        }
    }
    let overridden = match action { Action::Set(..) => true, Action::Reset(_) => false, _ => old_override.is_some() };
    let mut result = messages::gm::gm_dogma_result(id, attr_name, attr, old_base, new_base, old_effective.zip(new_effective), overridden);
    if key.is_none() { result.push_str("\n"); result.push_str(messages::gm::GM_DOGMA_BASE_ONLY); }
    let hull_foreign = item.category_id == i64::from(crate::consts::CATEGORY_SHIP)
        && attr != crate::consts::ATTR_CAPACITY
        && c.sde.types.get(item.type_id).is_none_or(|data| data.attribute(attr).is_none());
    if item.category_id == i64::from(CATEGORY_MODULE) || hull_foreign {
        result.push_str("\n"); result.push_str(messages::gm::GM_DOGMA_VISIBILITY);
    }
    Ok(result)
}

#[cfg(test)]
mod tests;
