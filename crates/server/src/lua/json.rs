//! Decode JSON values from the per-script persistent store in `game.lua_store`
//! and server-authored data files into Lua values. Neither source has a client
//! analogue. JSON null and values beyond the server's depth limit are unsupported.

use mlua::{Lua, Result, Value};

/// Server policy: limit host recursion below serde_json's default parse ceiling.
pub(super) const MAX_DEPTH: usize = 64;

pub(super) fn decode(lua: &Lua, value: serde_json::Value, key: &str, depth: usize) -> Result<Value> {
    if depth > MAX_DEPTH { return Err(mlua::Error::runtime(format!("{key} exceeds nesting limit"))); }
    Ok(match value {
        serde_json::Value::Null => return Err(mlua::Error::runtime(format!("invalid null in {key}"))),
        serde_json::Value::Bool(v) => Value::Boolean(v),
        serde_json::Value::Number(v) => if let Some(n) = v.as_i64() { Value::Integer(n) } else { Value::Number(v.as_f64().ok_or_else(|| mlua::Error::runtime("invalid store number"))?) },
        serde_json::Value::String(v) => Value::String(lua.create_string(v)?),
        serde_json::Value::Array(items) => {
            let table = lua.create_table()?;
            for (i, v) in items.into_iter().enumerate() { table.set(i + 1, decode(lua, v, key, depth + 1)?)?; }
            Value::Table(table)
        }
        serde_json::Value::Object(items) => {
            let table = lua.create_table()?;
            for (field, v) in items { table.set(field, decode(lua, v, key, depth + 1)?)?; }
            Value::Table(table)
        }
    })
}
