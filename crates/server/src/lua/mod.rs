//! Sandboxed Lua RPC, intercept, command, post-commit event and periodic extensions.
//! Scripts use client, world and DAO facades and a typed persistent store.
//! Sovereignty authored content uses a separate capability-closed environment in
//! the same VM. Events and process-local ticks carry no synchronous reply.

pub mod host;
pub mod events;
pub mod value;
pub(crate) mod content;
pub(crate) mod gateway;
mod json;
mod registry;
pub(crate) mod reload;
mod runner;
mod vm;

use std::{future::Future, sync::{OnceLock, atomic::{AtomicBool, Ordering}}, time::{Duration, Instant}};
use infinity_core::consts::GM_ROLES;
use crate::{client::Client, config::Config, db::Db, sde::Sde, services::{Call, CallError, CallResult}, space::World};
use std::sync::Arc;
use registry::{Handler, Registry};
use value::WireValue;

static HOST: OnceLock<Arc<reload::Host>> = OnceLock::new();
static ENABLED: AtomicBool = AtomicBool::new(false);
static EVENTS: OnceLock<tokio::sync::mpsc::UnboundedSender<events::GameEvent>> = OnceLock::new();

pub fn enabled() -> bool { ENABLED.load(Ordering::Acquire) }
pub(crate) fn content_policy(namespace: &str) -> Result<content::PinnedPolicy, CallError> {
    let host = HOST.get().ok_or_else(|| crate::services::db_err(anyhow::anyhow!("Lua host is not initialized")))?;
    content::PinnedPolicy::pin(host.snapshot(), namespace)
}

pub(crate) fn admit_content(policy: &content::PinnedPolicy) -> Result<content::OwnedAdmission, CallError> {
    Ok(HOST.get().ok_or_else(|| crate::services::db_err(anyhow::anyhow!("Lua host is not initialized")))?.admit(policy)?)
}

pub(crate) async fn retained_content(namespace: &str, version: &str) -> Result<content::PinnedPolicy, CallError> {
    HOST.get().ok_or_else(|| crate::services::db_err(anyhow::anyhow!("Lua host is not initialized")))?.retained(namespace, version).await
}

/// Narrow access to retained content policies for prepared progress consumers.
#[derive(Clone)]
pub(crate) struct RetainedPolicies(Arc<reload::Host>);

impl RetainedPolicies {
    pub(crate) async fn resolve(&self, namespace: &str, version: &str) -> Result<content::PinnedPolicy, CallError> {
        self.0.retained(namespace, version).await
    }
}

pub(crate) fn retained_policies() -> Result<RetainedPolicies, CallError> {
    if !enabled() {
        return Err(crate::services::db_err(anyhow::anyhow!("Lua host is disabled")));
    }
    Ok(RetainedPolicies(HOST.get().ok_or_else(|| crate::services::db_err(anyhow::anyhow!("Lua host is not initialized")))?.clone()))
}


pub async fn start(config: &Config, db: &Db, sde: &Arc<Sde>, world: &Arc<World>, hub: &Arc<crate::chat::Hub>) -> anyhow::Result<()> {
    if !config.lua.enabled { return Ok(()); }
    let pilots = host::pilots::Pilots::new(db, hub.clone()).await.map_err(|error| anyhow::anyhow!("{error:?}"))?;
    let resources = host::Resources { db: db.clone(), sde: sde.clone(), world: world.clone(), pilots, store_value_limit_bytes: config.lua.store_value_limit_bytes };
    let host = Arc::new(reload::Host::new(config.lua.clone(), resources).await?);
    HOST.set(host).map_err(|_| anyhow::anyhow!("Lua host already initialized"))?;
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    EVENTS.set(tx).map_err(|_| anyhow::anyhow!("Lua event runner already initialized"))?;
    let host = HOST.get().expect("initialized Lua host");
    tokio::spawn(runner::run(host.config.clone(), host.subscribe(), rx));
    ENABLED.store(true, Ordering::Release);
    Ok(())
}

