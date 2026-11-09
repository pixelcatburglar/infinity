use super::*;
use std::sync::atomic::AtomicUsize;

const NAME: &str = "eve_public.dailygoal.api.GetAllCurrentRequest";

fn generation(source: &str) -> Arc<Registry> {
    let lua = mlua::Lua::new();
    let env = lua.create_table().unwrap();
    let claims = Arc::new(Mutex::new(super::super::registry::Claims::default()));
    let open = Arc::new(AtomicBool::new(true));
    vm::register_claims(&lua, &env, claims.clone(), "test".into(), open).unwrap();
    let handler: mlua::Function = lua.load(source).eval().unwrap();
    let register: mlua::Table = env.get("register").unwrap();
    register.get::<mlua::Function>("gateway").unwrap().call::<()>((NAME, handler)).unwrap();
    let claims = std::mem::take(&mut *claims.lock().unwrap());
    Arc::new(Registry { lua, claims, overrides: Mutex::new(Default::default()), content: Default::default() })
}

fn identity() -> GatewayInvocationIdentity { GatewayInvocationIdentity { user_id: Some(1), character_id: Some(2), correlation_uuid: Some([3; 16]) } }

fn operation(count: Arc<AtomicUsize>) -> GatewayOperation {
    Box::new(move || Box::pin(async move {
        count.fetch_add(1, Ordering::SeqCst);
        Ok(DynamicMessage::new(protocol::descriptors(NAME).unwrap().1))
    }))
}

#[tokio::test]
async fn only_matching_host_call_can_return_authoritative_response() {
    let config = crate::config::lua::LuaConfig::default();
    let request = DynamicMessage::new(protocol::descriptors(NAME).unwrap().0);
    for (script, expected) in [
        ("return function(c, r) return c:content_read('daily_goals') end", 1),
        ("return function(c, r) return '' end", 0),
        ("return function(c, r) return c:content_commit('daily_goals') end", 0),
        ("return function(c, r) return c:content_read('wrong') end", 0),
    ] {
        let count = Arc::new(AtomicUsize::new(0));
        let result = invoke_in_generation(generation(script), &config, NAME, &request, identity(), GatewayMode::Read, operation(count.clone())).await;
        assert_eq!(result.is_ok(), expected == 1);
        assert_eq!(count.load(Ordering::SeqCst), expected);
    }
}

#[tokio::test]
async fn valid_token_preserves_domain_refusal() {
    let config = crate::config::lua::LuaConfig::default();
    let request = DynamicMessage::new(protocol::descriptors(NAME).unwrap().0);
    let registry = generation("return function(c, r) return c:content_read('daily_goals') end");
    let operation: GatewayOperation = Box::new(|| Box::pin(async {
        Err(GatewayError::Refused { code: crate::gateway::error::FailureCode::Forbidden, message: "denied" })
    }));
    let result = invoke_in_generation(registry, &config, NAME, &request, identity(), GatewayMode::Read, operation).await;
    assert!(matches!(result, Err(GatewayError::Refused { code: crate::gateway::error::FailureCode::Forbidden, .. })));
}

