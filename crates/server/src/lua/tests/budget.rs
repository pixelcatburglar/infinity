//! Real routing regression for Lua instruction budgets.

use super::super::*;

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE; run alone"]
async fn rpc_and_command_budget_abort_and_later_calls_succeed() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let world = World::new(db.clone(), sde.clone(), 1);
    let dir = std::env::temp_dir().join(format!("infinity-lua-budget-{}", std::process::id()));
    std::fs::create_dir(&dir).unwrap();
    std::fs::write(dir.join("budget.lua"), r#"
        register.rpc('budget', 'loop', function() while true do end end)
        register.rpc('budget', 'ok', function() return eve.int(42) end)
        register.command{name='budgetloop', help='', gm=false, fn=function() while true do end end}
        register.command{name='budgetok', help='', gm=false, fn=function() return 'ok' end}
    "#).unwrap();
    let mut config = Config::default();
    config.lua.scripts_dir = dir.to_string_lossy().into_owned();
    config.lua.call_budget_ms = 60_000;
    config.lua.call_budget_instructions = 1000;
    config.lua.instruction_hook_interval = 100;
    let chat = crate::chat::Hub::new(&config.chat.domain, db.clone(), world.clone());
    start(&config, &db, &sde, &world, &chat).await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let peer = tokio::net::TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
    let (stream, address) = listener.accept().await.unwrap();
    let conn = crate::conn::Conn::new(stream, address);
    let login = crate::handshake::Login { user_id: 1, user_name: "test".into(), role: GM_ROLES, client_id: 1, session_id: 1, language_id: "EN".into() };
    let mut client = Client::new(conn, Arc::new(config), db, sde, chat, world, login);
    let kwargs = marshal::Value::Dict(vec![]);
    let call = |method| crate::services::Call { service: "budget", method, args: &[], kwargs: &kwargs, bound: false, bind_params: None };
    assert!(matches!(dispatch(&mut client, &call("loop")).await, Some(Err(_))));
    assert!(matches!(dispatch(&mut client, &call("ok")).await, Some(Ok(marshal::Value::Int(42)))));
    assert!(matches!(command(&mut client, "budgetloop", &[]).await, Some(Err(_))));
    assert!(matches!(command(&mut client, "budgetok", &[]).await, Some(Ok(value)) if value == "ok"));
    drop(peer);
    std::fs::remove_dir_all(dir).unwrap();
}
