//! Per-controller-character runtime copy of the client's drone preferences, cleared on logout.
//! Until the first sync, drones are passive and unfocused. The client resends settings
//! before launch (menusvc:3613-3615); a partial first update uses SDE defaults.
//! `environment/godma.py:2342-2363` sends a numeric attribute-ID to bool dict
//! and ignores the return; `ui/inflight/drones/droneSettings.py:8-25` uses
//! CharSettingBool with SDE attribute defaults and sends individual changes.
use std::{collections::HashMap, sync::{Mutex, MutexGuard, OnceLock}};
use marshal::Value;
use infinity_core::{consts::{dogma::ATTR_DRONE_FOCUS_FIRE, dogma::ATTR_DRONE_IS_AGGRESSIVE}};
use crate::{sde::Sde, services::{CallError, notify_error}};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct DroneSettings {
    pub aggressive: bool,
    pub focus_fire: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Patch {
    pub aggressive: Option<bool>,
    pub focus_fire: Option<bool>,
}

#[derive(Default)]
struct State { owners: HashMap<i64, DroneSettings> }

fn state() -> MutexGuard<'static, State> {
    static STATE: OnceLock<Mutex<State>> = OnceLock::new();
    STATE.get_or_init(Default::default).lock().unwrap()
}

pub(crate) fn parse(args: &[Value]) -> Result<Patch, CallError> {
    let invalid = || notify_error(infinity_core::messages::drones::INVALID_COMMAND);
    let [Value::Dict(entries)] = args else { return Err(invalid()); };
    let mut patch = Patch::default();
    for (key, value) in entries {
        let Value::Bool(value) = value else { return Err(invalid()); };
        let slot = match key {
            Value::Int(id) if *id == i64::from(ATTR_DRONE_IS_AGGRESSIVE) => &mut patch.aggressive,
            Value::Int(id) if *id == i64::from(ATTR_DRONE_FOCUS_FIRE) => &mut patch.focus_fire,
            _ => return Err(invalid()),
        };
        if slot.replace(*value).is_some() { return Err(invalid()); }
    }
    Ok(patch)
}

pub(crate) fn defaults(sde: &Sde) -> Result<DroneSettings, CallError> {
    let default = |id| {
        let value = sde.dogma.attribute(id).and_then(|a| a.default_value)
            .ok_or_else(|| CallError::Internal(anyhow::anyhow!("missing drone setting default for attribute {id}")))?;
        if !value.is_finite() { return Err(CallError::Internal(anyhow::anyhow!("invalid drone setting default for attribute {id}"))); }
        Ok(value != 0.0)
    };
    Ok(DroneSettings { aggressive: default(ATTR_DRONE_IS_AGGRESSIVE)?, focus_fire: default(ATTR_DRONE_FOCUS_FIRE)? })
}

pub(crate) fn apply(owner: i64, defaults: DroneSettings, patch: Patch) -> DroneSettings {
    let mut state = state();
    let settings = state.owners.entry(owner).or_insert(defaults);
    if let Some(value) = patch.aggressive { settings.aggressive = value; }
    if let Some(value) = patch.focus_fire { settings.focus_fire = value; }
    *settings
}

pub(crate) fn current(owner: i64) -> DroneSettings {
    state().owners.get(&owner).copied().unwrap_or_default()
}

pub(crate) fn logged_off(owner: i64) {
    state().owners.remove(&owner);
}

#[cfg(test)]
mod tests;
