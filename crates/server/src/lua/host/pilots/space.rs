//! Spatial operations for simulated pilots through the normal ship and combat services.
//! State comes from character and ship DAOs and the system actor; capacitor is runtime-only.
//! Space-entry environment initialization reuses the normal entry rule; docked pilots have no in-space capacitor snapshot.

use mlua::{Lua, Table, Value};
use crate::{messages, services::{self, CallError, identity::Identity, ship}, space::V3};
use super::{lifecycle::{self, OnlinePilot}, lua_error, mutate, positive, Resources};

pub(super) async fn actual_system(r: &Resources, identity: &Identity) -> Option<i64> {
    if identity.station_id.is_some() || !r.world.is_online(identity.char_id) { return None; }
    let (system, ship) = (identity.system_id?, identity.ship_id?);
    let id = identity.char_id;
    r.world.with(system, move |s| s.pilot_ball(id).is_some_and(|ball| ball.id == ship)).await
        .filter(|matched| *matched).map(|_| system)
}

fn apply(r: &Resources, identity: &Identity, pilot: &mut OnlinePilot, change: ship::SessionChange) -> Result<(), CallError> {
    let previous = pilot.session.clone();
    let mut current = previous.clone();
    for (key, value) in change.updates {
        match (key, value) {
            ("stationid", marshal::Value::None) => current.station_id = None,
            ("stationid", marshal::Value::Int(id)) => current.station_id = Some(id),
            ("solarsystemid", marshal::Value::None) => {},
            ("solarsystemid", marshal::Value::Int(id)) => current.system_id = Some(id),
            (key, _) => return Err(CallError::Internal(anyhow::anyhow!("unexpected spatial session update {key}"))),
        }
    }
    services::presence::online(identity, &r.world, &r.pilots.hub, pilot.notify.clone(), Some(&previous), current.clone());
    pilot.session = current;
    if let Some(event) = change.event { crate::lua::on_event(event); }
    Ok(())
}

fn point(table: Table) -> mlua::Result<V3> {
    for entry in table.clone().pairs::<Value, Value>() {
        let (key, _) = entry?;
        let Value::String(key) = key else { return Err(mlua::Error::runtime("unexpected coordinate")); };
        if !["x", "y", "z"].iter().any(|name| key.as_bytes().as_ref() == name.as_bytes()) {
            return Err(mlua::Error::runtime("unexpected coordinate"));
        }
    }
    let point: V3 = [table.raw_get("x")?, table.raw_get("y")?, table.raw_get("z")?];
    if !point.iter().all(|v| v.is_finite()) { return Err(mlua::Error::runtime("invalid coordinate")); }
    Ok(point)
}

