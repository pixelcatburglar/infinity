//! Builds a generation in an isolated Lua 5.4 VM. Scripts use separate globals,
//! text-only modules under each configured root and invocation-local budgets.
//! Native modules, binary chunks and script-created coroutines are not supported.

use std::{path::Path, sync::{Arc, Mutex, atomic::{AtomicBool, AtomicU64, Ordering}}, time::{Duration, Instant}};
use anyhow::{Context, Result};
use mlua::{chunk::ChunkMode, Function, HookTriggers, Lua, Table, Thread, VmState};
use crate::config::lua::LuaConfig;
use super::{host, registry::{Claims, Command, Handler, Registry, Tick}, value};

/// The space actor loop cadence is 100 ms (space/actor.rs).
const MIN_TICK_INTERVAL_MS: u64 = 100;
#[derive(Debug)]
struct BudgetExceeded;

impl std::fmt::Display for BudgetExceeded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Lua execution budget exceeded")
    }
}

impl std::error::Error for BudgetExceeded {}

pub(super) fn budget_exceeded() -> mlua::Error {
    mlua::Error::external(BudgetExceeded)
}


fn install_budget(thread: &Thread, maximum: u64, interval: u64, deadline: Instant) -> mlua::Result<()> {
    let count = AtomicU64::new(0);
    thread.set_hook(HookTriggers::new().every_nth_instruction(interval as u32), move |_, _| {
        let instructions = count.fetch_add(interval, Ordering::Relaxed).saturating_add(interval);
        if instructions >= maximum || Instant::now() >= deadline {
            return Err(budget_exceeded());
        }
        Ok(VmState::Continue)
    })
}

pub fn budgeted_thread(lua: &Lua, function: Function, config: &LuaConfig, deadline: Instant) -> mlua::Result<Thread> {
    let thread = lua.create_thread(function)?;
    install_budget(&thread, config.call_budget_instructions, u64::from(config.instruction_hook_interval).min(config.call_budget_instructions), deadline)?;
    Ok(thread)
}

pub struct Invocation {
    pub function: Function,
    pub hook_thread: Arc<Mutex<Option<Thread>>>,
}

pub fn budgeted_invocation(lua: &Lua, handler: Function, config: &LuaConfig, deadline: Instant) -> mlua::Result<Invocation> {
    let hook_thread = Arc::new(Mutex::new(None));
    let installed = hook_thread.clone();
    let maximum = config.call_budget_instructions;
    let interval = u64::from(config.instruction_hook_interval).min(maximum);
    let setup = lua.create_function(move |_, thread: Thread| {
        install_budget(&thread, maximum, interval, deadline)?;
        *installed.lock().map_err(|e| mlua::Error::runtime(e.to_string()))? = Some(thread);
        Ok(())
    })?;
    // mlua's current_thread() maps implicit call_async coroutines to their owner.
    // Lua's coroutine.running() returns the actual coroutine that needs the hook.
    let factory: Function = lua.load("return function(setup, handler, running) return function(...) setup(running()); return handler(...) end end").eval()?;
    let running: Function = lua.globals().get::<Table>("coroutine")?.get("running")?;
    Ok(Invocation { function: factory.call((setup, handler, running))?, hook_thread })
}