#[tokio::test]
async fn cancelled_before_invocation_never_starts_operation() {
    let config = crate::config::lua::LuaConfig::default();
    let request = DynamicMessage::new(protocol::descriptors(NAME).unwrap().0);
    let registry = generation("return function(c, r) return c:content_read('daily_goals') end");
    let count = Arc::new(AtomicUsize::new(0));
    let invocation = invoke_in_generation(registry, &config, NAME, &request, identity(), GatewayMode::Read, operation(count.clone()));
    drop(invocation);
    assert_eq!(count.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn taken_operation_survives_invocation_timeout() {
    let config = crate::config::lua::LuaConfig { call_budget_ms: 1000, ..Default::default() };
    let request = DynamicMessage::new(protocol::descriptors(NAME).unwrap().0);
    let registry = generation("return function(c, r) return c:content_read('daily_goals') end");
    let (entered, entry) = tokio::sync::oneshot::channel();
    let (release, released) = tokio::sync::oneshot::channel();
    let (finish, completed) = tokio::sync::oneshot::channel();
    let operation: GatewayOperation = Box::new(|| Box::pin(async move {
        let _ = entered.send(());
        let _ = released.await;
        let _ = finish.send(());
        Ok(DynamicMessage::new(protocol::descriptors(NAME).unwrap().1))
    }));
    let invocation = invoke_in_generation(registry, &config, NAME, &request, identity(), GatewayMode::Read, operation);
    tokio::pin!(invocation);
    tokio::select! { result = entry => result.unwrap(), _ = &mut invocation => panic!("invocation ended before operation started") }
    assert!(invocation.await.is_err());
    release.send(()).unwrap();
    completed.await.unwrap();
}

#[test]
fn exact_registration_rejects_duplicates_unknown_and_late_claims() {
    let lua = mlua::Lua::new();
    let env = lua.create_table().unwrap();
    let claims = Arc::new(Mutex::new(super::super::registry::Claims::default()));
    let open = Arc::new(AtomicBool::new(true));
    vm::register_claims(&lua, &env, claims.clone(), "test".into(), open.clone()).unwrap();
    lua.load("register.gateway('eve_public.dailygoal.api.GetAllCurrentRequest', function() end)").set_environment(env.clone()).exec().unwrap();
    assert!(lua.load("register.gateway('eve_public.dailygoal.api.GetAllCurrentRequest', function() end)").set_environment(env.clone()).exec().is_err());
    assert!(lua.load("register.gateway('eve_public.dailygoal.api.FutureRequest', function() end)").set_environment(env.clone()).exec().is_err());
    open.store(false, Ordering::Release);
    assert!(lua.load("register.gateway('eve_public.dailygoal.api.GetRequest', function() end)").set_environment(env).exec().is_err());
    assert_eq!(claims.lock().unwrap().gateway.len(), 1);
}

#[tokio::test]
async fn real_lua_returns_full_native_daily_response() {
    let config = crate::config::lua::LuaConfig::default();
    let (request_desc, response_desc) = protocol::descriptors(NAME).unwrap();
    let request = DynamicMessage::new(request_desc);
    let text = r#"
        goals {
          id { uuid: "\001\002\003\004\005\006\007\010\011\012\013\014\015\016\017\020" }
          goal { target: 18446744073709551615 active_after { seconds: 123 nanos: 456 } omega: false }
          current_progress: 18446744073709551615
          earnings { unit { isk { amount { units: 17 nanos: 42 } } } omega_required: false }
          earnings { unit { lp { amount { amount: 18446744073709551615 associated_corporation { sequential: 99 } } } } }
          earnings { unit { sp { amount: 123 } } }
          earnings { unit { item { amount: 2 type { sequential: 456 } } } }
          earnings { unit { plex { amount { total_in_cents: 789 } } } }
        }
    "#;
    let expected = DynamicMessage::parse_text_format(response_desc, text).unwrap();
    let response = expected.clone();
    let operation: GatewayOperation = Box::new(move || Box::pin(async move { Ok(response) }));
    let registry = generation("return function(c, r) return c:content_read('daily_goals') end");
    assert_eq!(invoke_in_generation(registry, &config, NAME, &request, identity(), GatewayMode::Read, operation).await.unwrap(), expected);
}

#[tokio::test]
async fn forged_response_after_host_call_is_rejected() {
    let config = crate::config::lua::LuaConfig::default();
    let request = DynamicMessage::new(protocol::descriptors(NAME).unwrap().0);
    for script in [
        "return function(c, r) c:content_read('daily_goals'); return '' end",
        "return function(c, r) c:content_read('daily_goals'); error('after admission') end",
        "return function(c, r) c:content_read('daily_goals'); return c:content_read('daily_goals') end",
        "return function(c, r) local t = c:content_read('daily_goals'); pcall(function() c:content_commit('daily_goals') end); return t end",
        "return function(c, r) local t = c:content_read('daily_goals'); pcall(function() c:content_read({}) end); return t end",
        "return function(c, r) return c:content_read('daily_goals'), 'extra' end",
    ] {
        let count = Arc::new(AtomicUsize::new(0));
        assert!(invoke_in_generation(generation(script), &config, NAME, &request, identity(), GatewayMode::Read, operation(count.clone())).await.is_err());
        assert_eq!(count.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn retained_userdata_expires_after_invocation() {
    let config = crate::config::lua::LuaConfig::default();
    let request = DynamicMessage::new(protocol::descriptors(NAME).unwrap().0);
    let registry = generation("return function(c, r) saved = c; return c:content_read('daily_goals') end");
    let count = Arc::new(AtomicUsize::new(0));
    invoke_in_generation(registry.clone(), &config, NAME, &request, identity(), GatewayMode::Read, operation(count)).await.unwrap();
    assert!(registry.lua.load("return saved.user_id").eval::<mlua::Value>().is_err());
}

#[tokio::test]
async fn second_host_call_cannot_run_operation_again() {
    let config = crate::config::lua::LuaConfig::default();
    let request = DynamicMessage::new(protocol::descriptors(NAME).unwrap().0);
    let registry = generation("return function(c, r) local x = c:content_read('daily_goals'); pcall(function() c:content_read('daily_goals') end); return x end");
    let count = Arc::new(AtomicUsize::new(0));
    let result = invoke_in_generation(registry, &config, NAME, &request, identity(), GatewayMode::Read, operation(count.clone())).await;
    assert!(result.is_err());
    assert_eq!(count.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn previous_invocation_token_and_context_cannot_authorize_another() {
    let config = crate::config::lua::LuaConfig::default();
    let request = DynamicMessage::new(protocol::descriptors(NAME).unwrap().0);
    for script in [
        "return function(c) if saved then c:content_read('daily_goals'); return saved end; saved = c:content_read('daily_goals'); return saved end",
        "return function(c) if saved then return saved:content_read('daily_goals') end; saved = c; return c:content_read('daily_goals') end",
    ] {
        let registry = generation(script);
        let count = Arc::new(AtomicUsize::new(0));
        assert!(invoke_in_generation(registry.clone(), &config, NAME, &request, identity(), GatewayMode::Read, operation(count.clone())).await.is_ok());
        assert!(invoke_in_generation(registry, &config, NAME, &request, identity(), GatewayMode::Read, operation(count.clone())).await.is_err());
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn original_identity_and_correlation_are_exposed_losslessly() {
    let config = crate::config::lua::LuaConfig::default();
    let request = DynamicMessage::new(protocol::descriptors(NAME).unwrap().0);
    let registry = generation("return function(c, r) assert(c.user_id == 1 and c.character_id == 2 and c.correlation_uuid == string.rep(string.char(3), 16)); return c:content_read('daily_goals') end");
    let count = Arc::new(AtomicUsize::new(0));
    assert!(invoke_in_generation(registry, &config, NAME, &request, identity(), GatewayMode::Read, operation(count)).await.is_ok());
}

#[tokio::test]
async fn rejected_candidate_does_not_replace_old_generation() {
    let old = generation("return function(c, r) return c:content_read('daily_goals') end");
    let lua = mlua::Lua::new();
    let env = lua.create_table().unwrap();
    let claims = Arc::new(Mutex::new(super::super::registry::Claims::default()));
    vm::register_claims(&lua, &env, claims, "candidate".into(), Arc::new(AtomicBool::new(true))).unwrap();
    assert!(lua.load("register.gateway('eve_public.dailygoal.api.FutureRequest', function() end)").set_environment(env).exec().is_err());
    let request = DynamicMessage::new(protocol::descriptors(NAME).unwrap().0);
    let count = Arc::new(AtomicUsize::new(0));
    assert!(invoke_in_generation(old, &crate::config::lua::LuaConfig::default(), NAME, &request, identity(), GatewayMode::Read, operation(count)).await.is_ok());
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn failed_gateway_reload_preserves_active_registry() {
    use crate::{lua::{host::Resources, reload::Host}, sde::Sde, space::World};
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let world = World::new(db.clone(), sde.clone(), 1);
    let hub = crate::chat::Hub::new("test", db.clone(), world.clone());
    let pilots = crate::lua::host::pilots::Pilots::new(&db, hub).await.unwrap();
    let resources = Resources { db, sde, world, pilots, store_value_limit_bytes: 65536 };
    let dir = std::env::temp_dir().join(format!("gateway-reload-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    std::fs::create_dir(&dir).unwrap();
    let script = dir.join("test.lua");
    std::fs::write(&script, "register.gateway('eve_public.dailygoal.api.GetAllCurrentRequest', function(c, r) return c:content_read('daily_goals') end)").unwrap();
    let config = crate::config::lua::LuaConfig { scripts_dir: dir.to_string_lossy().into_owned(), ..Default::default() };
    let host = Host::new(config, resources).await.unwrap();
    let old = host.snapshot();
    for source in [
        "register.gateway('eve_public.dailygoal.api.FutureRequest', function() end)",
        "register.gateway('eve_public.dailygoal.api.GetAllCurrentRequest', function() end); register.gateway('eve_public.dailygoal.api.GetAllCurrentRequest', function() end)",
    ] {
        std::fs::write(&script, source).unwrap();
        assert!(host.reload().await.is_err());
        assert!(Arc::ptr_eq(&old, &host.snapshot()));
    }
    let request = DynamicMessage::new(protocol::descriptors(NAME).unwrap().0);
    let count = Arc::new(AtomicUsize::new(0));
    assert!(invoke_in_generation(host.snapshot(), &host.config, NAME, &request, identity(), GatewayMode::Read, operation(count)).await.is_ok());
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn caught_wrong_mode_never_takes_operation() {
    let config = crate::config::lua::LuaConfig::default();
    let request = DynamicMessage::new(protocol::descriptors(NAME).unwrap().0);
    let registry = generation("return function(c, r) pcall(function() c:content_commit('daily_goals') end); return '' end");
    let count = Arc::new(AtomicUsize::new(0));
    assert!(invoke_in_generation(registry, &config, NAME, &request, identity(), GatewayMode::Read, operation(count.clone())).await.is_err());
    assert_eq!(count.load(Ordering::SeqCst), 0);
}
