//! Per-script persistent JSON values in `game.lua_store`. Scripts can only access
//! their own keys; SQL and arbitrary Lua userdata are never exposed.

use std::collections::{BTreeMap, HashSet};
use mlua::{Lua, Result, Table, Value};
use serde_json::{Map, Number};
use crate::{dao::lua_store, db::Db};

use super::super::json::{decode, MAX_DEPTH};

fn charge(remaining: &mut usize, amount: usize, path: &str) -> Result<()> {
    *remaining = remaining.checked_sub(amount).ok_or_else(|| mlua::Error::runtime(format!("store value at {path} exceeds size limit")))?;
    Ok(())
}

struct Capped<'a>(&'a mut usize);
impl std::io::Write for Capped<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        *self.0 = self.0.checked_sub(bytes.len()).ok_or_else(|| std::io::Error::other("size limit"))?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
}

fn charge_json(value: &impl serde::Serialize, remaining: &mut usize, path: &str) -> Result<()> {
    serde_json::to_writer(Capped(remaining), value).map_err(|_| mlua::Error::runtime(format!("store value at {path} exceeds size limit")))
}

fn encode(value: Value, path: &str, active: &mut HashSet<usize>, remaining: &mut usize, depth: usize) -> Result<serde_json::Value> {
    let invalid = || mlua::Error::runtime(format!("unsupported store value at {path}"));
    if depth > MAX_DEPTH { return Err(mlua::Error::runtime(format!("store value at {path} exceeds nesting limit"))); }
    match value {
        Value::Boolean(v) => { charge_json(&v, remaining, path)?; Ok(v.into()) }
        Value::Integer(v) => { charge_json(&v, remaining, path)?; Ok(v.into()) }
        Value::Number(v) => {
            let number = Number::from_f64(v).ok_or_else(invalid)?;
            charge_json(&number, remaining, path)?;
            Ok(serde_json::Value::Number(number))
        }
        Value::String(v) => {
            let text = v.to_str().map_err(|_| invalid())?;
            charge_json(&text.as_ref(), remaining, path)?;
            Ok(text.to_string().into())
        }
        Value::Table(table) => {
            charge(remaining, 2, path)?;
            let ptr = table.to_pointer() as usize;
            if !active.insert(ptr) { return Err(mlua::Error::runtime(format!("cyclic store value at {path}"))); }
            let result = (|| {
                let mut object = Map::new();
                let mut array = BTreeMap::new();
                for pair in table.pairs::<Value, Value>() {
                    let (key, value) = pair?;
                    if !object.is_empty() || !array.is_empty() { charge(remaining, 1, path)?; }
                    match key {
                        Value::String(key) => {
                            let key = key.to_str().map_err(|_| invalid())?;
                            charge_json(&key.as_ref(), remaining, path)?;
                            charge(remaining, 1, path)?;
                            let key = key.to_string();
                            object.insert(key.clone(), encode(value, &format!("{path}.{key}"), active, remaining, depth + 1)?);
                        }
                        Value::Integer(key) if key > 0 => {
                            array.insert(key, encode(value, &format!("{path}[{key}]"), active, remaining, depth + 1)?);
                        }
                        _ => return Err(invalid()),
                    }
                }
                if !object.is_empty() && !array.is_empty() { return Err(mlua::Error::runtime(format!("mixed store keys at {path}"))); }
                if !array.is_empty() {
                    if array.keys().copied().ne(1..=array.len() as i64) { return Err(mlua::Error::runtime(format!("sparse store array at {path}"))); }
                    Ok(serde_json::Value::Array(array.into_values().collect()))
                } else { Ok(serde_json::Value::Object(object)) }
            })();
            active.remove(&ptr);
            result
        }
        _ => Err(invalid()),
    }
}

fn decode_stored(lua: &Lua, value: &str, key: &str, limit: usize) -> Result<Value> {
    // PostgreSQL renders JSONB with spaces after separators; allow that
    // representation before applying the cap to compact JSON.
    if value.len() > limit.saturating_mul(2) { return Err(mlua::Error::runtime(format!("store value at {key} exceeds {limit} bytes"))); }
    let value: serde_json::Value = serde_json::from_str(value).map_err(|error| mlua::Error::runtime(format!("invalid store value at {key}: {error}")))?;
    charge_json(&value, &mut limit.clone(), key)?;
    decode(lua, value, &format!("store value at {key}"), 0)
}

fn sequence_keys(table: Table) -> Result<Vec<String>> {
    let len = table.raw_len();
    let mut count = 0;
    for pair in table.clone().pairs::<Value, Value>() {
        let (index, key) = pair?;
        match (index, key) {
            (Value::Integer(index), Value::String(key)) if index > 0 && (index as u64) <= len as u64 => {
                key.to_str()?;
                count += 1;
            }
            _ => return Err(mlua::Error::runtime("store keys must be a dense sequence of strings")),
        }
    }
    if count != len {
        return Err(mlua::Error::runtime("store keys must be a dense sequence of strings"));
    }
    table.sequence_values::<String>().collect()
}

pub(super) fn register(lua: &Lua, env: &Table, db: &Db, script: &str, limit: usize) -> Result<()> {
    let store = lua.create_table()?;
    let (db, name) = (db.clone(), script.to_owned());
    let (handle, script) = (db.clone(), name.clone());
    store.set("get", lua.create_async_function(move |lua, key: String| {
        let (db, script) = (handle.clone(), script.clone());
        async move {
            match lua_store::get(&db, &script, &key).await.map_err(mlua::Error::external)? {
                Some(value) => decode_stored(&lua, &value, &key, limit),
                None => Ok(Value::Nil),
            }
        }
    })?)?;
    let (handle, script) = (db.clone(), name.clone());
    store.set("get_many", lua.create_async_function(move |lua, keys: Table| {
        let (db, script) = (handle.clone(), script.clone());
        async move {
            let keys = sequence_keys(keys)?;
            let rows = lua_store::get_many(&db, &script, &keys).await.map_err(mlua::Error::external)?;
            let values = lua.create_table()?;
            for row in rows {
                values.set(row.key.as_str(), decode_stored(&lua, &row.value, &row.key, limit)?)?;
            }
            Ok(values)
        }
    })?)?;
    let (handle, script) = (db.clone(), name.clone());
    store.set("set", lua.create_async_function(move |_, (key, value): (String, Value)| {
        let (db, script) = (handle.clone(), script.clone());
        async move {
            let value = encode(value, &key, &mut HashSet::new(), &mut limit.clone(), 0)?;
            let value = serde_json::to_string(&value).map_err(mlua::Error::external)?;
            if value.len() > limit { return Err(mlua::Error::runtime(format!("store value at {key} exceeds {limit} bytes"))); }
            lua_store::set(&db, &script, &key, &value).await.map_err(mlua::Error::external)
        }
    })?)?;
    let (handle, script) = (db.clone(), name.clone());
    store.set("delete", lua.create_async_function(move |_, key: String| {
        let (db, script) = (handle.clone(), script.clone());
        async move { lua_store::delete(&db, &script, &key).await.map_err(mlua::Error::external) }
    })?)?;
    store.set("keys", lua.create_async_function(move |_, prefix: String| {
        let (db, script) = (db.clone(), name.clone());
        async move { lua_store::keys(&db, &script, &prefix).await.map_err(mlua::Error::external) }
    })?)?;
    env.set("store", store)
}

#[cfg(test)]
mod tests;