fn environment(lua: &Lua, root: &Path) -> mlua::Result<Table> {
    let env = lua.create_table()?;
    let globals = lua.globals();
    for name in ["assert", "error", "ipairs", "next", "pairs", "select", "tonumber", "tostring", "type", "rawequal", "rawget", "rawlen", "rawset", "setmetatable", "_VERSION"] {
        env.set(name, globals.get::<mlua::Value>(name)?)?;
    }
    for name in ["math", "string", "table", "utf8"] {
        let original: Table = globals.get(name)?;
        let copy = lua.create_table()?;
        for item in original.pairs::<mlua::Value, mlua::Value>() { let (k, v) = item?; copy.set(k, v)?; }
        if name == "string" { copy.set("dump", mlua::Value::Nil)?; }
        env.set(name, copy)?;
    }
    let is_budget = lua.create_function(|_, value: mlua::Value| {
        Ok(matches!(value, mlua::Value::Error(error) if error.downcast_ref::<BudgetExceeded>().is_some()))
    })?;
    let factory: Function = lua.load(r#"
        return function(pcall, xpcall, pack, unpack, raise, is_budget, kind)
            local function finish(...)
                local results = pack(...)
                if not results[1] and is_budget(results[2]) then raise(results[2], 0) end
                return unpack(results, 1, results.n)
            end
            return function(f, ...) return finish(pcall(f, ...)) end,
                function(f, handler, ...)
                    if kind(handler) ~= "function" then return xpcall(f, handler, ...) end
                    return finish(xpcall(f, function(err)
                        if is_budget(err) then return err end
                        local ok, result = pcall(handler, err)
                        if ok or is_budget(result) then return result end
                        raise(result, 0)
                    end, ...))
                end
        end
    "#).eval()?;
    let table: Table = globals.get("table")?;
    let (pcall, xpcall): (Function, Function) = factory.call((
        globals.get::<Function>("pcall")?, globals.get::<Function>("xpcall")?,
        table.get::<Function>("pack")?, table.get::<Function>("unpack")?,
        globals.get::<Function>("error")?, is_budget, globals.get::<Function>("type")?,
    ))?;
    env.set("pcall", pcall)?;
    env.set("xpcall", xpcall)?;
    let os: Table = globals.get("os")?;
    let safe_os = lua.create_table()?;
    for name in ["time", "clock", "date", "difftime"] { safe_os.set(name, os.get::<mlua::Value>(name)?)?; }
    env.set("os", safe_os)?;
    env.set("collectgarbage", lua.create_function(|lua, option: Option<String>| {
        if option.as_deref() != Some("count") { return Err(mlua::Error::runtime("only collectgarbage('count') is permitted")); }
        Ok(lua.used_memory() as f64 / 1024.0)
    })?)?;
    env.set("_G", env.clone())?;
    let root = root.to_owned();
    let module_env = env.clone();
    let loader = lua.create_function(move |lua, name: String| {
        if name.is_empty() || !name.split('.').all(|part| !part.is_empty() && part.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_')) {
            return Err(mlua::Error::runtime("invalid module name"));
        }
        let path = root.join(format!("{}.lua", name.replace('.', "/"))).canonicalize().map_err(mlua::Error::external)?;
        if !path.starts_with(&root) { return Err(mlua::Error::runtime("module escapes scripts directory")); }
        let text = std::fs::read(&path).map_err(mlua::Error::external)?;
        lua.load(text.as_slice()).set_name(path.to_string_lossy()).set_mode(ChunkMode::Text).set_environment(module_env.clone()).into_function()
    })?;
    // The Lua wrapper invokes the module on its caller's coroutine, retaining its hook.
    let factory: Function = lua.load(r#"
        return function(loader)
            local cache = {}
            return function(name)
                if cache[name] ~= nil then return cache[name] end
                local chunk = loader(name)
                cache[name] = true
                local ok, result = pcall(chunk)
                if not ok then cache[name] = nil; error(result, 0) end
                if result ~= nil then cache[name] = result end
                return cache[name]
            end
        end
    "#).eval()?;
    env.set("require", factory.call::<Function>(loader)?)?;
    Ok(env)
}

fn register_claims(lua: &Lua, env: &Table, claims: Arc<Mutex<Claims>>, script: String, open: Arc<AtomicBool>) -> mlua::Result<()> {
    let register = lua.create_table()?;
    let rpc_claims = claims.clone();
    let rpc_script = script.clone();
    let rpc_open = open.clone();
    register.set("rpc", lua.create_function(move |_, (service, method, function): (String, String, Function)| {
        if !rpc_open.load(Ordering::Acquire) { return Err(mlua::Error::runtime("registration is closed")); }
        let mut claims = rpc_claims.lock().map_err(|e| mlua::Error::runtime(e.to_string()))?;
        let key = (service, method);
        if let Some(previous) = claims.rpc.get(&key) { return Err(mlua::Error::runtime(format!("duplicate RPC {}::{} in {} and {}", key.0, key.1, previous.script, rpc_script))); }
        claims.rpc.insert(key, Handler { script: rpc_script.clone(), function });
        Ok(())
    })?)?;
    let intercept_claims = claims.clone();
    let intercept_script = script.clone();
    let intercept_open = open.clone();
    register.set("intercept", lua.create_function(move |_, (service, method, function): (String, String, Function)| {
        if !intercept_open.load(Ordering::Acquire) { return Err(mlua::Error::runtime("registration is closed")); }
        if service.is_empty() || method.is_empty() || service.contains('*') || method.contains('*') {
            return Err(mlua::Error::runtime("intercept requires an exact service and method"));
        }
        let mut claims = intercept_claims.lock().map_err(|e| mlua::Error::runtime(e.to_string()))?;
        let key = (service, method);
        if let Some(previous) = claims.intercepts.get(&key) { return Err(mlua::Error::runtime(format!("duplicate intercept {}::{} in {} and {}", key.0, key.1, previous.script, intercept_script))); }
        claims.intercepts.insert(key, Handler { script: intercept_script.clone(), function });
        Ok(())
    })?)?;
    let cmd_claims = claims.clone();
    let cmd_script = script.clone();
    let cmd_open = open.clone();
    register.set("command", lua.create_function(move |_, spec: Table| {
        if !cmd_open.load(Ordering::Acquire) { return Err(mlua::Error::runtime("registration is closed")); }
        let name: String = spec.get("name")?;
        if name.is_empty() || !name.bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_') || name == "lua" { return Err(mlua::Error::runtime("invalid or reserved command name")); }
        let command = Command { handler: Handler { script: cmd_script.clone(), function: spec.get("fn")? }, help: spec.get("help")?, gm: spec.get("gm")? };
        let mut claims = cmd_claims.lock().map_err(|e| mlua::Error::runtime(e.to_string()))?;
        if let Some(previous) = claims.commands.get(&name) { return Err(mlua::Error::runtime(format!("duplicate command {name} in {} and {cmd_script}", previous.handler.script))); }
        claims.commands.insert(name, command);
        Ok(())
    })?)?;
    let tick_claims = claims.clone();
    let tick_script = script.clone();
    let tick_open = open.clone();
    register.set("tick", lua.create_function(move |_, spec: Table| {
        if !tick_open.load(Ordering::Acquire) { return Err(mlua::Error::runtime("registration is closed")); }
        let name = match spec.get::<mlua::Value>("name")? {
            mlua::Value::String(name) => name.to_str()?.to_owned(),
            _ => return Err(mlua::Error::runtime("tick name must be a string")),
        };
        if name.is_empty() { return Err(mlua::Error::runtime("tick name must not be empty")); }
        let milliseconds = match spec.get::<mlua::Value>("interval_ms")? {
            mlua::Value::Integer(value) if value >= MIN_TICK_INTERVAL_MS as i64 => value as u64,
            _ => return Err(mlua::Error::runtime(format!("tick interval_ms must be an integer of at least {MIN_TICK_INTERVAL_MS}"))),
        };
        let interval = Duration::from_millis(milliseconds);
        if Instant::now().checked_add(interval).is_none() || tokio::time::Instant::now().checked_add(interval).is_none() {
            return Err(mlua::Error::runtime("tick interval exceeds timer range"));
        }
        let function: Function = spec.get("fn")?;
        let mut claims = tick_claims.lock().map_err(|e| mlua::Error::runtime(e.to_string()))?;
        if claims.ticks.iter().any(|tick| tick.handler.script == tick_script && tick.name == name) {
            return Err(mlua::Error::runtime(format!("duplicate tick {name} in {tick_script}")));
        }
        claims.ticks.push(Tick { handler: Handler { script: tick_script.clone(), function }, name, interval, skipped: AtomicU64::new(0) });
        Ok(())
    })?)?;
    register.set("event", lua.create_function(move |_, (name, function): (String, Function)| {
        if !open.load(Ordering::Acquire) { return Err(mlua::Error::runtime("registration is closed")); }
        if !super::events::known_kind(&name) { return Err(mlua::Error::runtime(format!("unsupported event {name}"))); }
        claims.lock().map_err(|e| mlua::Error::runtime(e.to_string()))?.events.entry(name).or_default().push(Handler { script: script.clone(), function });
        Ok(())
    })?)?;
    env.set("register", register)
}

fn discover(config: &LuaConfig) -> Result<Vec<(std::path::PathBuf, std::path::PathBuf, String)>> {
    let mut scripts = Vec::new();
    let mut names = std::collections::HashMap::new();
    for dir in std::iter::once(&config.scripts_dir).chain(config.mod_dirs.iter()) {
        let root = Path::new(dir).canonicalize().context("canonicalizing Lua scripts directory")?;
        let mut paths = std::fs::read_dir(&root)?.map(|entry| entry.map(|e| e.path())).collect::<std::io::Result<Vec<_>>>()?;
        paths.sort();
        for path in paths {
            if path.extension().is_none_or(|e| e != "lua") { continue; }
            let path = path.canonicalize()?;
            anyhow::ensure!(path.starts_with(&root), "Lua script escapes scripts directory");
            let script = path.file_stem().and_then(|s| s.to_str()).context("invalid Lua script name")?.to_owned();
            anyhow::ensure!(!script.is_empty() && script.bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_'), "invalid Lua script name {script}");
            if let Some(previous) = names.insert(script.clone(), path.clone()) {
                anyhow::bail!("duplicate Lua script {script}: {} and {}", previous.display(), path.display());
            }
            scripts.push((root.clone(), path, script));
        }
    }
    Ok(scripts)
}

pub fn build(config: &LuaConfig, resources: &host::Resources) -> Result<Registry> {
    let scripts = discover(config)?;
    let lua = Lua::new();
    lua.set_memory_limit(config.memory_limit_mb * 1024 * 1024)?;
    let claims = Arc::new(Mutex::new(Claims::default()));
    for (root, path, script) in scripts {
        let env = environment(&lua, &root)?;
        value::register(&lua, &env)?;
        let settings = config.scripts.get(&script).cloned().unwrap_or_default();
        host::register(&lua, &env, &script, &settings, resources)?;
        let open = Arc::new(AtomicBool::new(true));
        register_claims(&lua, &env, claims.clone(), script.clone(), open.clone())?;
        let result = (|| -> Result<()> {
            let bytes = std::fs::read(&path)?;
            let function = lua.load(bytes.as_slice()).set_name(path.to_string_lossy()).set_mode(ChunkMode::Text).set_environment(env.clone()).into_function()?;
            let deadline = Instant::now() + Duration::from_millis(config.call_budget_ms);
            let thread = budgeted_thread(&lua, function, config, deadline)?;
            thread.resume::<()>(())?;
            Ok(())
        })();
        open.store(false, Ordering::Release);
        result.with_context(|| format!("loading script {script}"))?;
        env.set("register", mlua::Value::Nil)?;
        claims.lock().map_err(|e| anyhow::anyhow!(e.to_string()))?.script_roots.insert(script.clone(), root);
        claims.lock().map_err(|e| anyhow::anyhow!(e.to_string()))?.scripts.push(script);
    }
    let claims = std::mem::take(&mut *claims.lock().map_err(|e| anyhow::anyhow!(e.to_string()))?);
    if !claims.rpc.is_empty() { tracing::info!("Lua RPC claims take precedence over Rust handlers"); }
    for ((service, method), handler) in &claims.rpc {
        tracing::info!(script = %handler.script, service, method, "Lua RPC claim");
    }
    for (name, command) in &claims.commands {
        tracing::info!(script = %command.handler.script, command = %name, "Lua command claim");
    }
    for ((service, method), handler) in &claims.intercepts {
        tracing::info!(script = %handler.script, service, method, "Lua intercept claim");
    }
    for (event, handlers) in &claims.events {
        for handler in handlers { tracing::info!(script = %handler.script, event, "Lua event claim"); }
    }
    for tick in &claims.ticks {
        tracing::info!(script = %tick.handler.script, name = %tick.name, interval_ms = tick.interval.as_millis(), "Lua tick claim");
    }
    Ok(Registry { lua, claims, overrides: Mutex::new(Default::default()) })
}

#[cfg(test)]
mod tests;
