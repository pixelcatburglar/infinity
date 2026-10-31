//! Shared fitting, onlining, activation and resource-cascade policy.
//! Mirrors `shipfitting/fittingStuff.py`, `dogma/online.py`, and
//! `dogma/baseDogmaLocation.py:1120-1177`. Effective dogma values come from
//! Fit and fitted item rows from the game database. Structure fitting is not supported.

use std::{collections::HashSet, sync::Arc};
use marshal::Value;
use crate::{consts::*, dao::items::fit::{self as fit_dao, Module}, db::executor::Executor, messages, sde::Sde};
use super::super::{db_err, notify_error, user_error, CallError, fit::{Fit, Key}};

/// Client dogma effectSubSystem.
const EFFECT_SUBSYSTEM: i32 = 3772;
/// Client dogma rack effects and slot-count attributes (rigSlots is not upgradeSlotsLeft).
const RACKS: [(i32, i32, i32); 5] = [(11, 12, FLAG_FITTING_FIRST), (13, 13, FLAG_MED_SLOT_FIRST), (12, 14, FLAG_HI_SLOT_FIRST), (EFFECT_RIG_SLOT, 1137, FLAG_RIG_FIRST), (EFFECT_SUBSYSTEM, 1367, FLAG_SUBSYSTEM_FIRST)];
/// Client inventory rack width.
const RACK_WIDTH: i32 = 8;
/// Client visible subsystem slots.
const VISIBLE_SUBSYSTEMS: i32 = 4;
/// Client inventory service rack, used in cascade ordering.
const SERVICE_FIRST: i32 = 164;
/// Client fitting resource tolerance, online.py.
const ONLINE_TOLERANCE: f64 = 1e-6;
/// Client cascade ratio denominator, baseDogmaLocation.py.
const RATIO_FLOOR: f64 = 1e-7;
/// Client UserError typed value tags.
const UE_TYPEID: i64 = 4;
/// Client eveexceptions.const.UE_GROUPID.
const UE_GROUPID: i64 = 7;
/// Client dogma attributes: restrictions, limits, sizes, and subsystem slot.
const CAN_FIT_GROUPS: [i32; 20] = [1298,1299,1300,1301,1872,1879,1880,1881,2065,2396,2476,2477,2478,2479,2480,2481,2482,2483,2484,2485];
/// Client dogma.const canFitShipType1 through canFitShipType12.
const CAN_FIT_TYPES: [i32; 12] = [1302,1303,1304,1305,1944,2103,2463,2486,2487,2488,2758,5948];
/// Client dogma.const.attributeMaxGroupFitted.
const MAX_GROUP_FITTED: i32 = 1544;
/// Client dogma.const.attributeMaxTypeFitted.
const MAX_TYPE_FITTED: i32 = 2431;
/// Client dogma.const.attributeMaxGroupOnline.
const MAX_GROUP_ONLINE: i32 = 978;
/// Client dogma.const.attributeMaxGroupActive.
const MAX_GROUP_ACTIVE: i32 = 763;
/// Client dogma.const.attributeMaxShipGroupActiveID.
const MAX_SHIP_GROUP_ACTIVE_ID: i32 = 909;
/// Client dogma.const.attributeMaxShipGroupActive.
const MAX_SHIP_GROUP_ACTIVE: i32 = 910;
/// Client dogma.const.attributeRigSize.
const RIG_SIZE: i32 = 1547;
/// Client dogma.const.attributeSubSystemSlot.
const SUBSYSTEM_SLOT: i32 = 1366;
/// Client dogma attributeFitsToShipType, used by subsystems.
const FITS_TO_SHIP_TYPE: i32 = 1380;