async fn invoke(c: &mut Client, host: &Arc<reload::Host>, registry: &Registry, handler: &Handler, args: Vec<mlua::Value>, query: Option<(i64, crate::services::sovereignty::queries::Query)>, adaption: Option<host::client::AdaptionInvocation<'_>>) -> Result<mlua::MultiValue, CallError> {
    let deadline = Instant::now() + Duration::from_millis(host.config.call_budget_ms);
    let invocation = vm::budgeted_invocation(&registry.lua, handler.function.clone(), &host.config, deadline).map_err(|e| CallError::Internal(e.into()))?;
    let future = host::client::invoke(c, &registry.lua, invocation, args, tokio::time::Instant::from_std(deadline), query, adaption);
    let mut future = std::pin::pin!(future);
    let guarded = std::future::poll_fn(|cx| {
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| future.as_mut().poll(cx))) {
            Ok(result) => result,
            Err(_) => std::task::Poll::Ready(Err(CallError::Internal(anyhow::anyhow!("Lua runtime panic")))),
        }
    });
    let result = guarded.await;
    if let Err(CallError::Internal(error)) = &result {
        tracing::warn!(script = %handler.script, definition_line = ?handler.function.info().line_defined, "Lua handler failed: {error:#}");
    }
    result
}


pub async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    dispatch_host(c, call, HOST.get()?).await
}

async fn dispatch_host(c: &mut Client, call: &Call<'_>, host: &Arc<reload::Host>) -> Option<CallResult> {
    let character = c.session.int("charid");
    let system = c.session.int("solarsystemid");
    let registry = host.snapshot();
    if sovereignty_claimed(call) && !registry.claims.rpc.contains_key(&(call.service.to_owned(), call.method.to_owned())) {
        return None;
    }
    let query = if sovereignty_claimed(call) {
        match sovereignty_query(call, character, system) {
            Ok(query) => Some(query),
            Err(error) => return Some(Err(error)),
        }
    } else { None };
    if let Some(crate::services::sovereignty::queries::Query::AllianceStructures { alliance_id } | crate::services::sovereignty::queries::Query::CapitalSystemInfo { alliance_id }) = query {
        match crate::dao::characters::members::organization(&c.db, character.unwrap_or(0)).await {
            Ok(Some(org)) if org.alliance_id == Some(alliance_id) => {}
            Ok(_) => return Some(Err(CallError::Internal(anyhow::anyhow!("sovereignty alliance membership required")))),
            Err(error) => return Some(Err(crate::services::db_err(error))),
        }
    }
    if let Some(handler) = registry.claims.intercepts.get(&(call.service.to_owned(), call.method.to_owned())) {
        let args = match (registry.lua.pack(WireValue(marshal::Value::Tuple(call.args.to_vec()))), registry.lua.pack(WireValue(call.kwargs.clone()))) {
            (Ok(args), Ok(kwargs)) => vec![args, kwargs],
            (Err(error), _) | (_, Err(error)) => return Some(Err(CallError::Internal(error.into()))),
        };
        match invoke(c, host, &registry, handler, args, None, None).await.and_then(intercept_result) {
            Ok(()) => {}
            Err(error) => return Some(Err(error)),
        }
    }
    let exact = (call.service.to_owned(), call.method.to_owned());
    let wildcard = (call.service.to_owned(), "*".to_owned());
    let (handler, is_wildcard) = match registry.claims.rpc.get(&exact) {
        Some(handler) => (handler, false),
        None if sovereignty_claimed(call) || call.service == "shipAdaptionMgr" => return None,
        None => (registry.claims.rpc.get(&wildcard)?, true),
    };
    let adaption = if call.service == "shipAdaptionMgr" {
        let pin = match content::PinnedPolicy::pin(registry.clone(), "ship_adaption") { Ok(pin) => pin, Err(error) => return Some(Err(error)) };
        let Some(definitions) = pin.adaption() else { return Some(Err(crate::services::db_err(anyhow::anyhow!("missing adaptation definitions")))) };
        let request = match crate::services::ship_adaption::model::decode(call, c.login.user_id, character.unwrap_or(0), definitions.quoted_count()) {
            Some(request) => request,
            None => return Some(Err(crate::services::ship_adaption::operations::refusal(definitions, content::adaption::INVALID))),
        };
        Some(host::client::AdaptionInvocation { host, pin, request, identity: crate::services::identity::Identity::of_client(c), user: c.login.user_id })
    } else { None };
    let args = match (registry.lua.pack(WireValue(marshal::Value::Tuple(call.args.to_vec()))), registry.lua.pack(WireValue(call.kwargs.clone()))) {
        (Ok(args), Ok(kwargs)) => vec![args, kwargs],
        (Err(error), _) | (_, Err(error)) => return Some(Err(CallError::Internal(error.into()))),
    };
    match invoke(c, host, &registry, handler, args, query.map(|query| (character.unwrap_or(0), query)), adaption).await {
        Ok(mut result) => {
            let result = result.pop_front().unwrap_or(mlua::Value::Nil);
            if result.is_nil() && is_wildcard { return None; }
            if crate::services::lua_fixed_claimed(call) {
                let mut seen = registry.overrides.lock().unwrap_or_else(|e| e.into_inner());
                if seen.insert(exact) { tracing::info!(script = %handler.script, "Lua overrides fixed core answer {}::{}", call.service, call.method); }
            }
            Some(value::from_lua(result).map_err(|e| CallError::Internal(e.into())))
        }
        Err(error) => Some(Err(error)),
    }
}

