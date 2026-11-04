use super::*;

#[test]
fn sandbox_hides_native_and_budget_escape_paths() -> mlua::Result<()> {
    let lua = Lua::new();
    let root = std::env::current_dir().unwrap().canonicalize().unwrap();
    let env = environment(&lua, &root, LuaConfig::default().data_file_limit_bytes)?;
    let safe: bool = lua.load("return io == nil and debug == nil and package == nil and load == nil and loadfile == nil and dofile == nil and coroutine == nil and type(pcall) == 'function' and type(xpcall) == 'function' and getmetatable == nil and string.dump == nil and os.execute == nil and type(os.time) == 'function'").set_environment(env.clone()).eval()?;
    assert!(safe);
    assert!(lua.load("return require('../outside')").set_environment(env.clone()).eval::<mlua::Value>().is_err());
    assert!(lua.load("collectgarbage('stop')").set_environment(env).exec().is_err());
    Ok(())
}

#[test]
fn instruction_budget_aborts_and_vm_survives() -> mlua::Result<()> {
    let lua = Lua::new();
    let config = LuaConfig { call_budget_instructions: 1000, instruction_hook_interval: 100, ..LuaConfig::default() };
    let loop_fn = lua.load("while true do end").into_function()?;
    assert!(budgeted_thread(&lua, loop_fn, &config, Instant::now() + Duration::from_secs(60))?.resume::<()>(()).is_err());
    assert_eq!(lua.load("return 42").eval::<i64>()?, 42);
    Ok(())
}

#[test]
fn memory_limit_is_enforced() -> mlua::Result<()> {
    let lua = Lua::new();
    lua.set_memory_limit(lua.used_memory() + 64 * 1024)?;
    assert!(lua.load("return string.rep('x', 1024 * 1024)").eval::<mlua::Value>().is_err());
    Ok(())
}

#[test]
fn conflicts_name_both_scripts() -> mlua::Result<()> {
    let lua = Lua::new();
    let claims = Arc::new(Mutex::new(Claims::default()));
    let a = lua.create_table()?;
    let b = lua.create_table()?;
    register_claims(&lua, &a, claims.clone(), "first.lua".into(), Arc::new(AtomicBool::new(true)))?;
    register_claims(&lua, &b, claims, "second.lua".into(), Arc::new(AtomicBool::new(true)))?;
    lua.load("register.rpc('svc', 'method', function() end)").set_environment(a).exec()?;
    let error = lua.load("register.rpc('svc', 'method', function() end)").set_environment(b).exec().unwrap_err().to_string();
    assert!(error.contains("first.lua") && error.contains("second.lua"));
    Ok(())
}