pub(super) fn register(lua: &Lua, api: &Table, resources: &Resources) -> mlua::Result<()> {
    let r = resources.clone();
    api.set("undock", lua.create_async_function(move |_, id: i64| {
        let r = r.clone(); async move {
            positive(id)?;
            mutate(async move {
                let (mut state, row) = lifecycle::locked(&r, id).await?;
                let pilot = state.as_mut().ok_or_else(|| services::notify_error(messages::lua::PILOT_OFFLINE))?;
                let identity = lifecycle::snapshot(&r, id, &row).await?;
                let system = actual_system(&r, &identity).await;
                let result = ship::ship::undock(&identity, &r.db, &r.world, &r.sde, system, pilot.session.alliance_id).await?;
                apply(&r, &identity, pilot, result.change)?;
                drop(result.economy);
                lifecycle::entered_space(&r, &pilot.session).await?;
                Ok(())
            }).await.map_err(lua_error)
        }
    })?)?;
    let r = resources.clone();
    api.set("dock", lua.create_async_function(move |_, (id, station): (i64, i64)| {
        let r = r.clone(); async move {
            positive(id)?; positive(station)?;
            mutate(async move {
                let (mut state, row) = lifecycle::locked(&r, id).await?;
                let pilot = state.as_mut().ok_or_else(|| services::notify_error(messages::lua::PILOT_OFFLINE))?;
                let identity = lifecycle::snapshot(&r, id, &row).await?;
                let system = actual_system(&r, &identity).await;
                let change = ship::dock(&identity, &r.db, &r.world, system, station, None::<&mut crate::client::Client>).await?;
                apply(&r, &identity, pilot, change)
            }).await.map_err(lua_error)
        }
    })?)?;
    let r = resources.clone();
    api.set("stop", lua.create_async_function(move |_, id: i64| {
        let r = r.clone(); async move {
            positive(id)?;
            mutate(async move {
                let (state, row) = lifecycle::locked(&r, id).await?;
                state.as_ref().ok_or_else(|| services::notify_error(messages::lua::PILOT_OFFLINE))?;
                let identity = lifecycle::snapshot(&r, id, &row).await?;
                let system = actual_system(&r, &identity).await;
                ship::beyonce::stop(&identity, &r.world, system)?; Ok(())
            }).await.map_err(lua_error)
        }
    })?)?;
    let r = resources.clone();
    api.set("orbit", lua.create_async_function(move |_, (id, target, range): (i64, i64, f64)| {
        let r = r.clone(); async move {
            positive(id)?; positive(target)?;
            if !range.is_finite() || range < 0.0 || !(range as f32).is_finite() { return Err(mlua::Error::runtime("invalid orbit range")); }
            let range = range as f32;
            mutate(async move {
                let (state, row) = lifecycle::locked(&r, id).await?;
                state.as_ref().ok_or_else(|| services::notify_error(messages::lua::PILOT_OFFLINE))?;
                let identity = lifecycle::snapshot(&r, id, &row).await?;
                let system = actual_system(&r, &identity).await;
                ship::beyonce::orbit(&identity, &r.world, system, target, range)?; Ok(())
            }).await.map_err(lua_error)
        }
    })?)?;
    let r = resources.clone();
    api.set("approach", lua.create_async_function(move |_, (id, target): (i64, i64)| {
        let r = r.clone(); async move {
            positive(id)?; positive(target)?;
            mutate(async move {
                let (state, row) = lifecycle::locked(&r, id).await?;
                state.as_ref().ok_or_else(|| services::notify_error(messages::lua::PILOT_OFFLINE))?;
                let identity = lifecycle::snapshot(&r, id, &row).await?;
                let system = actual_system(&r, &identity).await;
                ship::beyonce::follow_ball(&identity, &r.world, system, target, 0.0)?; Ok(())
            }).await.map_err(lua_error)
        }
    })?)?;
    let r = resources.clone();
    api.set("warp_to", lua.create_async_function(move |_, (id, coordinates): (i64, Table)| {
        let r = r.clone(); async move {
            positive(id)?; let point = point(coordinates)?;
            mutate(async move {
                let (state, row) = lifecycle::locked(&r, id).await?;
                state.as_ref().ok_or_else(|| services::notify_error(messages::lua::PILOT_OFFLINE))?;
                let identity = lifecycle::snapshot(&r, id, &row).await?;
                let system = actual_system(&r, &identity).await;
                ship::warp_to(&identity, &r.db, &r.world, system, None, point, 0.0, None).await?; Ok(())
            }).await.map_err(lua_error)
        }
    })?)?;
    let r = resources.clone();
    api.set("warp_to_ball", lua.create_async_function(move |_, (id, target): (i64, i64)| {
        let r = r.clone(); async move {
            positive(id)?; positive(target)?;
            mutate(async move {
                let (state, row) = lifecycle::locked(&r, id).await?;
                state.as_ref().ok_or_else(|| services::notify_error(messages::lua::PILOT_OFFLINE))?;
                let identity = lifecycle::snapshot(&r, id, &row).await?;
                let system = actual_system(&r, &identity).await;
                ship::beyonce::warp_to_ball(&identity, &r.db, &r.world, &r.sde, system, r.pilots.account, target, false, 0.0, None).await?; Ok(())
            }).await.map_err(lua_error)
        }
    })?)?;
    let r = resources.clone();
    api.set("condition", lua.create_async_function(move |lua, id: i64| {
        let r = r.clone(); async move {
            positive(id)?;
            let (_state, row) = lifecycle::locked(&r, id).await.map_err(lua_error)?;
            let identity = lifecycle::snapshot(&r, id, &row).await.map_err(lua_error)?;
            let Some(ship) = identity.ship_id else { return Ok(None); };
            let Some((shield, armor, hull)) = services::combat::ship_condition(ship) else { return Ok(None); };
            let capacitor = if let Some(system) = actual_system(&r, &identity).await {
                services::combat::groups_health::capacitor_snapshot(&r.db, &r.world, system, ship).await.map_err(lua_error)?.map(|c| c.level)
            } else { None };
            let table = lua.create_table()?;
            table.set("shield", shield)?; table.set("armor_damage", armor)?;
            table.set("hull_damage", hull)?; table.set("capacitor", capacitor)?;
            Ok(Some(table))
        }
    })?)?;
    let r = resources.clone();
    api.set("targeters", lua.create_async_function(move |lua, id: i64| {
        let r = r.clone(); async move {
            positive(id)?;
            let (_state, row) = lifecycle::locked(&r, id).await.map_err(lua_error)?;
            let identity = lifecycle::snapshot(&r, id, &row).await.map_err(lua_error)?;
            let ids = match (actual_system(&r, &identity).await, identity.ship_id) {
                (Some(system), Some(ship)) => services::combat::targeters(&r.world, system, ship).await,
                _ => Vec::new(),
            };
            lua.create_sequence_from(ids)
        }
    })?)?;
    Ok(())
}
