//! Closed, synchronous sovereignty policy evaluation using the generation's Lua VM.
//! Input is read-only userdata; no database or client state is exposed to scripts.
//! Arbitrary operations, external modules and mutable state between calls are unsupported.

use std::time::{Duration, Instant};
use mlua::{Lua, Table, UserData, UserDataFields, Value, chunk::ChunkMode};
use serde::Deserialize;
use super::Package;
use infinity_core::consts::FILETIME_SECOND;
use crate::lua::vm;

#[derive(Clone, Copy, Debug)]
pub(crate) enum Rule { InitializeCapital, CapitalLost, SelfDestruct }

impl Rule {
    fn name(self) -> &'static str {
        match self { Self::InitializeCapital => "InitializeCapital", Self::CapitalLost => "CapitalLost", Self::SelfDestruct => "SelfDestruct" }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct PolicyInput {
    pub now: i64,
    pub system: i64,
    pub current_system: Option<i64>,
    pub first_appointment: Option<i64>,
    pub last_loss: Option<i64>,
    pub not_before: Option<i64>,
    pub owned_count: u64,
}

impl UserData for PolicyInput {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_field_method_get("now", |_, this| Ok(this.now));
        fields.add_field_method_get("system", |_, this| Ok(this.system));
        fields.add_field_method_get("current_system", |_, this| Ok(this.current_system));
        fields.add_field_method_get("first_appointment", |_, this| Ok(this.first_appointment));
        fields.add_field_method_get("last_loss", |_, this| Ok(this.last_loss));
        fields.add_field_method_get("not_before", |_, this| Ok(this.not_before));
        fields.add_field_method_get("owned_count", |_, this| Ok(this.owned_count));
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum PolicyPlan {
    Unchanged,
    Capital { current_system: i64, not_before: i64 },
    CapitalLost { not_before: i64 },
    SelfDestruct { due: i64 },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Data {
    version: String,
    source: String,
    capital_cooldown_seconds: i64,
    self_destruct_seconds: i64,
    capture_checkpoint_seconds: i64,
}

#[derive(Clone)]
pub(super) struct Values {
    capital_cooldown_ticks: i64,
    self_destruct_ticks: i64,
    pub checkpoint_ms: u64,
}

impl Values {
    pub(super) fn parse(bytes: &[u8], version: &str) -> mlua::Result<Self> {
        let data: Data = serde_json::from_slice(bytes).map_err(mlua::Error::external)?;
        if data.version != version || data.source.is_empty() { return Err(mlua::Error::runtime("invalid policy data identity")); }
        let ticks = |seconds: i64| -> mlua::Result<i64> {
            if seconds <= 0 { return Err(mlua::Error::runtime("policy duration must be positive")); }
            if seconds > i64::MAX / FILETIME_SECOND { return Err(mlua::Error::runtime("policy duration exceeds FILETIME range")); }
            Ok(crate::client::filetime_ticks_from_seconds(seconds))
        };
        let checkpoint = u64::try_from(data.capture_checkpoint_seconds).map_err(mlua::Error::external)?;
        let milliseconds_per_second = crate::space::seconds_to_ms(1.0) as u64;
        let checkpoint_ms = checkpoint.checked_mul(milliseconds_per_second).filter(|value| *value > 0)
            .ok_or_else(|| mlua::Error::runtime("invalid checkpoint interval"))?;
        if checkpoint_ms > i64::MAX as u64 || Instant::now().checked_add(Duration::from_millis(checkpoint_ms)).is_none() {
            return Err(mlua::Error::runtime("checkpoint interval exceeds timer range"));
        }
        Ok(Self { capital_cooldown_ticks: ticks(data.capital_cooldown_seconds)?, self_destruct_ticks: ticks(data.self_destruct_seconds)?, checkpoint_ms })
    }
}

impl UserData for Values {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_field_method_get("capital_cooldown_ticks", |_, this| Ok(this.capital_cooldown_ticks));
        fields.add_field_method_get("self_destruct_ticks", |_, this| Ok(this.self_destruct_ticks));
    }
}

fn run(lua: &Lua, package: &Package, rule: Option<Rule>, input: Option<&PolicyInput>) -> mlua::Result<Value> {
    let source = std::str::from_utf8(&package.source).map_err(mlua::Error::external)?;
    let wrapper = format!(r#"local rule, input, policy = ...
        local exports = (function()
{source}
        end)()
        if type(exports) ~= 'table' or type(exports.InitializeCapital) ~= 'function'
            or type(exports.CapitalLost) ~= 'function' or type(exports.SelfDestruct) ~= 'function' then
            error('invalid policy exports')
        end
        for k in pairs(exports) do
            if k ~= 'InitializeCapital' and k ~= 'CapitalLost' and k ~= 'SelfDestruct' then error('unknown policy export') end
        end
        if rule == nil then return true end
        return exports[rule](input, policy)
    "#);
    let argument = input.map(|v| lua.create_userdata(v.clone())).transpose()?;
    let super::PackageDomain::Sovereignty(values) = &package.domain else { return Err(mlua::Error::runtime("not a sovereignty policy")); };
    run_closed(lua, &wrapper, &package.config, (rule.map(Rule::name), argument, lua.create_userdata(values.clone())?), None)
}

/// Executes text in a capability-closed environment with the ordinary call budget.
pub(super) fn run_closed<A: mlua::IntoLuaMulti>(lua: &Lua, source: &str, config: &crate::config::lua::LuaConfig, args: A, caller_deadline: Option<Instant>) -> mlua::Result<Value> {
    let env = lua.create_table()?;
    for name in ["type", "error", "pairs"] { env.set(name, lua.globals().get::<Value>(name)?)?; }
    let factory = lua.load(source).set_mode(ChunkMode::Text).set_environment(env).into_function()?;
    let own_deadline = Instant::now().checked_add(Duration::from_millis(config.call_budget_ms))
        .ok_or_else(|| mlua::Error::runtime("invalid policy budget"))?;
    let deadline = caller_deadline.map_or(own_deadline, |deadline| deadline.min(own_deadline));
    if Instant::now() >= deadline { return Err(vm::budget_exceeded()); }
    let thread = vm::budgeted_thread(lua, factory, config, deadline)?;
    let result = thread.resume(args)?;
    if Instant::now() >= deadline { return Err(vm::budget_exceeded()); }
    Ok(result)
}

pub(super) fn validate(lua: &Lua, package: &Package) -> mlua::Result<()> {
    match run(lua, package, None, None)? { Value::Boolean(true) => Ok(()), _ => Err(mlua::Error::runtime("invalid policy exports")) }
}

fn integer(table: &Table, key: &str) -> mlua::Result<i64> {
    match table.get::<Value>(key)? {
        Value::Integer(value) => Ok(value),
        _ => Err(mlua::Error::runtime(format!("policy plan field {key} must be an integer"))),
    }
}

pub(super) fn evaluate(lua: &Lua, package: &Package, rule: Rule, input: &PolicyInput) -> mlua::Result<PolicyPlan> {
    if input.system <= 0 || input.current_system.is_some_and(|v| v <= 0) || input.owned_count > i64::MAX as u64 {
        return Err(mlua::Error::runtime("invalid policy input"));
    }
    let table = match run(lua, package, Some(rule), Some(input))? { Value::Table(v) => v, _ => return Err(mlua::Error::runtime("policy plan must be a table")) };
    let kind: String = table.get("kind")?;
    let (plan, keys): (PolicyPlan, &[&str]) = match kind.as_str() {
        "unchanged" => (PolicyPlan::Unchanged, &["kind"]),
        "capital" => (PolicyPlan::Capital { current_system: integer(&table, "current_system")?, not_before: integer(&table, "not_before")? }, &["kind", "current_system", "not_before"]),
        "capital_lost" => (PolicyPlan::CapitalLost { not_before: integer(&table, "not_before")? }, &["kind", "not_before"]),
        "self_destruct" => (PolicyPlan::SelfDestruct { due: integer(&table, "due")? }, &["kind", "due"]),
        _ => return Err(mlua::Error::runtime("unknown policy plan")),
    };
    for pair in table.pairs::<String, Value>() {
        let (key, _) = pair?;
        if !keys.contains(&key.as_str()) { return Err(mlua::Error::runtime("unknown policy plan field")); }
    }
    let valid = match (&rule, &plan) {
        (Rule::InitializeCapital | Rule::CapitalLost, PolicyPlan::Unchanged) => true,
        (Rule::InitializeCapital, PolicyPlan::Capital { current_system, not_before }) =>
            input.current_system.is_none() && *current_system == input.system && *not_before > input.now
                && input.not_before.is_none_or(|previous| *not_before >= previous),
        (Rule::CapitalLost, PolicyPlan::CapitalLost { not_before }) =>
            input.current_system == Some(input.system) && *not_before > input.now
                && input.not_before.is_none_or(|previous| *not_before >= previous),
        (Rule::SelfDestruct, PolicyPlan::SelfDestruct { due }) =>
            due.checked_sub(input.now).is_some_and(|duration| duration > 0),
        _ => false,
    };
    if !valid { return Err(mlua::Error::runtime("policy plan violates input constraints")); }
    Ok(plan)
}
