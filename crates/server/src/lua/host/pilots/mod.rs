//! Simulated pilot operations for scripts, backed by shared character services.
//! Character data comes from the service account and SDE; online state is process-local.
//! The client accepts absent portrait data in eve/client/script/ui/services/ccSvc.py;
//! simulated pilots do not receive generated portraits or customized dolls.

mod effects;
mod lifecycle;
mod space;
#[cfg(test)]
mod tests;

use std::{collections::HashMap, future::Future, sync::{Arc, Mutex}};
use mlua::{Lua, Table, Value};
use crate::{chat::Hub, dao::accounts::simulated, db::Db, services::{self, CallError}};
use super::{client::lua_error, Resources};
use lifecycle::OnlinePilot;

pub(crate) struct Pilots {
    account: i64,
    hub: Arc<Hub>,
    states: Mutex<HashMap<i64, Arc<tokio::sync::Mutex<Option<OnlinePilot>>>>>,
}

impl Pilots {
    pub(crate) async fn new(db: &Db, hub: Arc<Hub>) -> Result<Arc<Self>, CallError> {
        Ok(Arc::new(Self { account: simulated::account_id(db).await.map_err(services::db_err)?, hub, states: Mutex::new(HashMap::new()) }))
    }
    fn state(&self, id: i64) -> Arc<tokio::sync::Mutex<Option<OnlinePilot>>> {
        self.states.lock().unwrap().entry(id).or_default().clone()
    }
}

async fn mutate<T: Send + 'static, F: Future<Output = Result<T, CallError>> + Send + 'static>(future: F) -> Result<T, CallError> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let mut future = std::pin::pin!(future);
        let result = std::future::poll_fn(|cx| match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| future.as_mut().poll(cx))) {
            Ok(result) => result,
            Err(_) => std::task::Poll::Ready(Err(CallError::Internal(anyhow::anyhow!("pilot operation panicked")))),
        }).await;
        if let Err(error) = &result { tracing::warn!(?error, "simulated pilot operation failed"); }
        let _ = tx.send(result);
    });
    rx.await.map_err(|_| CallError::Internal(anyhow::anyhow!("pilot operation task stopped")))?
}

fn positive(id: i64) -> mlua::Result<i64> {
    if id <= 0 { return Err(mlua::Error::runtime("ID must be positive")); }
    Ok(id)
}
fn small_id(id: i64) -> mlua::Result<i64> {
    positive(id)?;
    i32::try_from(id).map_err(|_| mlua::Error::runtime("ID exceeds supported range"))?;
    Ok(id)
}
fn optional_small(id: Option<i64>) -> mlua::Result<Option<i64>> { id.map(small_id).transpose() }

fn creation(table: Table, sde: &crate::sde::Sde) -> mlua::Result<services::character::creation::NewPilot> {
    for entry in table.clone().pairs::<Value, Value>() {
        let (key, _) = entry?;
        let Value::String(key) = key else { return Err(mlua::Error::runtime("unexpected operation argument")); };
        if !["name", "race_id", "bloodline_id", "ancestry_id", "gender_id", "school_id", "station_id", "balance"].iter().any(|name| key.as_bytes().as_ref() == name.as_bytes()) {
            return Err(mlua::Error::runtime("unexpected operation argument"));
        }
    }
    let race = small_id(table.raw_get("race_id")?)?;
    let bloodline = small_id(table.raw_get("bloodline_id")?)?;
    let ancestry = optional_small(table.raw_get("ancestry_id")?)?;
    let school = optional_small(table.raw_get("school_id")?)?;
    let station: Option<i64> = table.raw_get("station_id")?;
    if let Some(id) = station { positive(id)?; }
    let gender: i64 = table.raw_get("gender_id")?;
    if !(0..=1).contains(&gender) { return Err(mlua::Error::runtime("invalid gender")); }
    let balance: f64 = table.raw_get("balance")?;
    if !balance.is_finite() || balance < 0.0 { return Err(mlua::Error::runtime("invalid balance")); }
    if sde.characters.race(race).is_none() || sde.characters.bloodline(bloodline).is_none()
        || school.is_some_and(|id| sde.characters.school(id).is_none())
        || station.is_some_and(|id| sde.map.station(id).is_none()) {
        return Err(mlua::Error::runtime("unknown SDE creation ID"));
    }
    let none = marshal::dumps(&marshal::Value::None);
    Ok(services::character::creation::NewPilot { name: table.raw_get("name")?, race, bloodline, ancestry, gender, school, station, balance, appearance: none.clone(), portrait: none })
}

pub(super) fn register(lua: &Lua, env: &Table, resources: &Resources) -> mlua::Result<()> {
    let api = lua.create_table()?;
    let r = resources.clone();
    api.set("create", lua.create_async_function(move |_, table: Table| {
        let r = r.clone();
        async move {
            let pilot = creation(table, &r.sde)?;
            mutate(async move { Ok(services::character::creation::create(&r.db, &r.sde, r.pilots.account, pilot).await?.char_id) }).await.map_err(lua_error)
        }
    })?)?;
    let r = resources.clone();
    api.set("online", lua.create_async_function(move |_, id: i64| {
        let r = r.clone();
        async move { positive(id)?; mutate(async move { lifecycle::online(r, id).await }).await.map_err(lua_error) }
    })?)?;
    let r = resources.clone();
    api.set("offline", lua.create_async_function(move |_, id: i64| {
        let r = r.clone();
        async move { positive(id)?; mutate(async move { lifecycle::offline(r, id).await }).await.map_err(lua_error) }
    })?)?;
    let r = resources.clone();
    api.set("identity", lua.create_async_function(move |lua, id: i64| {
        let r = r.clone();
        async move {
            positive(id)?;
            let (identity, online) = lifecycle::identity(&r, id).await.map_err(lua_error)?;
            let row = lua.create_table()?;
            row.set("char_id", identity.char_id)?;
            row.set("corp_id", identity.corp_id)?;
            row.set("ship_id", identity.ship_id)?;
            row.set("station_id", identity.station_id)?;
            row.set("system_id", identity.system_id)?;
            row.set("online", online)?;
            Ok(row)
        }
    })?)?;
    space::register(lua, &api, resources)?;
    effects::register(lua, &api, resources)?;
    env.set("pilots", api)
}
