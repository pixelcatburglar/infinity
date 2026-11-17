//! GM per-item dogma base overrides. The slash role gate is owned by the parent service.
//! Mirrors godma item attributes and the client brain's hull preAssignment effects;
//! overrides live in game.item_attributes and follow singleton item identity.
//! A hull's capacity override may additionally carry unlimited-cargo mode, set
//! only by the `infinity` literal on an assembled real ship.
//! Owned drone queries evaluate the owner's fit and live incoming projections.
//! Hull and fitted-module overrides reconcile online resources atomically.
//! Charges, characters, structures, NPCs and stacked items are not supported.
//! Live overrides publish fitted defenses without replacing actor-owned condition.
//! `me` is the active ship; an optional `=` in any spacing joins attribute and value (`/dogma me hiSlots = 8`).
use super::{args, slash_error, Client, CallError};
use infinity_core::messages;
use crate::{dao::items::{dogma as dao, fit as fit_dao, loading::{Item, ItemQuery}}};
use infinity_core::consts::{CATEGORY_CHARGE, CATEGORY_DRONE, CATEGORY_ENTITY, CATEGORY_MODULE, CATEGORY_OWNER, CATEGORY_STARBASE, CATEGORY_STRUCTURE};
use infinity_core::consts::dogma::{CHAR_ATTRIBUTES, ATTR_AGILITY, ATTR_MASS, ATTR_MAX_VELOCITY};
use crate::services::{combat, db_err, fit::{self, Fit, Key}, items::load_items, effects::{notify, propulsion}};
use crate::db::outbox::Outbox;
use crate::services::effects::online::{self, OnlineChanged};
use marshal::Value;

