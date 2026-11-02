use super::*;

mod acceptance;
mod budget;
mod fixture;
mod roots;

#[tokio::test]
async fn event_handlers_continue_in_order_after_errors_and_budget_exhaustion() -> mlua::Result<()> {
    let lua = mlua::Lua::new();
    lua.globals().set("seen", lua.create_table()?)?;
    let mut claims = registry::Claims::default();
    let mut handlers = Vec::new();
    for source in ["return function(ev) table.insert(seen, ev.char_id); error('failed') end", "return function(ev) while true do end end", "return function(ev) table.insert(seen, ev.system_id) end"] {
        handlers.push(Handler { script: "test".into(), function: lua.load(source).eval()? });
    }
    claims.events.insert("BallparkBound".into(), handlers);
    let registry = Registry { lua, claims, overrides: std::sync::Mutex::new(Default::default()) };
    let config = crate::config::lua::LuaConfig { call_budget_instructions: 1000, instruction_hook_interval: 100, ..Default::default() };
    for _ in 0..2 {
        invoke_event(&registry, &config, &events::GameEvent::BallparkBound { char_id: 3, system_id: 9 }).await;
    }
    let seen: mlua::Table = registry.lua.globals().get("seen")?;
    assert_eq!(seen.len()?, 4);
    for (index, expected) in [(1, 3), (2, 9), (3, 3), (4, 9)] {
        assert_eq!(seen.get::<i64>(index)?, expected);
    }
    Ok(())
}

#[test]
fn intercept_results_allow_deny_and_reject_invalid_shapes() -> mlua::Result<()> {
    let lua = mlua::Lua::new();
    let allow: mlua::MultiValue = lua.load("return true").eval()?;
    assert!(intercept_result(allow).is_ok());
    let deny: mlua::MultiValue = lua.load("return false, 'Refused'").eval()?;
    match intercept_result(deny) {
        Err(CallError::User { msg, dict }) => { assert_eq!(msg, "Refused"); assert!(dict.is_empty()); }
        _ => panic!("expected user refusal"),
    }
    for source in ["return false", "return true, 'extra'", "return 'true'", "return false, 42"] {
        let values: mlua::MultiValue = lua.load(source).eval()?;
        assert!(matches!(intercept_result(values), Err(CallError::Internal(_))));
    }
    Ok(())
}
