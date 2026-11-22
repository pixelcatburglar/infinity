//! Typed, immutable fields-and-deployables policy compiled at registration.
//! The closed `Compile` operation in `content/fields_deployables/policy.lua`
//! maps the authored probe payload routing in `data/policy.json` onto the
//! definitions the interdiction probe launcher consumes; the original dogma
//! radius, duration and capacity values are read from the SDE at capture and
//! are never copied into content. A retained package recompiles its kept
//! authored bytes, so probe semantics never depend on load order.

use std::collections::BTreeMap;
use mlua::{Lua, Table, Value};
use serde::Deserialize;
use infinity_core::consts::dogma::ATTR_MAX_VELOCITY;
use crate::{config::lua::LuaConfig, services::fit};
use super::policy;

/// Registration namespace of the fields-and-deployables package.
pub(crate) const NAMESPACE: &str = "fields_deployables";

/// The payload family one probe charge executes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PayloadFamily {
    /// A stationary warp-disruption field centred on the probe ball.
    WarpField,
    /// One delayed stasis-web pulse landed through the shared dbuff collection.
    StasisPulse,
}

/// The compiled probe payload policy.
#[derive(Debug)]
pub(crate) struct Definitions {
    /// Probe charge group to payload family, in authored order.
    families: BTreeMap<i32, PayloadFamily>,
    /// The dbuff collection every stasis pulse lands on its receivers.
    stasis_collection_id: i32,
}

impl Definitions {
    /// The compiled family for one probe charge group.
    pub(crate) fn payload_family(&self, group: i32) -> Option<PayloadFamily> {
        self.families.get(&group).copied()
    }
    /// The validated stasis collection identifier.
    pub(crate) fn stasis_collection_id(&self) -> i32 {
        self.stasis_collection_id
    }
}

/// Authored JSON identity and routing values; gameplay numbers stay in the SDE.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Authored {
    version: String,
    source: String,
    payloads: BTreeMap<String, String>,
    stasis_collection_id: i64,
}

fn authored(bytes: &[u8]) -> mlua::Result<Authored> {
    let authored: Authored = serde_json::from_slice(bytes).map_err(mlua::Error::external)?;
    if authored.version.is_empty() || authored.source.is_empty() || authored.payloads.is_empty()
        || authored.payloads.keys().any(|group| group.parse::<i64>().ok().is_none_or(|id| id <= 0))
        || authored.payloads.values().any(|family| !matches!(family.as_str(), "warp_field" | "stasis_pulse"))
        || authored.stasis_collection_id <= 0 {
        return Err(mlua::Error::runtime("invalid fields-deployables policy data"));
    }
    Ok(authored)
}

fn keys(table: &Table, allowed: &[&str]) -> mlua::Result<()> {
    for pair in table.clone().pairs::<String, Value>() {
        let (key, _) = pair?;
        if !allowed.contains(&key.as_str()) {
            return Err(mlua::Error::runtime(format!("unknown fields-deployables field {key}")));
        }
    }
    Ok(())
}

fn field(table: &Table, key: &str) -> mlua::Result<Value> {
    match table.get::<Value>(key)? {
        Value::Nil => Err(mlua::Error::runtime(format!("missing fields-deployables field {key}"))),
        value => Ok(value),
    }
}

fn integer_at(table: &Table, key: &str) -> mlua::Result<i64> {
    match field(table, key)? {
        Value::Integer(value) => Ok(value),
        _ => Err(mlua::Error::runtime(format!("fields-deployables field {key} must be an integer"))),
    }
}

fn table_at(table: &Table, key: &str) -> mlua::Result<Table> {
    match field(table, key)? {
        Value::Table(value) => Ok(value),
        _ => Err(mlua::Error::runtime(format!("fields-deployables field {key} must be a table"))),
    }
}