/// SDE invCategories: Infrastructure Upgrade.
const CATEGORY_INFRASTRUCTURE_UPGRADE: i64 = 39;
/// SDE invCategories: Sovereignty Structures.
const CATEGORY_SOVEREIGNTY_STRUCTURES: i64 = 40;
/// SDE invCategories: Structure Module.
const CATEGORY_STRUCTURE_MODULE: i64 = 66;
/// Categories that are not override targets.
const EXCLUDED_CATEGORIES: [i64; 8] = [
    CATEGORY_OWNER as i64, CATEGORY_CHARGE as i64, CATEGORY_ENTITY as i64,
    CATEGORY_STARBASE as i64, CATEGORY_INFRASTRUCTURE_UPGRADE, CATEGORY_SOVEREIGNTY_STRUCTURES,
    CATEGORY_STRUCTURE as i64, CATEGORY_STRUCTURE_MODULE,
];

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Target { Ship, Item(i64), Flag(i32) }
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Action<'a> { List, Query(&'a str), Reset(&'a str), Set(&'a str, f64), Unlimited(&'a str) }

pub(super) fn parse(args: &[String]) -> Option<(Target, Action<'_>)> {
    let target = match args.first()?.as_str() {
        "ship" | "me" => Target::Ship,
        s if s.starts_with("flag:") => Target::Flag(s[5..].parse::<i32>().ok().filter(|v| *v >= 0)?),
        s => Target::Item(args::positive_i64(s)?),
    };
    // `=` may join attribute and value in any spacing; normalize the tail to a
    // borrowed (attribute, value) pair without touching the input tokens.
    let (attr, value) = match &args[1..] {
        [one] => match one.split_once('=') {
            None => (one.as_str(), None),
            Some((a, v)) if !a.is_empty() && !v.is_empty() && !v.contains('=') => (a, Some(v)),
            _ => return None,
        },
        [a, b] if !a.contains('=') => match b.strip_prefix('=') {
            None => (a.as_str(), Some(b.as_str())),
            Some(v) if !v.is_empty() && !v.contains('=') => (a.as_str(), Some(v)),
            _ => return None,
        },
        [a, b] => match a.strip_suffix('=') {
            Some(a) if !a.is_empty() && !a.contains('=') && !b.contains('=') => (a, Some(b.as_str())),
            _ => return None,
        },
        [a, eq, b] if eq == "=" && !a.contains('=') && !b.contains('=') => (a.as_str(), Some(b.as_str())),
        _ => return None,
    };
    let action = match (attr, value) {
        ("", _) | ("list", Some(_)) => return None,
        ("list", None) => Action::List,
        (attr, None) => Action::Query(attr),
        (attr, Some("reset")) => Action::Reset(attr),
        (attr, Some("infinity")) => Action::Unlimited(attr),
        (attr, Some(value)) => Action::Set(attr, value.parse::<f64>().ok().filter(|v| v.is_finite())?),
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

fn valid_kinematic_action(attr: i32, action: Action<'_>) -> bool {
    let Action::Set(_, value) = action else { return true };
    match attr {
        ATTR_AGILITY => crate::space::Ball::valid_agility(value),
        ATTR_MASS => crate::space::Ball::valid_mass(value),
        ATTR_MAX_VELOCITY => crate::space::Ball::valid_max_speed(value),
        _ => true,
    }
}

pub(super) fn denied(id: i32, matches_runtime: impl Fn(&str, i32) -> bool) -> Option<&'static str> {
    if CHAR_ATTRIBUTES.contains(&i64::from(id)) { return Some(messages::gm::GM_DOGMA_CHARACTER); }
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

fn drone_location(item: &Item) -> Option<i64> {
    (item.singleton && item.flag_id == 0 && item.category_id == i64::from(CATEGORY_DRONE))
        .then_some(item.location_id)
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
                lines.push(messages::gm::gm_dogma_list_entry(row.attribute_id, row.value, row.cargo_unlimited));
            }
            if !found { lines.push(messages::gm::GM_DOGMA_EMPTY.into()); }
        }
        return Ok(lines.join("\n"));
    }
    let attr_name = match action { Action::Query(a) | Action::Reset(a) | Action::Set(a, _) | Action::Unlimited(a) => a, Action::List => unreachable!() };
    let attr = attribute(c, attr_name)?;
    if !valid_kinematic_action(attr, action) {
        return Err(slash_error(messages::gm::GM_DOGMA_KINEMATIC_INVALID.into()));
    }
    if let Some(reason) = denied(attr, |name, id| c.sde.dogma.attributes_by_name(name).any(|a| a.attribute_id == id)) {
        return Err(slash_error(reason.into()));
    }
    if matches!(action, Action::Unlimited(_)) && attr != infinity_core::consts::dogma::ATTR_CAPACITY {
        return Err(slash_error(messages::gm::GM_DOGMA_UNLIMITED_ATTR.into()));
    }
    let drone_key = drone_query_key(&item, action, c.session.int("charid"));
    let mut fit_ship = ship;
    let mut fit_pilot = c.session.int("charid").unwrap_or(0);
    let mut fit_system = c.session.int("solarsystemid");
    let mut key = if matches!(action, Action::Query(_)) && item.category_id == i64::from(CATEGORY_DRONE) {
        drone_key
    } else if Some(id) == ship { Some(Key::Ship) } else if modules.iter().any(|m| m.item_id == id) { Some(Key::Module(id)) } else { None };
    if key.is_none() && item.category_id != i64::from(CATEGORY_DRONE) {
        let candidate = if item.category_id == i64::from(infinity_core::consts::CATEGORY_SHIP) { id }
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
    let unlimited = matches!(action, Action::Unlimited(_));
    if unlimited && !matches!(target, Target::Ship | Target::Item(_)) {
        return Err(slash_error(messages::gm::GM_DOGMA_UNLIMITED_TARGET.into()));
    }
    if unlimited && !(item.category_id == i64::from(infinity_core::consts::CATEGORY_SHIP) && item.singleton && item.quantity == 1) {
        return Err(slash_error(messages::gm::GM_DOGMA_UNLIMITED_TARGET.into()));
    }
    // Every capacity mutation on a hull (finite set, reset, infinity) takes the
    // same locked patch path so mode and value stay one committed change.
    let capacity_mutation = !matches!(action, Action::Query(_)) && attr == infinity_core::consts::dogma::ATTR_CAPACITY
        && item.category_id == i64::from(infinity_core::consts::CATEGORY_SHIP);
    let cascade_ship = if matches!(action, Action::Query(_)) { None }
        else if item.category_id == i64::from(infinity_core::consts::CATEGORY_SHIP) { Some(id) }
        else if item.category_id == i64::from(CATEGORY_MODULE) && crate::services::inventory::is_fitting_flag(item.flag_id) {
            let hull = load_items(&c.db, &c.sde, ItemQuery::Id(item.location_id)).await.map_err(db_err)?.into_iter().next();
            hull.filter(|h| h.owner_id == item.owner_id && h.category_id == i64::from(infinity_core::consts::CATEGORY_SHIP) && h.singleton).map(|h| h.item_id)
        } else { None };
    let rows = dao::overrides(&c.db, id).await.map_err(db_err)?;
    let old_override = rows.iter().find(|r| r.attribute_id == attr).map(|r| r.value);
    if matches!(action, Action::Reset(_)) && old_override.is_none() && !capacity_mutation { return Ok(messages::gm::gm_dogma_noop(id, attr)); }
    let fallback = fit::base_attribute(c.sde.types.get(item.type_id), c.sde.dogma.attribute(attr), attr);
    let char_id = c.session.int("charid").unwrap_or(0);
    // A live mutation reports the actor's gated values instead of reloading a
    // fit; queries and stored hulls keep the cold loads below.
    let live_mutation = !matches!(action, Action::Query(_)) && fit_system.is_some() && key.is_some();
    let old_fit = if drone_key.is_some() {
        let system = if let Some(location) = drone_location(&item) {
            c.world.with(location, move |s| s.drone(id).is_some() && s.ball(id).is_some())
                .await.filter(|live| *live).map(|_| location)
        } else { None };
        Some(if let Some(system) = system {
            Fit::drone_in_space(&c.db, &c.world, system, char_id, id).await?
        } else {
            Fit::drone(&c.db, c.sde.clone(), char_id, id).await?
        })
    } else if key.is_some() && !live_mutation {
        Some(if let Some(system) = fit_system {
            Fit::load_in_space(&c.db, &c.world, system, fit_pilot, fit_ship.unwrap()).await?
        } else {
            Fit::load(&c.db, c.sde.clone(), fit_pilot, fit_ship.unwrap()).await?
        })
    } else { None };
    let old_live = if live_mutation {
        let (system, ship, key) = (fit_system.unwrap(), fit_ship.unwrap(), key.unwrap());
        c.world.with(system, move |s| s.with_combat_fit(ship, move |fit, _, _| (fit.base(key, attr), fit.value(key, attr)))).await.flatten()
    } else { None };
    let old_base = old_live.map(|(base, _)| base)
        .or_else(|| old_fit.as_ref().zip(key).map(|(fit, key)| fit.base(key, attr)))
        .unwrap_or(old_override.unwrap_or(fallback));
    let old_effective = old_live.map(|(_, effective)| effective)
        .or_else(|| old_fit.as_ref().zip(key).map(|(fit, key)| fit.value(key, attr)));
    let drone_token = if !matches!(action, Action::Query(_)) {
        if let Some(system) = drone_location(&item) {
            let owner = item.owner_id;
            c.world.with(system, move |s| {
                let drone = s.drone(id)?;
                if drone.owner != owner || owner == infinity_core::consts::OWNER_UNOWNED { return None; }
                // The record generation pins this exact live instance for the
                // post-commit patch; the membership binds its combat record.
                Some((system, owner, drone.generation, s.combat_membership_generation(id)?))
            }).await.flatten()
        } else { None }
    } else { None };
    let mut wrote = false;
    let mut written_state = None;
    if !matches!(action, Action::Query(_)) {
        let db = c.db.clone();
        let sde = c.sde.clone();
        let owner = item.owner_id;
        let fences = online::capture_online_fences(&c.world, &[owner]).await;
        let value = if let Action::Set(_, value) = action { Some(value) } else { None };
        let reset = matches!(action, Action::Reset(_));
        let client = &*c;
        (written_state, wrote) = db.transaction(|tx, outbox: &mut Outbox<OnlineChanged>| {
            let sde = sde.clone();
            let fences = fences.clone();
            Box::pin(async move {
                let mut bound = None;
                if let Some(hull) = cascade_ship {
                    if !crate::dao::items::ship::lock_reload_ship(tx, hull).await.map_err(db_err)? {
                        return Err(slash_error(messages::gm::GM_DOGMA_FORBIDDEN_TARGET.into()));
                    }
                    // The locked witness supplies both the target row and its
                    // persisted override state; capacity mutations use it in
                    // place of a second plain item read.
                    if capacity_mutation {
                        let row = dao::hull_override(tx, id, attr).await.map_err(db_err)?
                            .ok_or_else(|| slash_error(messages::gm::GM_DOGMA_FORBIDDEN_TARGET.into()))?;
                        if row.item.owner_id != owner || !row.item.singleton || row.item.quantity != 1
                            || sde.types.get(row.item.type_id).and_then(|ty| ty.category_id) != Some(infinity_core::consts::CATEGORY_SHIP) {
                            return Err(slash_error(messages::gm::GM_DOGMA_UNLIMITED_TARGET.into()));
                        }
                        bound = Some(row);
                    } else {
                        let target = load_items(tx, &sde, ItemQuery::Id(id)).await.map_err(db_err)?.into_iter().next()
                            .ok_or_else(|| slash_error(messages::gm::GM_DOGMA_FORBIDDEN_TARGET.into()))?;
                        if target.owner_id != owner || !target.singleton || (id != hull && (target.location_id != hull || !crate::services::inventory::is_fitting_flag(target.flag_id))) {
                            return Err(slash_error(messages::gm::GM_DOGMA_FORBIDDEN_TARGET.into()));
                        }
                        bound = None;
                    }
                }
                // No-ops are decided from the locked witness before any fit
                // capture; the bool tells the outer block to skip publication.
                let locked_state = bound.as_ref().and_then(|row| row.state);
                let locked_type = bound.as_ref().map(|row| row.item.type_id).unwrap_or_default();
                let unchanged = capacity_mutation && if unlimited { locked_state.is_some_and(|state| state.cargo_unlimited) }
                    else if reset { locked_state.is_none() }
                    else if let Some(value) = value { locked_state == Some(dao::OverrideState { value, cargo_unlimited: false }) }
                    else { false };
                if unchanged { return Ok((locked_state, false)); }
                // Every cascaded hull's before-fit is captured before the
                // override write, so the committed delta carries this change;
                // capacity mutations reuse the same boundary, not a second one.
                let before = match cascade_ship {
                    Some(hull) => Some(online::fitting::capture(tx, &sde, &fences, owner, hull).await?),
                    None => None,
                };
                let new_state = if let Some(value) = value {
                    if !dao::set_override(tx, id, attr, value).await.map_err(db_err)? {
                        return Err(slash_error(messages::gm::GM_DOGMA_SINGLETON.into()));
                    }
                    if capacity_mutation { Some(dao::OverrideState { value, cargo_unlimited: false }) } else { None }
                } else if unlimited {
                    // An existing finite row keeps its real base; only a fresh
                    // row starts from the type default.
                    let base = locked_state.map(|state| state.value)
                        .unwrap_or_else(|| fit::base_attribute(sde.types.get(locked_type), sde.dogma.attribute(attr), attr));
                    if !dao::set_cargo_unlimited(tx, id, base).await.map_err(db_err)? {
                        return Err(slash_error(messages::gm::GM_DOGMA_SINGLETON.into()));
                    }
                    Some(dao::OverrideState { value: base, cargo_unlimited: true })
                } else {
                    dao::clear_override(tx, id, attr).await.map_err(db_err)?;
                    None
                };
                if let (Some(hull), Some(before)) = (cascade_ship, before) {
                    let fit = online::reconcile_core(tx, &sde, outbox, &fences, owner, hull).await?;
                    online::fitting::finish(tx, before, fit, outbox).await?;
                }
                Ok((new_state, true))
            })
        }, |result, outbox| async move {
            let result = online::dispatch_changed(result, outbox, client)?;
            if result.1 {
                // Only a committed write publishes the drone's own override
                // patch; the captured record generation pins the live instance.
                if let Some((system, owner, drone_generation, membership)) = drone_token {
                    let patch = crate::services::fit::attributes::CommittedFitting::overridden_attributes(
                        vec![(Key::Drone(id), attr, value)]);
                    client.world.post(system, move |s| {
                        s.publish_drone_fitting(id, owner, drone_generation, membership, patch);
                    });
                }
            }
            Ok(result)
        }).await?;
        if wrote && key.is_none() {
            if let Some((hull, (system, generation))) = cascade_ship.and_then(|hull| fences.membership(hull).map(|fence| (hull, fence))) {
                c.world.post(system, move |s| {
                    if s.combat_membership_generation(hull) == Some(generation) { s.refresh_projection_target(hull); }
                });
            }
        }
    }
    // Only a committed write re-reads report values and republishes the brain;
    // a locked-witness no-op skips them. Live targets re-read through the same
    // actor after the queued patch applied, so no fit leaves the actor and the
    // fitting publisher's own notices and kinematics are not duplicated.
    let (new_live, new_brain) = if wrote && live_mutation {
        let (system, ship, key) = (fit_system.unwrap(), fit_ship.unwrap(), key.unwrap());
        let republish = !capacity_mutation && fit_pilot == char_id && key == Key::Ship;
        let read = c.world.with(system, move |s| s.with_combat_fit(ship, move |fit, _, _| {
            let brain = (republish && fit.type_has_attribute(key, attr))
                .then(|| crate::services::dogma::brain::compile(fit, char_id, ship));
            (fit.base(key, attr), fit.value(key, attr), brain)
        })).await.flatten();
        match read {
            Some((base, effective, brain)) => (Some((base, effective)), brain),
            None => (None, None),
        }
    } else { (None, None) };
    if let Some((character, ship, structure)) = new_brain {
        c.notify(crate::services::dogma::brain::UPDATED_EVENT,
            crate::services::dogma::brain::update_args(char_id, character, ship, structure));
    }
    let new_fit = if wrote && key.is_some() && !live_mutation {
        Some(Fit::load(&c.db, c.sde.clone(), fit_pilot, fit_ship.unwrap()).await?)
    } else { None };
    let current_fit = new_fit.as_ref().or(old_fit.as_ref());
    let new_base = new_live.map(|(base, _)| base)
        .or_else(|| current_fit.zip(key).map(|(fit, key)| fit.base(key, attr)))
        .unwrap_or(match action { Action::Set(_, value) => value, Action::Reset(_) => fallback, _ => old_base });
    let new_effective = new_live.map(|(_, effective)| effective)
        .or_else(|| current_fit.zip(key).map(|(fit, key)| fit.value(key, attr)));
    if wrote && !capacity_mutation {
        if let (Some(old), Some(new), Some(key)) = (old_fit.as_ref(), new_fit.as_ref(), key) {
            let before = propulsion::kinematics(old, Key::Ship);
            let after = propulsion::kinematics(new, Key::Ship);
            if before != after {
                if let (Some(system), Some(ship), Some(revision)) = (fit_system, fit_ship, new.projection_revision()) {
                    c.world.post(system, move |s| {
                        if current_projection(s, ship, revision) {
                            s.queue_projection_kinematics(ship, revision, after);
                        }
                    });
                }
            }
            let entry = notify::attribute_change(fit_pilot, Value::Int(id), attr, Value::Float(new.wire_value(key, attr)), Value::Float(old.wire_value(key, attr)));
            c.world.notify_pilot(fit_pilot, "OnModuleAttributeChanges", vec![Value::list([entry])]);
            if fit_pilot == char_id && key == Key::Ship && new.type_has_attribute(key, attr) { crate::services::dogma::brain::update_from_fit(c, new, char_id, fit_ship.unwrap()); }
        }
    }
    // A capacity mutation reports the transaction's locked-witness state;
    // other attributes keep the rows read before the write.
    let cargo_mode = attr == infinity_core::consts::dogma::ATTR_CAPACITY && if capacity_mutation {
        written_state.is_some_and(|state| state.cargo_unlimited)
    } else {
        rows.iter().any(|r| r.attribute_id == infinity_core::consts::dogma::ATTR_CAPACITY && r.cargo_unlimited)
    };
    let overridden = match action {
        Action::Set(..) => true,
        Action::Unlimited(_) => cargo_mode,
        Action::Reset(_) => false,
        _ => old_override.is_some(),
    };
    let mut result = messages::gm::gm_dogma_result(id, attr_name, attr, old_base, new_base, old_effective.zip(new_effective), overridden, cargo_mode);
    if cargo_mode { result.push_str("\n"); result.push_str(messages::gm::GM_DOGMA_UNLIMITED); }
    if key.is_none() { result.push_str("\n"); result.push_str(messages::gm::GM_DOGMA_BASE_ONLY); }
    let hull_foreign = item.category_id == i64::from(infinity_core::consts::CATEGORY_SHIP)
        && attr != infinity_core::consts::dogma::ATTR_CAPACITY
        && c.sde.types.get(item.type_id).is_none_or(|data| data.attribute(attr).is_none());
    if item.category_id == i64::from(CATEGORY_MODULE) || hull_foreign {
        result.push_str("\n"); result.push_str(messages::gm::GM_DOGMA_VISIBILITY);
    }
    Ok(result)
}

#[cfg(all(test, feature = "tests-slash"))]
mod tests;