pub(crate) fn sovereignty_claimed(call: &Call<'_>) -> bool {
    matches!((call.service, call.method),
        ("sovMgr", "GetSystemSovereigntyInfo" | "GetInfrastructureHubInfo" | "GetSovStructuresInfoForSolarSystem" | "GetSovStructuresInfoForLocalSolarSystem")
        | ("allianceRegistry", "GetAllianceSovereigntyStructuresInfo" | "GetCapitalSystemInfo"))
}

fn sovereignty_query(call: &Call<'_>, character: Option<i64>, system: Option<i64>) -> Result<crate::services::sovereignty::queries::Query, CallError> {
    use crate::services::sovereignty::queries::Query;
    use marshal::Value;
    let invalid = || CallError::Internal(anyhow::anyhow!("invalid sovereignty request scope"));
    if !character.is_some_and(|id| id > 0) || !(matches!(call.kwargs, Value::None) || matches!(call.kwargs, Value::Dict(v) if v.is_empty())) {
        return Err(invalid());
    }
    let query = match (call.service, call.method, call.args) {
        ("sovMgr", "GetSystemSovereigntyInfo", [Value::Int(id)]) if *id > 0 => Query::SystemSovereigntyInfo { system_id: *id },
        ("sovMgr", "GetInfrastructureHubInfo", [Value::Int(id)]) if *id > 0 => Query::InfrastructureHubInfo { system_id: *id },
        ("sovMgr", "GetSovStructuresInfoForSolarSystem", [Value::Int(id)]) if *id > 0 => Query::Structures { system_id: *id },
        ("sovMgr", "GetSovStructuresInfoForLocalSolarSystem", []) => Query::Structures { system_id: system.filter(|id| *id > 0).ok_or_else(invalid)? },
        ("allianceRegistry", method, []) if call.bound => {
            let Some(Value::Tuple(values)) = call.bind_params else { return Err(invalid()) };
            let [Value::Int(alliance), Value::Int(1)] = values.as_slice() else { return Err(invalid()) };
            if *alliance <= 0 { return Err(invalid()); }
            match method {
                "GetAllianceSovereigntyStructuresInfo" => Query::AllianceStructures { alliance_id: *alliance },
                "GetCapitalSystemInfo" => Query::CapitalSystemInfo { alliance_id: *alliance },
                _ => return Err(invalid()),
            }
        }
        _ => return Err(invalid()),
    };
    Ok(query)
}

fn intercept_result(mut values: mlua::MultiValue) -> Result<(), CallError> {
    match (values.pop_front(), values.pop_front(), values.is_empty()) {
        (Some(mlua::Value::Boolean(true)), None, true) => Ok(()),
        (Some(mlua::Value::Boolean(false)), Some(mlua::Value::String(message)), true) => {
            let message = message.to_str().map_err(|error| CallError::Internal(error.into()))?;
            Err(CallError::User { msg: message.to_owned(), dict: Vec::new() })
        }
        _ => Err(CallError::Internal(anyhow::anyhow!("invalid Lua intercept result"))),
    }
}

