mod retention;

use super::*;
use crate::{lua::host::Resources, sde::Sde, space::World};
use std::future::Future;

async fn wait_for_ticks(ticks: &mlua::Function, expected: i64) {
    for _ in 0..10_000 {
        if ticks.call::<i64>(()).unwrap() == expected { return; }
        tokio::task::yield_now().await;
    }
    panic!("runner did not reach tick count {expected}; actual {}, paused time {:?}", ticks.call::<i64>(()).unwrap(), tokio::time::Instant::now());
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn reload_preserves_old_generation_on_failure_and_in_flight_call() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let world = World::new(db.clone(), sde.clone(), 1);
    let hub = crate::chat::Hub::new("test", db.clone(), world.clone());
    let pilots = crate::lua::host::pilots::Pilots::new(&db, hub).await.unwrap();
    let resources = Resources { db, sde, world, pilots, store_value_limit_bytes: 65536 };
    let dir = std::env::temp_dir().join(format!("lua-reload-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    std::fs::create_dir(&dir).unwrap();
    let script = dir.join("test.lua");
    let extra = dir.join("extra");
    std::fs::create_dir(&extra).unwrap();
    std::fs::write(extra.join("other.lua"), "register.rpc('other','m',function() return 7 end)").unwrap();
    std::fs::write(&script, "local n=0; local ticks=0; register.tick{name='pulse', interval_ms=100, fn=function() ticks=ticks+1 end}; register.rpc('svc','ticks',function() return ticks end); register.rpc('svc','m',function() n=n+1; return n end)").unwrap();
    let config = LuaConfig { scripts_dir: dir.to_string_lossy().into_owned(), mod_dirs: vec![extra.to_string_lossy().into_owned()], ..LuaConfig::default() };
    let host = Host::new(config, resources).await.unwrap();
    let mut generations = host.subscribe();
    let old = host.snapshot();
    assert_eq!(old.claims.scripts, ["test", "other"]);
    assert_eq!(old.claims.script_roots["test"], dir.canonicalize().unwrap());
    assert_eq!(old.claims.script_roots["other"], extra.canonicalize().unwrap());
    tokio::time::pause();
    let (_event_tx, event_rx) = tokio::sync::mpsc::unbounded_channel();
    let (ready, started) = tokio::sync::oneshot::channel();
    let mut run = Box::pin(crate::lua::runner::run(host.config.clone(), host.subscribe(), event_rx));
    let runner = tokio::spawn(async move {
        let mut ready = Some(ready);
        std::future::poll_fn(|cx| {
            let result = run.as_mut().poll(cx);
            if let Some(ready) = ready.take() { let _ = ready.send(()); }
            result
        }).await
    });
    started.await.unwrap();
    // Let Tokio advance to the rounded timer-wheel deadline: pausing after I/O
    // can leave the clock between millisecond ticks.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let ticks = old.claims.rpc[&("svc".into(), "ticks".into())].function.clone();
    wait_for_ticks(&ticks, 1).await;
    assert_eq!(ticks.call::<i64>(()).unwrap(), 1);
    assert_eq!(old.claims.rpc[&("other".into(), "m".into())].function.call::<i64>(()).unwrap(), 7);
    let duplicate = extra.join("test.lua");
    std::fs::write(&duplicate, "invalid syntax !").unwrap();
    let error = format!("{:#}", host.reload().await.unwrap_err());
    assert!(error.contains(&script.display().to_string()) && error.contains(&duplicate.display().to_string()));
    assert!(Arc::ptr_eq(&old, &host.snapshot()));
    assert!(!generations.has_changed().unwrap());
    assert_eq!(host.snapshot().claims.ticks.len(), 1);
    tokio::time::advance(std::time::Duration::from_millis(100)).await;
    wait_for_ticks(&ticks, 2).await;
    assert_eq!(ticks.call::<i64>(()).unwrap(), 2);
    std::fs::remove_file(duplicate).unwrap();
    let handler = old.claims.rpc.get(&("svc".into(), "m".into())).unwrap().function.clone();
    assert_eq!(handler.call::<i64>(()).unwrap(), 1);
    std::fs::write(&script, "invalid ! syntax").unwrap();
    assert!(host.reload().await.is_err());
    assert!(Arc::ptr_eq(&old, &host.snapshot()));
    assert_eq!(handler.call::<i64>(()).unwrap(), 2);
    std::fs::write(&script, "register.rpc('svc','m',function() end); register.rpc('svc','m',function() end)").unwrap();
    assert!(host.reload().await.is_err());
    assert!(Arc::ptr_eq(&old, &host.snapshot()));
    std::fs::write(&script, "register.content{key='unknown',schema_version=1,content_version='1',datasets={},state_schema='sovereignty_policy_v1',operations={},progress_rules={}}").unwrap();
    assert!(host.reload().await.is_err());
    assert!(Arc::ptr_eq(&old, &host.snapshot()));
    assert!(!generations.has_changed().unwrap());
    let (sender, receiver) = tokio::sync::oneshot::channel::<()>();
    let receiver = Arc::new(tokio::sync::Mutex::new(Some(receiver)));
    let (entered, entry) = tokio::sync::oneshot::channel();
    let entered = std::sync::Mutex::new(Some(entered));
    let wait = old.lua.create_async_function(move |_, ()| {
        let receiver = receiver.clone();
        if let Some(entered) = entered.lock().unwrap().take() { let _ = entered.send(()); }
        async move { receiver.lock().await.take().unwrap().await.map_err(mlua::Error::external) }
    }).unwrap();
    old.lua.globals().set("gate", wait).unwrap();
    old.lua.globals().set("old_handler", handler).unwrap();
    let pending = old.lua.load("gate(); return old_handler()").into_function().unwrap();
    let pending = old.lua.create_thread(pending).unwrap().into_async::<i64>(()).unwrap();
    tokio::pin!(pending);
    tokio::select! { biased; _ = &mut pending => panic!("completed before release"), result = entry => result.unwrap() }
    std::fs::write(&script, "local n=100; register.rpc('svc','m',function() n=n+1; return n end)").unwrap();
    host.reload().await.unwrap();
    let new = host.snapshot();
    assert!(generations.has_changed().unwrap());
    runner.abort();
    assert!(Arc::ptr_eq(&new, &generations.borrow_and_update()));
    assert!(!Arc::ptr_eq(&old, &new));
    assert_eq!(new.claims.rpc.get(&("svc".into(), "m".into())).unwrap().function.call::<i64>(()).unwrap(), 101);
    sender.send(()).unwrap();
    assert_eq!(pending.await.unwrap(), 3);
    std::fs::remove_dir_all(dir).unwrap();
}
