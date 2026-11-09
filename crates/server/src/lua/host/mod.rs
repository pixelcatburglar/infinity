//! Script-specific host functions backed by existing server facades and typed data.
//! Client operations use invocation-local contexts; no Lua executes in world actors.
//! Persistent script values use a server-owned typed store, not arbitrary SQL.

pub mod client;
mod dao;
mod log;
pub(crate) mod pilots;
mod sde;
mod store;
mod time;
mod world;

use std::sync::Arc;
use mlua::{Lua, Result, Table, Value};
use crate::{db::Db, sde::Sde, space::World};

#[derive(Clone)]
pub struct Resources {
    pub db: Db,
    pub sde: Arc<Sde>,
    pub world: Arc<World>,
    pub(crate) pilots: Arc<pilots::Pilots>,
    pub store_value_limit_bytes: usize,
}

fn config_value(lua: &Lua, value: &toml::Value) -> Result<Value> {
    Ok(match value {
        toml::Value::String(v) => Value::String(lua.create_string(v)?),
        toml::Value::Integer(v) => Value::Integer(*v),
        toml::Value::Float(v) => Value::Number(*v),
        toml::Value::Boolean(v) => Value::Boolean(*v),
        toml::Value::Datetime(v) => Value::String(lua.create_string(v.to_string())?),
        toml::Value::Array(values) => {
            let result = lua.create_table()?;
            for (index, value) in values.iter().enumerate() { result.set(index + 1, config_value(lua, value)?)?; }
            Value::Table(result)
        }
        toml::Value::Table(values) => {
            let result = lua.create_table()?;
            for (key, value) in values { result.set(key.as_str(), config_value(lua, value)?)?; }
            Value::Table(result)
        }
    })
}

pub fn register(lua: &Lua, env: &Table, script: &str, config: &toml::Table, resources: &Resources) -> Result<()> {
    let messages = lua.create_table()?;
    messages.set("motd_help", infinity_core::messages::lua::LUA_MOTD_HELP)?;
    messages.set("motd_saved", infinity_core::messages::lua::LUA_MOTD_SAVED)?;
    messages.set("motd_empty", infinity_core::messages::lua::LUA_MOTD_EMPTY)?;
    messages.set("custom_notify", infinity_core::messages::MSG_CUSTOM_NOTIFY)?;
    env.set("messages", messages)?;
    env.set("config", config_value(lua, &toml::Value::Table(config.clone()))?)?;
    dao::register(lua, env, &resources.db, &resources.sde)?;
    world::register(lua, env, resources)?;
    sde::register(lua, env, &resources.sde)?;
    log::register(lua, env, script)?;
    time::register(lua, env, &resources.world)?;
    pilots::register(lua, env, resources)?;
    store::register(lua, env, &resources.db, script, resources.store_value_limit_bytes)
}

#[cfg(test)]
mod tests;