fn definitions(value: Value) -> mlua::Result<Definitions> {
    let Value::Table(result) = value else {
        return Err(mlua::Error::runtime("fields-deployables Compile must return a table"));
    };
    keys(&result, &["payloads", "stasis_collection_id"])?;
    let payloads = table_at(&result, "payloads")?;
    let mut families = BTreeMap::new();
    for pair in payloads.pairs::<Value, Value>() {
        let (key, value) = pair?;
        let (Value::Integer(group), Value::String(family)) = (key, value) else {
            return Err(mlua::Error::runtime("invalid fields-deployables payload routing"));
        };
        let Ok(group) = i32::try_from(group) else {
            return Err(mlua::Error::runtime("invalid fields-deployables payload routing"));
        };
        if group <= 0 || families.contains_key(&group) {
            return Err(mlua::Error::runtime("invalid fields-deployables payload routing"));
        }
        let family = match family.to_str()?.as_ref() {
            "warp_field" => PayloadFamily::WarpField,
            "stasis_pulse" => PayloadFamily::StasisPulse,
            _ => return Err(mlua::Error::runtime("unknown fields-deployables payload family")),
        };
        families.insert(group, family);
    }
    if families.is_empty() {
        return Err(mlua::Error::runtime("fields-deployables payload routing must not be empty"));
    }
    let collection = integer_at(&result, "stasis_collection_id")?;
    let stasis_collection_id = i32::try_from(collection).ok().filter(|id| *id > 0)
        .ok_or_else(|| mlua::Error::runtime("invalid fields-deployables stasis collection"))?;
    Ok(Definitions { families, stasis_collection_id })
}

/// Finite structural invariants plus the referenced-data checks the SDE can
/// answer: every routed charge group exists, and the stasis collection is the
/// real Minimum/PostPercent web on `maxVelocity`. Retained packages keep
/// their references (`sde` is `None`) exactly like the other compilers.
pub(super) fn validate(definitions: &Definitions, sde: Option<&crate::sde::Sde>) -> mlua::Result<()> {
    let Some(sde) = sde else { return Ok(()) };
    if definitions.families.keys().any(|group| sde.types.group(*group).is_none()) {
        return Err(mlua::Error::runtime("unknown fields-deployables charge group"));
    }
    let collection = sde.dbuffs.get(definitions.stasis_collection_id)
        .ok_or_else(|| mlua::Error::runtime("unknown fields-deployables stasis collection"))?;
    let exact = collection.modifiers.len() == 1
        && collection.modifiers[0].modifier.op == fit::OP_POST_PERCENT
        && collection.modifiers[0].modifier.modified == ATTR_MAX_VELOCITY;
    if collection.aggregate != crate::sde::dbuff::DbuffAggregate::Minimum || !exact {
        return Err(mlua::Error::runtime("invalid fields-deployables stasis collection"));
    }
    Ok(())
}

/// Runs the authored closed `Compile`; the Lua source decides the routing, Rust
/// only validates the typed result. `sde` enables reference validation.
pub(super) fn compile(lua: &Lua, source: &[u8], data: &[u8], config: &LuaConfig, sde: Option<&crate::sde::Sde>) -> mlua::Result<Definitions> {
    // Structural validation of the authored bytes runs before the closed call.
    let _authored = authored(data)?;
    let input = lua.create_table()?;
    input.set("authored", super::super::json::decode(lua, serde_json::from_slice(data).map_err(mlua::Error::external)?, "authored", 0)?)?;
    let source = std::str::from_utf8(source).map_err(mlua::Error::external)?;
    let wrapper = format!(r#"local input = ...
        local exports = (function()
{source}
        end)()
        if type(exports) ~= 'table' or type(exports.Compile) ~= 'function' then error('invalid fields-deployables exports') end
        for key in pairs(exports) do if key ~= 'Compile' then error('unknown fields-deployables export') end end
        return exports.Compile(input)
    "#);
    let definitions = definitions(policy::run_closed(lua, &wrapper, config, (input,), None)?)?;
    validate(&definitions, sde)?;
    Ok(definitions)
}

#[cfg(all(test, feature = "tests-lua"))]
mod tests;