fn typed(tag: i64, id: i32) -> Value { Value::tuple([Value::Int(tag), Value::Int(i64::from(id))]) }
fn int(value: i32) -> Value { Value::Int(i64::from(value)) }
fn has_effect(sde: &Sde, ty: i32, effect: i32) -> bool { sde.types.effects(ty).iter().any(|e| e.effect_id == effect) }
fn group(sde: &Sde, ty: i32) -> i32 { sde.types.get(ty).and_then(|t| t.group_id).unwrap_or(0) }
fn refusal(msg: &'static str) -> CallError { notify_error(msg) }
fn rack(sde: &Sde, ty: i32) -> Option<(i32, i32, i32)> { RACKS.iter().copied().find(|r| has_effect(sde, ty, r.0)) }
fn free_rack_slot(first: i32, count: i32, used: impl Fn(i32) -> bool) -> Option<i32> {
    (first..first + count.clamp(0, RACK_WIDTH)).find(|&flag| !used(flag))
}

fn skills(fit: &Fit, sde: &Sde, item: i64, ty: i32, online: bool) -> Result<(), CallError> {
    let key = Key::Module(item);
    let missing: Vec<_> = ATTR_REQUIRED_SKILLS.iter().zip(ATTR_REQUIRED_SKILL_LEVELS).filter_map(|(&skill_attr, level_attr)| {
        let skill = fit.value(key, skill_attr) as i32;
        let level = fit.value(key, level_attr) as i32;
        (skill > 0 && fit.value(Key::Skill(skill), ATTR_SKILL_LEVEL) < f64::from(level)).then(|| {
            (sde.types.get(skill).and_then(|d| d.name.clone()).unwrap_or_else(|| skill.to_string()), level)
        })
    }).collect();
    if missing.is_empty() { return Ok(()); }
    Err(user_error(if online { messages::MSG_ONLINE_HAS_SKILL_PREREQUISITES } else { messages::MSG_FITTING_HAS_SKILL_PREREQUISITES }, vec![
        ("requiredSkills".into(), Value::unicode(messages::fitting::required_skills(&missing))),
        ("item".into(), int(ty)), ("skillCount".into(), Value::Int(missing.len() as i64)),
    ]))
}

fn structural(fit: &Fit, sde: &Sde, modules: &[Module], item: i64, ty: i32, flag: Option<i32>, online: bool) -> Result<(), CallError> {
    let key = Key::Module(item);
    let ship_type = fit.source_type(Key::Ship).unwrap_or(0);
    let ship_data = sde.types.get(ship_type);
    let data = sde.types.get(ty);
    if ship_data.and_then(|d| d.category_id) != Some(CATEGORY_SHIP) || !matches!(data.and_then(|d| d.category_id), Some(CATEGORY_MODULE | CATEGORY_SUBSYSTEM)) {
        return Err(refusal(messages::fitting::CATEGORY));
    }
    if data.and_then(|d| d.category_id) == Some(CATEGORY_SUBSYSTEM) {
        let valid_ship = fit.value(key, FITS_TO_SHIP_TYPE) as i32;
        if valid_ship > 0 && valid_ship != ship_type {
            return Err(user_error(messages::MSG_CANNOT_FIT_SUBSYSTEM_NOT_SHIP, vec![
                ("subSystemName".into(), typed(UE_TYPEID, ty)),
                ("validShipName".into(), typed(UE_TYPEID, valid_ship)),
                ("shipName".into(), typed(UE_TYPEID, ship_type)),
            ]));
        }
    }
    let ship_group = group(sde, ship_type);
    let mut restricted = false;
    let mut allowed = false;
    for (&attr, expected) in CAN_FIT_GROUPS.iter().map(|a| (a, ship_group)).chain(CAN_FIT_TYPES.iter().map(|a| (a, ship_type))) {
        let value = fit.value(key, attr) as i32;
        if value > 0 { restricted = true; allowed |= value == expected; }
    }
    if restricted && !allowed { return Err(refusal(messages::fitting::HULL)); }
    skills(fit, sde, item, ty, online)?;
    let Some((effect, slots_attr, first)) = rack(sde, ty) else {
        return Err(user_error(messages::MSG_MODULE_NOT_POWERED, vec![("type".into(), int(ty))]));
    };
    let count = (fit.value(Key::Ship, slots_attr) as i32).clamp(0, RACK_WIDTH);
    if count == 0 { return Err(user_error(messages::MSG_MODULE_NOT_POWERED, vec![("type".into(), int(ty))])); }
    if effect == EFFECT_SUBSYSTEM && fit.value(key, SUBSYSTEM_SLOT) as i32 >= first + VISIBLE_SUBSYSTEMS && count <= VISIBLE_SUBSYSTEMS {
        return Err(user_error(messages::MSG_NO_SUITABLE_SLOTS_FOR_THIS_SUBSYSTEM, vec![("subSystemName".into(), typed(UE_TYPEID, ty))]));
    }
    if let Some(flag) = flag {
        if flag < first || flag >= first + count { return Err(refusal(messages::fitting::RACK)); }
        if effect == EFFECT_SUBSYSTEM {
            let desired = fit.value(key, SUBSYSTEM_SLOT) as i32;
            if desired < first + VISIBLE_SUBSYSTEMS && flag != desired { return Err(refusal(messages::fitting::RACK)); }
            if desired >= first + VISIBLE_SUBSYSTEMS && flag < first + VISIBLE_SUBSYSTEMS { return Err(refusal(messages::fitting::RACK)); }
        }
        if modules.iter().any(|m| m.item_id != item && m.flag_id == flag) { return Err(refusal(messages::fitting::OCCUPIED)); }
    }
    if has_effect(sde, ty, EFFECT_TURRET_FITTED) && fit.value(Key::Ship, ATTR_TURRET_SLOTS_LEFT) < 0.0 { return Err(refusal(messages::fitting::TURRET)); }
    if has_effect(sde, ty, EFFECT_LAUNCHER_FITTED) && fit.value(Key::Ship, ATTR_LAUNCHER_SLOTS_LEFT) < 0.0 { return Err(refusal(messages::fitting::LAUNCHER)); }
    if effect == EFFECT_RIG_SLOT {
        let size = fit.value(Key::Ship, RIG_SIZE);
        if size != 0.0 && size != fit.value(key, RIG_SIZE) { return Err(refusal(messages::fitting::RIG_SIZE)); }
        if fit.value(Key::Ship, ATTR_UPGRADE_LOAD) > fit.value(Key::Ship, ATTR_UPGRADE_CAPACITY) { return Err(refusal(messages::fitting::CALIBRATION)); }
    }
    let group_id = group(sde, ty);
    let others: Vec<_> = modules.iter().filter(|m| m.item_id != item).collect();
    for (attr, by_group) in [(MAX_GROUP_FITTED, true), (MAX_TYPE_FITTED, false)] {
        let limit = fit.value(key, attr) as i32;
        let used = others.iter().filter(|m| if by_group { group(sde, m.type_id) == group_id } else { m.type_id == ty }).count();
        if limit > 0 && used >= limit as usize {
            let mut args = vec![("ship".into(), int(ship_type)), ("module".into(), int(ty)), ("noOfModules".into(), int(limit)), ("noOfModulesFitted".into(), Value::Int(used as i64))];
            if by_group { args.push(("groupName".into(), Value::unicode(sde.types.group(group_id).and_then(|g| g.name.as_deref()).unwrap_or("")))); }
            return Err(user_error(if by_group { messages::MSG_CANT_FIT_TOO_MANY_BY_GROUP } else { messages::MSG_CANT_FIT_TOO_MANY_BY_TYPE }, args));
        }
    }
    Ok(())
}

/// Select a slot without mutating the inventory.
pub(crate) async fn free_slot(ex: &(impl Executor + ?Sized), sde: Arc<Sde>, owner: i64, ship: i64, item: i64, type_id: i32) -> Result<Option<i32>, CallError> {
    let mut fit = Fit::load(ex, sde.clone(), owner, ship).await?;
    fit.include_module(ex, item, type_id).await?;
    let modules = fit_dao::modules(ex, ship).await.map_err(db_err)?;
    structural(&fit, &sde, &modules, item, type_id, None, false)?;
    let Some((effect, attr, first)) = rack(&sde, type_id) else { return Ok(None) };
    let count = (fit.value(Key::Ship, attr) as i32).clamp(0, RACK_WIDTH);
    let desired = fit.value(Key::Module(item), SUBSYSTEM_SLOT) as i32;
    let start = if effect == EFFECT_SUBSYSTEM && desired >= first + VISIBLE_SUBSYSTEMS { first + VISIBLE_SUBSYSTEMS } else if effect == EFFECT_SUBSYSTEM { desired } else { first };
    let end = if effect == EFFECT_SUBSYSTEM && desired < first + VISIBLE_SUBSYSTEMS { (desired + 1).min(first + count) } else { first + count };
    Ok(free_rack_slot(start, (end - start).max(0), |flag| modules.iter().any(|m| m.item_id != item && m.flag_id == flag)))
}

/// Validate the actual fitted candidate inside the caller's transaction.
pub(crate) async fn validate_fit(ex: &(impl Executor + ?Sized), sde: Arc<Sde>, owner: i64, ship: i64, item: i64) -> Result<(), CallError> {
    let fit = Fit::load(ex, sde.clone(), owner, ship).await?;
    let modules = fit_dao::modules(ex, ship).await.map_err(db_err)?;
    let m = modules.iter().find(|m| m.item_id == item).ok_or_else(|| CallError::Internal(anyhow::anyhow!("fitted item missing")))?;
    structural(&fit, &sde, &modules, item, m.type_id, Some(m.flag_id), false)
}

/// Validate a prospective online state without writing it.
pub(crate) async fn validate_online(ex: &(impl Executor + ?Sized), sde: Arc<Sde>, owner: i64, ship: i64, item: i64) -> Result<(), CallError> {
    let mut fit = Fit::load(ex, sde.clone(), owner, ship).await?;
    let modules = fit_dao::modules(ex, ship).await.map_err(db_err)?;
    let m = modules.iter().find(|m| m.item_id == item).ok_or_else(|| CallError::Internal(anyhow::anyhow!("fitted item missing")))?;
    structural(&fit, &sde, &modules, item, m.type_id, Some(m.flag_id), true)?;
    fit.set_module_online(item, false);
    let old_cpu = fit.value(Key::Ship, ATTR_CPU_LOAD);
    let old_power = fit.value(Key::Ship, ATTR_POWER_LOAD);
    fit.set_module_online(item, true);
    for (need_attr, load_attr, output_attr, old, msg) in [(ATTR_CPU, ATTR_CPU_LOAD, ATTR_CPU_OUTPUT, old_cpu, messages::MSG_NOT_ENOUGH_CPU), (ATTR_POWER, ATTR_POWER_LOAD, ATTR_POWER_OUTPUT, old_power, messages::MSG_NOT_ENOUGH_POWER)] {
        let need = fit.value(Key::Module(item), need_attr);
        let output = fit.value(Key::Ship, output_attr);
        if resource_exceeded(need, fit.value(Key::Ship, load_attr), output) {
            return Err(user_error(msg, vec![("moduleName".into(), typed(UE_TYPEID, m.type_id)), ("moduleType".into(), int(m.type_id)), ("require".into(), Value::Float(need)), ("remaining".into(), Value::Float(output - old)), ("total".into(), Value::Float(output))]));
        }
    }
    let gid = group(&sde, m.type_id);
    let same = modules.iter().filter(|other| group(&sde, other.type_id) == gid);
    let limit = same.clone().filter(|other| fit.type_has_attribute(Key::Module(other.item_id), MAX_GROUP_ONLINE)).map(|other| fit.value(Key::Module(other.item_id), MAX_GROUP_ONLINE) as i32).min();
    let used = same.filter(|other| other.item_id != item && other.online).count();
    if let Some(limit) = limit { if used as i64 >= i64::from(limit) {
        return Err(user_error(messages::MSG_CANNOT_ONLINE_REACHED_MAX_GROUP_ONLINE, vec![("type".into(), typed(UE_TYPEID, m.type_id)), ("group".into(), typed(UE_GROUPID, gid)), ("maxGroupOnline".into(), int(limit))]));
    }}
    Ok(())
}

/// Validate a bank-expanded activation against the current active set.
pub(crate) async fn validate_active(ex: &(impl Executor + ?Sized), sde: Arc<Sde>, fit: &Fit, ship: i64, active: &[i64], activating: &[i64]) -> Result<(), CallError> {
    let modules = fit_dao::modules(ex, ship).await.map_err(db_err)?;
    let mut running: HashSet<_> = active.iter().copied().collect();
    for &item in activating {
        if running.contains(&item) { continue; }
        let Some(module) = modules.iter().find(|m| m.item_id == item) else { return Err(CallError::Internal(anyhow::anyhow!("activating item missing"))); };
        let gid = group(&sde, module.type_id);
        let mut limit = fit.value(Key::Module(item), MAX_GROUP_ACTIVE);
        if fit.value(Key::Ship, MAX_SHIP_GROUP_ACTIVE_ID) as i32 == gid { limit = limit.max(fit.value(Key::Ship, MAX_SHIP_GROUP_ACTIVE)); }
        let count = modules.iter().filter(|m| running.contains(&m.item_id) && group(&sde, m.type_id) == gid).count();
        if limit > 0.0 && count as f64 >= limit { return Err(user_error(messages::MSG_EFFECT_CROWDED_OUT, vec![("module".into(), int(module.type_id)), ("count".into(), Value::Int(count as i64))])); }
        running.insert(item);
    }
    Ok(())
}
fn resource_exceeded(need: f64, load: f64, output: f64) -> bool {
    need != 0.0 && load > output + ONLINE_TOLERANCE
}


fn worse_resource(cpu_load: f64, cpu_output: f64, power_load: f64, power_output: f64) -> Option<i32> {
    let cpu = cpu_load / cpu_output.max(RATIO_FLOOR);
    let power = power_load / power_output.max(RATIO_FLOOR);
    if cpu <= 1.0 && power <= 1.0 { None } else if power > cpu { Some(ATTR_POWER) } else { Some(ATTR_CPU) }
}
fn most_resource(candidates: impl Iterator<Item = (bool, f64, i64)>) -> Option<i64> {
    candidates.filter(|candidate| candidate.1 != 0.0)
        .max_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.total_cmp(&b.1)).then_with(|| a.2.cmp(&b.2)))
        .map(|entry| entry.2)
}


/// Pick one online item to offline; caller reloads the fit after each change.
pub(crate) async fn overload_candidate(ex: &(impl Executor + ?Sized), sde: Arc<Sde>, owner: i64, ship: i64) -> Result<Option<i64>, CallError> {
    let fit = Fit::load(ex, sde, owner, ship).await?;
    let Some(attr) = worse_resource(fit.value(Key::Ship, ATTR_CPU_LOAD), fit.value(Key::Ship, ATTR_CPU_OUTPUT), fit.value(Key::Ship, ATTR_POWER_LOAD), fit.value(Key::Ship, ATTR_POWER_OUTPUT)) else { return Ok(None) };
    let modules = fit_dao::modules(ex, ship).await.map_err(db_err)?;
    Ok(most_resource(modules.iter().filter(|m| m.online).map(|m| {
        (!(SERVICE_FIRST..SERVICE_FIRST + RACK_WIDTH).contains(&m.flag_id), fit.value(Key::Module(m.item_id), attr), m.item_id)
    })))
}

#[cfg(test)] mod tests;