#[test]
fn required_module_runs_under_callers_instruction_budget() -> mlua::Result<()> {
    let dir = std::env::temp_dir().join(format!("lua-require-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    std::fs::create_dir(&dir).unwrap();
    std::fs::write(dir.join("loop.lua"), "local x = 0; for i = 1, 10000000 do x = x + i end; return x").unwrap();
    let lua = Lua::new();
    let env = environment(&lua, &dir.canonicalize().unwrap(), LuaConfig::default().data_file_limit_bytes)?;
    let config = LuaConfig { call_budget_instructions: 1000, instruction_hook_interval: 100, ..LuaConfig::default() };
    let function = lua.load("return require('loop')").set_environment(env).into_function()?;
    let result = budgeted_thread(&lua, function, &config, Instant::now() + Duration::from_secs(60))?.resume::<mlua::Value>(());
    std::fs::remove_dir_all(dir).unwrap();
    assert!(result.unwrap_err().downcast_ref::<BudgetExceeded>().is_some());
    Ok(())
}

#[test]
fn protected_calls_preserve_results_and_ordinary_errors() -> mlua::Result<()> {
    let lua = Lua::new();
    let env = environment(&lua, &std::env::current_dir().unwrap(), LuaConfig::default().data_file_limit_bytes)?;
    lua.load(r#"
        local ok, err = pcall(function() error('ordinary') end)
        assert(not ok and string.find(err, 'ordinary', 1, true))
        local values = table.pack(pcall(function() return 1, nil, 3, nil end))
        assert(values.n == 5 and values[1] and values[2] == 1 and values[3] == nil and values[4] == 3 and values[5] == nil)
        local called = false
        local success, result = xpcall(function() error('bad') end, function(e) called = true; return 'handled:' .. e end)
        assert(not success and called and string.find(result, 'handled:', 1, true))
        local failed, handler_error = xpcall(function() error('bad') end, function() error('handler failed') end)
        assert(not failed and handler_error == 'error in error handling')
        local ran = false
        local valid = pcall(function() xpcall(function() ran = true end, nil) end)
        assert(not valid and not ran)
        local same, message = pcall(function() error('Lua execution budget exceeded', 0) end)
        assert(not same and message == 'Lua execution budget exceeded')
        local p, x = pcall, xpcall
        table.pack = nil; table.unpack = nil; error = nil; pcall = nil; xpcall = nil
        local ok2, a, b, c = p(function() return 1, nil, 3 end)
        assert(ok2 and a == 1 and b == nil and c == 3)
        assert(x(function() return 7 end, function(e) return e end))
    "#).set_environment(env).exec()
}

#[test]
fn protected_calls_cannot_catch_typed_budget_errors() -> mlua::Result<()> {
    let lua = Lua::new();
    let env = environment(&lua, &std::env::current_dir().unwrap(), LuaConfig::default().data_file_limit_bytes)?;
    let config = LuaConfig { call_budget_instructions: 1000, instruction_hook_interval: 100, ..LuaConfig::default() };
    for source in [
        "pcall(function() while true do end end)",
        "xpcall(function() pcall(function() while true do end end) end, function(e) intercepted = true; return 'replaced' end)",
        "pcall(function() xpcall(function() error('ordinary') end, function() while true do end end) end)",
    ] {
        let function = lua.load(source).set_environment(env.clone()).into_function()?;
        let error = budgeted_thread(&lua, function, &config, Instant::now() + Duration::from_secs(60))?.resume::<()>(()).unwrap_err();
        assert!(error.downcast_ref::<BudgetExceeded>().is_some());
        assert!(!env.get::<Option<bool>>("intercepted")?.unwrap_or(false));
    }
    Ok(())
}

#[test]
fn protected_handler_budget_at_each_instruction_boundary() -> mlua::Result<()> {
    let lua = Lua::new();
    let env = environment(&lua, &std::env::current_dir().unwrap(), LuaConfig::default().data_file_limit_bytes)?;
    for maximum in 1..=128 {
        let config = LuaConfig { call_budget_instructions: maximum, instruction_hook_interval: 1, ..LuaConfig::default() };
        let function = lua.load("pcall(function() xpcall(function() error('ordinary') end, function() while true do end end) end)").set_environment(env.clone()).into_function()?;
        let error = budgeted_thread(&lua, function, &config, Instant::now() + Duration::from_secs(60))?.resume::<()>(()).unwrap_err();
        assert!(error.downcast_ref::<BudgetExceeded>().is_some(), "instruction budget {maximum}: {error}");
    }
    Ok(())
}

#[test]
fn caught_memory_exhaustion_keeps_allocator_limit() -> mlua::Result<()> {
    let lua = Lua::new();
    let env = environment(&lua, &std::env::current_dir().unwrap(), LuaConfig::default().data_file_limit_bytes)?;
    let limit = lua.used_memory() + 256 * 1024;
    lua.set_memory_limit(limit)?;
    let (first, second, used): (bool, bool, f64) = lua.load(r#"
        local first = pcall(function() return string.rep('x', 1024 * 1024) end)
        local second = pcall(function() return string.rep('y', 1024 * 1024) end)
        return first, second, collectgarbage('count')
    "#).set_environment(env).eval()?;
    assert!(!first && !second);
    assert!(used * 1024.0 <= limit as f64);
    Ok(())
}

#[test]
fn required_module_uses_safe_protected_calls() -> mlua::Result<()> {
    let dir = std::env::temp_dir().join(format!("lua-protected-module-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    std::fs::create_dir(&dir).unwrap();
    std::fs::write(dir.join("module.lua"), "return pcall(function() return 42 end) and type(xpcall) == 'function'").unwrap();
    let lua = Lua::new();
    let env = environment(&lua, &dir.canonicalize().unwrap(), LuaConfig::default().data_file_limit_bytes)?;
    let result = lua.load("return require('module')").set_environment(env).eval::<bool>();
    std::fs::remove_dir_all(dir).unwrap();
    assert!(result?);
    Ok(())
}

#[tokio::test]
async fn protected_calls_yield_and_retain_invocation_budget() -> mlua::Result<()> {
    let lua = Lua::new();
    let env = environment(&lua, &std::env::current_dir().unwrap(), LuaConfig::default().data_file_limit_bytes)?;
    env.set("wait", lua.create_async_function(|_, ()| async {
        tokio::task::yield_now().await;
        Ok(42)
    })?)?;
    let config = LuaConfig { call_budget_instructions: 1000, instruction_hook_interval: 100, ..LuaConfig::default() };
    for source in [
        "local ok, n = pcall(wait); assert(ok and n == 42); return n",
        "local ok, n = xpcall(wait, function(e) return e end); assert(ok and n == 42); return n",
    ] {
        let handler = lua.load(source).set_environment(env.clone()).into_function()?;
        let invocation = budgeted_invocation(&lua, handler, &config, Instant::now() + Duration::from_secs(60))?;
        let _guard = super::super::HookGuard(invocation.hook_thread);
        assert_eq!(invocation.function.call_async::<i64>(()).await?, 42);
    }
    let handler = lua.load("pcall(function() wait(); while true do end end)").set_environment(env).into_function()?;
    let invocation = budgeted_invocation(&lua, handler, &config, Instant::now() + Duration::from_secs(60))?;
    let _guard = super::super::HookGuard(invocation.hook_thread);
    let error = invocation.function.call_async::<()>(()).await.unwrap_err();
    assert!(error.downcast_ref::<BudgetExceeded>().is_some());
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn protected_async_call_obeys_no_context_deadline() -> mlua::Result<()> {
    let lua = Lua::new();
    let env = environment(&lua, &std::env::current_dir().unwrap(), LuaConfig::default().data_file_limit_bytes)?;
    env.set("wait", lua.create_async_function(|_, ()| async {
        std::future::pending::<mlua::Result<()>>().await
    })?)?;
    let function = lua.load("return function() pcall(wait); resumed = true end").set_environment(env.clone()).eval()?;
    let handler = Handler { script: "test".into(), function };
    let registry = Registry { lua, claims: Claims::default(), overrides: Mutex::new(Default::default()) };
    let config = LuaConfig { call_budget_ms: 100, ..LuaConfig::default() };
    let call = super::super::invoke_no_context(&registry, &config, &handler, Ok(mlua::Value::Nil));
    tokio::pin!(call);
    tokio::select! { biased; _ = &mut call => panic!("completed before deadline"), _ = tokio::task::yield_now() => {} }
    tokio::time::advance(Duration::from_millis(100)).await;
    let error = call.await.unwrap_err();
    assert!(error.downcast_ref::<BudgetExceeded>().is_some());
    assert!(env.get::<Option<bool>>("resumed")?.is_none());
    Ok(())
}

#[test]
fn retained_registration_is_closed_for_all_claims() -> mlua::Result<()> {
    let lua = Lua::new();
    let env = environment(&lua, &std::env::current_dir().unwrap(), LuaConfig::default().data_file_limit_bytes)?;
    let claims = Arc::new(Mutex::new(Claims::default()));
    let open = Arc::new(AtomicBool::new(true));
    register_claims(&lua, &env, claims.clone(), "test".into(), open.clone())?;
    lua.load("saved = register; register.rpc('s', 'm', function() saved.rpc('s', 'other', function() end) end)").set_environment(env.clone()).exec()?;
    open.store(false, Ordering::Release);
    let handler = claims.lock().unwrap().rpc.get(&("s".into(), "m".into())).unwrap().function.clone();
    assert!(handler.call::<()>(()).unwrap_err().to_string().contains("registration is closed"));
    for expression in [
        "saved.command{name='other', help='', gm=false, fn=function() end}",
        "saved.event('BallparkBound', function() end)",
        "saved.intercept('s', 'm', function() end)",
        "saved.tick{name='late', interval_ms=100, fn=function() end}",
    ] {
        assert!(lua.load(expression).set_environment(env.clone()).exec().unwrap_err().to_string().contains("registration is closed"));
    }
    Ok(())
}

#[test]
fn event_and_intercept_registration_validate_claims() -> mlua::Result<()> {
    let lua = Lua::new();
    let claims = Arc::new(Mutex::new(Claims::default()));
    let a = lua.create_table()?;
    let b = lua.create_table()?;
    register_claims(&lua, &a, claims.clone(), "first".into(), Arc::new(AtomicBool::new(true)))?;
    register_claims(&lua, &b, claims.clone(), "second".into(), Arc::new(AtomicBool::new(true)))?;
    lua.load("register.event('Docked', function() end); register.event('Docked', function() end); register.rpc('s','m',function() end); register.intercept('s','m',function() return true end)").set_environment(a.clone()).exec()?;
    assert_eq!(claims.lock().unwrap().events["Docked"].len(), 2);
    for kind in ["CharacterSelected", "CharacterLoggedOff", "Undocked", "Docked", "Jumped", "ShipLost", "NpcKilled", "ItemsMoved", "WalletChanged", "SkillTrained", "MarketOrderPlaced", "MarketOrderFilled", "MissionAccepted", "MissionCompleted", "BallparkBound"] {
        let register: Table = a.get("register")?;
        let event: Function = register.get("event")?;
        event.call::<()>((kind, lua.create_function(|_, ()| Ok(()))?))?;
    }
    assert!(lua.load("register.event('Unknown', function() end)").set_environment(a.clone()).exec().is_err());
    assert!(lua.load("register.intercept('*','m', function() end)").set_environment(a).exec().is_err());
    let error = lua.load("register.intercept('s','m', function() end)").set_environment(b).exec().unwrap_err().to_string();
    assert!(error.contains("first") && error.contains("second"));
    Ok(())
}

#[test]
fn tick_registration_validates_and_scopes_names() -> mlua::Result<()> {
    let lua = Lua::new();
    let claims = Arc::new(Mutex::new(Claims::default()));
    let a = lua.create_table()?;
    let b = lua.create_table()?;
    register_claims(&lua, &a, claims.clone(), "first".into(), Arc::new(AtomicBool::new(true)))?;
    register_claims(&lua, &b, claims.clone(), "second".into(), Arc::new(AtomicBool::new(true)))?;
    for spec in [
        "name='', interval_ms=100, fn=function() end",
        "name='a', interval_ms=99, fn=function() end",
        "name='a', interval_ms=100.5, fn=function() end",
        "name='a', interval_ms='100', fn=function() end",
        "name='a', interval_ms=100, fn=1",
    ] {
        assert!(lua.load(format!("register.tick{{{spec}}}")).set_environment(a.clone()).exec().is_err(), "{spec}");
    }
    let source = "register.tick{name='a', interval_ms=100, fn=function() end}";
    lua.load(source).set_environment(a.clone()).exec()?;
    assert!(lua.load(source).set_environment(a).exec().is_err());
    lua.load(source).set_environment(b).exec()?;
    assert_eq!(claims.lock().unwrap().ticks.len(), 2);
    Ok(())
}

#[tokio::test]
async fn instruction_budgets_are_isolated_across_awaits() -> mlua::Result<()> {
    let lua = Lua::new();
    let (sender, receiver) = tokio::sync::oneshot::channel::<()>();
    let receiver = Arc::new(tokio::sync::Mutex::new(Some(receiver)));
    lua.globals().set("wait", lua.create_async_function(move |_, ()| {
        let receiver = receiver.clone();
        async move { receiver.lock().await.take().unwrap().await.map_err(mlua::Error::external) }
    })?)?;
    let config = LuaConfig { call_budget_instructions: 1000, instruction_hook_interval: 100, call_budget_ms: 60_000, ..LuaConfig::default() };
    let pending = budgeted_thread(&lua, lua.load("wait(); return 42").into_function()?, &config, Instant::now() + Duration::from_secs(60))?.into_async::<i64>(())?;
    tokio::pin!(pending);
    tokio::select! { biased; _ = &mut pending => panic!("completed before release"), _ = tokio::task::yield_now() => {} }
    let excessive = budgeted_thread(&lua, lua.load("while true do end").into_function()?, &config, Instant::now() + Duration::from_secs(60))?;
    assert!(excessive.resume::<()>(()).is_err());
    sender.send(()).unwrap();
    assert_eq!(pending.await?, 42);
    Ok(())
}

#[tokio::test]
async fn async_invocation_budgets_are_isolated_across_awaits() -> mlua::Result<()> {
    let lua = Lua::new();
    let (sender, receiver) = tokio::sync::oneshot::channel::<()>();
    let receiver = Arc::new(tokio::sync::Mutex::new(Some(receiver)));
    lua.globals().set("wait", lua.create_async_function(move |_, ()| {
        let receiver = receiver.clone();
        async move { receiver.lock().await.take().unwrap().await.map_err(mlua::Error::external) }
    })?)?;
    let config = LuaConfig { call_budget_instructions: 1000, instruction_hook_interval: 100, call_budget_ms: 60_000, ..LuaConfig::default() };
    let deadline = Instant::now() + Duration::from_secs(60);
    let pending = budgeted_invocation(&lua, lua.load("wait(); return 42").into_function()?, &config, deadline)?;
    let call = pending.function.call_async::<i64>(());
    tokio::pin!(call);
    let _guard = super::super::HookGuard(pending.hook_thread);
    tokio::select! { biased; _ = &mut call => panic!("completed before release"), _ = tokio::task::yield_now() => {} }
    {
        let excessive = budgeted_invocation(&lua, lua.load("while true do end").into_function()?, &config, deadline)?;
        let loop_call = excessive.function.call_async::<()>(());
        tokio::pin!(loop_call);
        let _guard = super::super::HookGuard(excessive.hook_thread);
        assert!(loop_call.await.unwrap_err().to_string().contains("Lua execution budget exceeded"));
    }
    sender.send(()).unwrap();
    assert_eq!(call.await?, 42);
    Ok(())
}

#[test]
fn bundled_reference_scripts_return_expected_wire_values() -> mlua::Result<()> {
    use marshal::Value as Wire;
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("lua").canonicalize().unwrap();
    let lua = Lua::new();
    let env = environment(&lua, &root, LuaConfig::default().data_file_limit_bytes)?;
    value::register(&lua, &env)?;
    let claims = Arc::new(Mutex::new(Claims::default()));
    let open = Arc::new(AtomicBool::new(true));
    register_claims(&lua, &env, claims.clone(), "fixed_answers".into(), open.clone())?;
    let source = std::fs::read(root.join("fixed_answers.lua")).unwrap();
    lua.load(source.as_slice()).set_environment(env.clone()).exec()?;
    open.store(false, Ordering::Release);
    let claims = claims.lock().unwrap();
    assert_eq!(claims.rpc.len(), 5);
    let call = |service: &str, method: &str| -> mlua::Result<Wire> {
        let function = &claims.rpc.get(&(service.into(), method.into())).unwrap().function;
        value::from_lua(function.call::<mlua::Value>(())?)
    };
    assert_eq!(call("characterRenameMgr", "GetDetails")?, Wire::dict([("byGM", Wire::Int(0))]));
    assert_eq!(call("localizationServer", "GetAllTextChanges")?, Wire::None);
    assert_eq!(call("localizationServer", "UpdateLocalizationQAWrap")?, Wire::None);
    assert_eq!(call("eveacsMgr", "IsCanary")?, Wire::Bool(false));
    let Wire::Instance(class, state) = call("infoGatheringMgr", "GetStateAndConfig")? else { panic!("expected KeyVal") };
    assert_eq!(*class, Wire::str("utillib.KeyVal"));
    let Wire::Dict(fields) = *state else { panic!("expected KeyVal state") };
    for (name, expected) in [
        ("isEnabled", Wire::Bool(false)), ("infoTypes", Wire::List(vec![])),
        ("clientWorkerInterval", Wire::Int(0)), ("infoTypesOncePerRun", Wire::List(vec![])),
        ("infoTypeAggregates", Wire::Dict(vec![])), ("infoTypeParameters", Wire::Dict(vec![])),
    ] {
        assert_eq!(fields.iter().find(|(key, _)| *key == Wire::str(name)).map(|(_, value)| value), Some(&expected));
    }
    assert_eq!(fields.len(), 6);
    Ok(())
}

#[test]
fn bundled_motd_uses_current_command_and_notification_protocol() -> mlua::Result<()> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("lua").canonicalize().unwrap();
    let lua = Lua::new();
    let env = environment(&lua, &root, LuaConfig::default().data_file_limit_bytes)?;
    value::register(&lua, &env)?;
    let claims = Arc::new(Mutex::new(Claims::default()));
    let open = Arc::new(AtomicBool::new(true));
    register_claims(&lua, &env, claims.clone(), "motd".into(), open.clone())?;
    let messages = lua.create_table()?;
    messages.set("motd_help", crate::messages::lua::LUA_MOTD_HELP)?;
    messages.set("motd_saved", crate::messages::lua::LUA_MOTD_SAVED)?;
    messages.set("motd_empty", crate::messages::lua::LUA_MOTD_EMPTY)?;
    messages.set("custom_notify", crate::messages::MSG_CUSTOM_NOTIFY)?;
    env.set("messages", messages)?;
    lua.load("local data = {}; store = { get = function(k) return data[k] end, set = function(k,v) data[k]=v end }; ctx = {}; world = { notify_pilot = function(char_id, event, args) notification = {char_id, event, args[1], args[2]} end }").set_environment(env.clone()).exec()?;
    let source = std::fs::read(root.join("motd.lua")).unwrap();
    lua.load(source.as_slice()).set_environment(env.clone()).exec()?;
    open.store(false, Ordering::Release);
    let claims = claims.lock().unwrap();
    let command = &claims.commands.get("motd").unwrap();
    assert!(command.gm);
    let ctx: Table = env.get("ctx")?;
    assert_eq!(command.handler.function.call::<String>((ctx.clone(), lua.create_table()?))?, crate::messages::lua::LUA_MOTD_EMPTY);
    let args = lua.create_sequence_from(["set", "测试", "公告"])?;
    assert_eq!(command.handler.function.call::<String>((ctx.clone(), args))?, crate::messages::lua::LUA_MOTD_SAVED);
    assert_eq!(command.handler.function.call::<String>((ctx.clone(), lua.create_table()?))?, "测试 公告");
    let event = super::super::events::GameEvent::CharacterSelected { user_id: 11, char_id: 22 };
    claims.events["CharacterSelected"][0].function.call::<()>(event.to_lua(&lua)?)?;
    let notification: Table = env.get("notification")?;
    assert_eq!(notification.get::<i64>(1)?, 22);
    assert_eq!(notification.get::<String>(2)?, "OnRemoteMessage");
    assert_eq!(value::from_lua(notification.get(3)?)?, marshal::Value::str(crate::messages::MSG_CUSTOM_NOTIFY));
    assert_eq!(value::from_lua(notification.get(4)?)?, marshal::Value::dict([("notify", marshal::Value::unicode("测试 公告"))]));
    Ok(())
}

#[test]
fn roots_are_ordered_and_duplicate_names_report_both_paths() {
    let dir = std::env::temp_dir().join(format!("lua-roots-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    let primary = dir.join("primary");
    let extra = dir.join("extra");
    std::fs::create_dir_all(&primary).unwrap();
    std::fs::create_dir(&extra).unwrap();
    std::fs::write(primary.join("z.lua"), "").unwrap();
    std::fs::write(primary.join("a.lua"), "").unwrap();
    std::fs::write(extra.join("b.lua"), "").unwrap();
    let config = LuaConfig { scripts_dir: primary.to_string_lossy().into_owned(), mod_dirs: vec![extra.to_string_lossy().into_owned()], ..Default::default() };
    let scripts = discover(&config).unwrap();
    assert_eq!(scripts.iter().map(|(_, _, name)| name.as_str()).collect::<Vec<_>>(), ["a", "z", "b"]);
    assert_eq!(scripts[0].0, primary.canonicalize().unwrap());
    assert_eq!(scripts[2].0, extra.canonicalize().unwrap());
    std::fs::write(extra.join("a.lua"), "").unwrap();
    let error = discover(&config).unwrap_err().to_string();
    assert!(error.contains(&primary.join("a.lua").display().to_string()));
    assert!(error.contains(&extra.join("a.lua").display().to_string()));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn requires_are_isolated_by_root_and_reject_escape() -> mlua::Result<()> {
    let dir = std::env::temp_dir().join(format!("lua-modules-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    let first = dir.join("first");
    let second = dir.join("second");
    std::fs::create_dir_all(first.join("lib")).unwrap();
    std::fs::create_dir_all(second.join("lib")).unwrap();
    std::fs::write(first.join("lib/shared.lua"), "return 'first'").unwrap();
    std::fs::write(second.join("lib/shared.lua"), "return 'second'").unwrap();
    let lua = Lua::new();
    let a = environment(&lua, &first.canonicalize().unwrap(), LuaConfig::default().data_file_limit_bytes)?;
    let b = environment(&lua, &second.canonicalize().unwrap(), LuaConfig::default().data_file_limit_bytes)?;
    assert_eq!(lua.load("return require('lib.shared')").set_environment(a.clone()).eval::<String>()?, "first");
    assert_eq!(lua.load("return require('lib.shared')").set_environment(b.clone()).eval::<String>()?, "second");
    assert!(lua.load("return require('../second/lib/shared')").set_environment(a).eval::<String>().is_err());
    assert!(lua.load("return require('missing')").set_environment(b).eval::<String>().is_err());
    std::fs::remove_dir_all(dir).unwrap();
    Ok(())
}

#[cfg(unix)]
#[test]
fn require_and_script_symlinks_cannot_escape_their_root() -> mlua::Result<()> {
    use std::os::unix::fs::symlink;
    let dir = std::env::temp_dir().join(format!("lua-escape-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    let root = dir.join("root");
    std::fs::create_dir_all(&root).unwrap();
    let outside = dir.join("outside.lua");
    std::fs::write(&outside, "return 42").unwrap();
    symlink(&outside, root.join("escape.lua")).unwrap();
    let lua = Lua::new();
    let env = environment(&lua, &root, LuaConfig::default().data_file_limit_bytes)?;
    let require: Function = env.get("require")?;
    assert!(require.call::<mlua::Value>("escape").is_err());
    let config = LuaConfig { scripts_dir: root.to_string_lossy().into_owned(), ..Default::default() };
    assert!(discover(&config).unwrap_err().to_string().contains("escapes"));
    std::fs::remove_dir_all(dir).unwrap();
    Ok(())
}

#[path = "data/tests.rs"]
mod data;