pub async fn command(c: &mut Client, name: &str, args: &[String]) -> Option<Result<String, CallError>> {
    let host = HOST.get()?;
    if name == "lua" {
        if c.login.role & GM_ROLES == 0 { return Some(Err(crate::services::notify_error(infinity_core::messages::gm::GM_ROLE_REQUIRED))); }
        return Some(match args {
            [arg] if arg == "reload" => match host.reload().await { Ok(()) => Ok(infinity_core::messages::gm::LUA_RELOADED.into()), Err(error) => Ok(infinity_core::messages::gm::lua_reload_failed(&format!("{error:#}"))) },
            [arg] if arg == "list" => {
                let registry = host.snapshot();
                let mut entries: Vec<_> = registry.claims.rpc.iter().map(|((svc, method), h)| format!("{}: {}::{}", h.script, svc, method)).collect();
                entries.extend(registry.claims.commands.iter().map(|(name, cmd)| format!("{}: /{}", cmd.handler.script, name)));
                entries.extend(registry.claims.intercepts.iter().map(|((svc, method), h)| format!("{}: intercept {}::{}", h.script, svc, method)));
                entries.extend(registry.claims.events.iter().flat_map(|(event, handlers)| handlers.iter().map(move |h| format!("{}: event {}", h.script, event))));
                entries.extend(registry.claims.ticks.iter().map(|tick| format!("{}: tick {} interval_ms={} skipped={}", tick.handler.script, tick.name, tick.interval.as_millis(), tick.skipped.load(Ordering::Relaxed))));
                entries.extend(registry.claims.scripts.iter().map(|name| format!("script: {name} root={}", registry.claims.script_roots[name].display())));
                entries.sort();
                entries.push(format!("memory={} MiB, time={} ms, instructions={}, interval={}", host.config.memory_limit_mb, host.config.call_budget_ms, host.config.call_budget_instructions, host.config.instruction_hook_interval));
                Ok(infinity_core::messages::gm::lua_list(&entries.join("\n")))
            }
            _ => Ok(infinity_core::messages::gm::LUA_HELP.into()),
        });
    }
    let registry = host.snapshot();
    let command = registry.claims.commands.get(name)?;
    if command.gm && c.login.role & GM_ROLES == 0 { return Some(Err(crate::services::notify_error(infinity_core::messages::gm::GM_ROLE_REQUIRED))); }
    let args = match registry.lua.create_sequence_from(args.iter().map(String::as_str)) { Ok(args) => vec![mlua::Value::Table(args)], Err(error) => return Some(Err(CallError::Internal(error.into()))) };
    Some(match invoke(c, host, &registry, &command.handler, args, None, None).await {
        Ok(mut values) => match values.pop_front() { Some(mlua::Value::String(s)) => s.to_str().map(|s| s.to_owned()).map_err(|e| CallError::Internal(e.into())), _ => Err(CallError::Internal(anyhow::anyhow!("Lua command must return a string"))) },
        Err(error) => Err(error),
    })
}

pub fn command_help() -> Vec<(String, String)> {
    let Some(host) = HOST.get() else { return Vec::new() };
    let registry = host.snapshot();
    let mut result: Vec<_> = registry.claims.commands.iter().map(|(name, cmd)| (name.clone(), cmd.help.clone())).collect();
    result.push(("lua".into(), infinity_core::messages::gm::LUA_HELP.into()));
    result.sort();
    result
}

pub(crate) fn on_event(event: events::GameEvent) {
    if let Some(sender) = EVENTS.get() {
        if sender.send(event).is_err() { tracing::warn!("Lua event runner stopped"); }
    }
}

struct HookGuard(std::sync::Arc<std::sync::Mutex<Option<mlua::Thread>>>);
impl Drop for HookGuard {
    fn drop(&mut self) {
        if let Some(thread) = self.0.lock().unwrap_or_else(|e| e.into_inner()).take() { thread.remove_hook(); }
    }
}

async fn invoke_event(registry: &Registry, config: &crate::config::lua::LuaConfig, event: &events::GameEvent) {
    let Some(handlers) = registry.claims.events.get(event.kind()) else { return };
    for handler in handlers {
        let result = invoke_no_context(registry, config, handler, event.to_lua(&registry.lua).map(mlua::Value::Table)).await;
        if let Err(error) = result { tracing::warn!(script = %handler.script, event = event.kind(), "Lua event handler failed: {error}"); }
    }
}

async fn invoke_no_context(registry: &Registry, config: &crate::config::lua::LuaConfig, handler: &Handler, argument: mlua::Result<mlua::Value>) -> mlua::Result<()> {
    let future = async {
        let deadline = Instant::now() + Duration::from_millis(config.call_budget_ms);
        let timeout = tokio::time::Instant::now() + Duration::from_millis(config.call_budget_ms);
        let invocation = vm::budgeted_invocation(&registry.lua, handler.function.clone(), config, deadline)?;
        let call = invocation.function.call_async::<()>(argument?);
        tokio::pin!(call);
        let _guard = HookGuard(invocation.hook_thread);
        tokio::select! {
            biased;
            _ = tokio::time::sleep_until(timeout) => Err(vm::budget_exceeded()),
            result = &mut call => result,
        }
    };
    let mut future = std::pin::pin!(future);
    std::future::poll_fn(|cx| match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| future.as_mut().poll(cx))) {
        Ok(result) => result,
        Err(_) => std::task::Poll::Ready(Err(mlua::Error::runtime("Lua runtime panic"))),
    }).await
}

#[cfg(test)]
pub(crate) mod tests;
