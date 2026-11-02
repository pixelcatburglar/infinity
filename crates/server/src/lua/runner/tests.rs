use super::*;
use crate::lua::registry::{Claims, Handler, Tick};
use std::{future::Future, sync::{Mutex, atomic::AtomicU64}};

fn registry(source: &str) -> Arc<Registry> {
    let lua = mlua::Lua::new();
    lua.globals().set("seen", lua.create_table().unwrap()).unwrap();
    let function = lua.load(source).eval().unwrap();
    let mut claims = Claims::default();
    claims.ticks.push(Tick { handler: Handler { script: "test".into(), function }, name: "pulse".into(), interval: Duration::from_millis(100), skipped: AtomicU64::new(0) });
    Arc::new(Registry { lua, claims, overrides: Mutex::new(Default::default()) })
}

fn config() -> LuaConfig {
    LuaConfig { call_budget_ms: 60_000, call_budget_instructions: 10_000, instruction_hook_interval: 100, ..Default::default() }
}

async fn until(mut predicate: impl FnMut() -> bool) {
    for _ in 0..10_000 {
        if predicate() { return; }
        tokio::task::yield_now().await;
    }
    panic!("runner did not reach the expected state");
}

async fn start(config: LuaConfig, receiver: watch::Receiver<Arc<Registry>>, rx: mpsc::UnboundedReceiver<GameEvent>) -> tokio::task::JoinHandle<()> {
    let (ready, started) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        let mut run = std::pin::pin!(run(config, receiver, rx));
        let mut ready = Some(ready);
        std::future::poll_fn(|cx| {
            let result = run.as_mut().poll(cx);
            if let Some(ready) = ready.take() { let _ = ready.send(()); }
            result
        }).await
    });
    started.await.unwrap();
    task
}

fn observed(registry: &Registry) -> i64 {
    registry.lua.globals().get::<mlua::Table>("seen").unwrap().len().unwrap()
}

fn adoption_event(registry: Arc<Registry>) -> Arc<Registry> {
    let handler = registry.lua.load("return function() adopted = true end").eval().unwrap();
    let mut registry = Arc::try_unwrap(registry).ok().unwrap();
    registry.claims.events.insert("BallparkBound".into(), vec![Handler { script: "test".into(), function: handler }]);
    Arc::new(registry)
}

async fn adopted(events: &mpsc::UnboundedSender<GameEvent>, registry: &Registry) {
    events.send(GameEvent::BallparkBound { char_id: 1, system_id: 1 }).unwrap();
    until(|| registry.lua.globals().get::<Option<bool>>("adopted").unwrap() == Some(true)).await;
}

#[tokio::test(start_paused = true)]
async fn tick_starts_after_interval_and_fires_exact_cadence() {
    let registry = registry("return function(now) assert(type(now)=='number'); table.insert(seen, now) end");
    let (_generation, receiver) = watch::channel(registry.clone());
    let (_events, rx) = mpsc::unbounded_channel();
    let task = start(config(), receiver, rx).await;
    let seen: mlua::Table = registry.lua.globals().get("seen").unwrap();
    assert_eq!(seen.len().unwrap(), 0);
    for count in 1..=10 {
        tokio::time::advance(Duration::from_millis(100)).await;
        until(|| observed(&registry) == count).await;
        assert_eq!(seen.len().unwrap(), count);
    }
    assert_eq!(registry.claims.ticks[0].skipped.load(Ordering::Relaxed), 0);
    task.abort();
}

#[tokio::test(start_paused = true)]
async fn slow_tick_skips_without_backlog_and_serializes_events() {
    let registry = registry("return function(now) table.insert(seen, 'tick'); gate() end");
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let gate_fn = gate.clone();
    registry.lua.globals().set("gate", registry.lua.create_async_function(move |_, ()| {
        let gate = gate_fn.clone();
        async move { gate.acquire().await.unwrap().forget(); Ok(()) }
    }).unwrap()).unwrap();
    let handler = registry.lua.load("return function(ev) table.insert(seen, ev.char_id) end").eval().unwrap();
    // This registry has not yet been shared with the runner.
    let mut registry = Arc::try_unwrap(registry).ok().unwrap();
    registry.claims.events.insert("BallparkBound".into(), vec![Handler { script: "test".into(), function: handler }]);
    let registry = Arc::new(registry);
    let (_generation, receiver) = watch::channel(registry.clone());
    let (events, rx) = mpsc::unbounded_channel();
    let task = start(config(), receiver, rx).await;
    tokio::time::advance(Duration::from_millis(100)).await;
    until(|| observed(&registry) == 1).await;
    for char_id in [1, 2] { events.send(GameEvent::BallparkBound { char_id, system_id: 1 }).unwrap(); }
    tokio::time::advance(Duration::from_millis(350)).await;
    let seen: mlua::Table = registry.lua.globals().get("seen").unwrap();
    assert_eq!(seen.len().unwrap(), 1);
    gate.add_permits(1);
    until(|| observed(&registry) == 3 && registry.claims.ticks[0].skipped.load(Ordering::Relaxed) == 3).await;
    assert_eq!(registry.claims.ticks[0].skipped.load(Ordering::Relaxed), 3);
    assert_eq!(seen.len().unwrap(), 3);
    assert_eq!(seen.get::<i64>(2).unwrap(), 1);
    assert_eq!(seen.get::<i64>(3).unwrap(), 2);
    tokio::time::advance(Duration::from_millis(50)).await;
    until(|| observed(&registry) == 4).await;
    assert_eq!(seen.len().unwrap(), 4);
    task.abort();
}

#[tokio::test(start_paused = true)]
async fn simultaneous_ticks_and_queued_event_progress_after_slow_handler() {
    let registry = registry("return function() table.insert(seen, 'slow'); gate() end");
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let gate_fn = gate.clone();
    registry.lua.globals().set("gate", registry.lua.create_async_function(move |_, ()| {
        let gate = gate_fn.clone();
        async move { gate.acquire().await.unwrap().forget(); Ok(()) }
    }).unwrap()).unwrap();
    let fast = registry.lua.load("return function() table.insert(seen, 'fast') end").eval().unwrap();
    let event = registry.lua.load("return function() table.insert(seen, 'event') end").eval().unwrap();
    let mut registry = Arc::try_unwrap(registry).ok().unwrap();
    registry.claims.ticks.push(Tick { handler: Handler { script: "test".into(), function: fast }, name: "fast".into(), interval: Duration::from_millis(100), skipped: AtomicU64::new(0) });
    registry.claims.events.insert("BallparkBound".into(), vec![Handler { script: "test".into(), function: event }]);
    let registry = Arc::new(registry);
    let (_generation, receiver) = watch::channel(registry.clone());
    let (events, rx) = mpsc::unbounded_channel();
    let task = start(config(), receiver, rx).await;
    tokio::time::advance(Duration::from_millis(100)).await;
    until(|| observed(&registry) == 1).await;
    events.send(GameEvent::BallparkBound { char_id: 1, system_id: 1 }).unwrap();
    tokio::time::advance(Duration::from_millis(350)).await;
    gate.add_permits(1);
    until(|| observed(&registry) == 3 && registry.claims.ticks[0].skipped.load(Ordering::Relaxed) == 3 && registry.claims.ticks[1].skipped.load(Ordering::Relaxed) == 3).await;
    let seen: mlua::Table = registry.lua.globals().get("seen").unwrap();
    assert_eq!(seen.len().unwrap(), 3);
    let second: String = seen.get(2).unwrap();
    let third: String = seen.get(3).unwrap();
    assert!((second == "fast" && third == "event") || (second == "event" && third == "fast"));
    assert_eq!(registry.claims.ticks[0].skipped.load(Ordering::Relaxed), 3);
    assert_eq!(registry.claims.ticks[1].skipped.load(Ordering::Relaxed), 3);
    gate.add_permits(1);
    tokio::time::advance(Duration::from_millis(50)).await;
    until(|| observed(&registry) == 5).await;
    assert_eq!(seen.len().unwrap(), 5);
    task.abort();
}

#[tokio::test(start_paused = true)]
async fn replacement_resets_schedule_and_retires_old_ticks() {
    let old = registry("return function() table.insert(seen, 1) end");
    let new = adoption_event(registry("return function() table.insert(seen, 1) end"));
    let (generation, receiver) = watch::channel(old.clone());
    let (events, rx) = mpsc::unbounded_channel();
    let task = start(config(), receiver, rx).await;
    tokio::time::advance(Duration::from_millis(100)).await;
    until(|| observed(&old) == 1).await;
    generation.send_replace(new.clone());
    adopted(&events, &new).await;
    tokio::time::advance(Duration::from_millis(99)).await;
    new.lua.globals().set("adopted", false).unwrap();
    adopted(&events, &new).await;
    assert_eq!(new.lua.globals().get::<mlua::Table>("seen").unwrap().len().unwrap(), 0);
    tokio::time::advance(Duration::from_millis(1)).await;
    until(|| observed(&new) == 1).await;
    assert_eq!(new.lua.globals().get::<mlua::Table>("seen").unwrap().len().unwrap(), 1);
    assert_eq!(old.lua.globals().get::<mlua::Table>("seen").unwrap().len().unwrap(), 1);
    task.abort();
}

#[tokio::test(start_paused = true)]
async fn queued_events_use_replacement_generation_in_fifo_order() {
    let old = registry("return function() table.insert(seen, 'tick') end");
    let old_handler = old.lua.load("return function(ev) table.insert(seen, ev.char_id) end").eval().unwrap();
    let mut old = Arc::try_unwrap(old).ok().unwrap();
    old.claims.events.insert("BallparkBound".into(), vec![Handler { script: "test".into(), function: old_handler }]);
    let old = Arc::new(old);

    let new = registry("return function() table.insert(seen, 'tick') end");
    let (done, completed) = tokio::sync::oneshot::channel();
    let done = Mutex::new(Some(done));
    new.lua.globals().set("done", new.lua.create_function(move |_, ()| {
        if let Some(done) = done.lock().unwrap().take() { let _ = done.send(()); }
        Ok(())
    }).unwrap()).unwrap();
    let new_handler = new.lua.load("return function(ev) table.insert(seen, ev.char_id); if ev.char_id == 2 then done() end end").eval().unwrap();
    let mut new = Arc::try_unwrap(new).ok().unwrap();
    new.claims.events.insert("BallparkBound".into(), vec![Handler { script: "test".into(), function: new_handler }]);
    let new = Arc::new(new);

    let (generation, receiver) = watch::channel(old.clone());
    let (events, rx) = mpsc::unbounded_channel();
    let task = start(config(), receiver, rx).await;
    generation.send_replace(new.clone());
    for char_id in [1, 2] { events.send(GameEvent::BallparkBound { char_id, system_id: 1 }).unwrap(); }
    completed.await.unwrap();
    let seen: mlua::Table = new.lua.globals().get("seen").unwrap();
    assert_eq!(seen.len().unwrap(), 2);
    assert_eq!(seen.get::<i64>(1).unwrap(), 1);
    assert_eq!(seen.get::<i64>(2).unwrap(), 2);
    assert_eq!(observed(&old), 0);
    task.abort();
}

#[tokio::test(start_paused = true)]
async fn selected_event_adopts_pending_replacement_before_dispatch() {
    let old = registry("return function() table.insert(seen, 'tick') end");
    let old_handler = old.lua.load("return function(ev) table.insert(seen, ev.char_id) end").eval().unwrap();
    let mut old = Arc::try_unwrap(old).ok().unwrap();
    old.claims.events.insert("BallparkBound".into(), vec![Handler { script: "test".into(), function: old_handler }]);
    let old = Arc::new(old);
    let new = registry("return function() table.insert(seen, 'tick') end");
    let new_handler = new.lua.load("return function(ev) table.insert(seen, ev.char_id) end").eval().unwrap();
    let mut new = Arc::try_unwrap(new).ok().unwrap();
    new.claims.events.insert("BallparkBound".into(), vec![Handler { script: "test".into(), function: new_handler }]);
    let new = Arc::new(new);
    let (generation, mut receiver) = watch::channel(old.clone());
    let mut current = receiver.borrow_and_update().clone();
    let mut schedule = schedules(&current);
    let (events, mut rx) = mpsc::unbounded_channel();
    generation.send_replace(new.clone());
    for char_id in [1, 2] { events.send(GameEvent::BallparkBound { char_id, system_id: 1 }).unwrap(); }
    for _ in 0..2 {
        let event = rx.recv().await.unwrap();
        assert!(dispatch_selected(Selected::Event(event), &config(), &mut receiver, &mut current, &mut schedule).await);
    }
    let seen: mlua::Table = new.lua.globals().get("seen").unwrap();
    assert_eq!(seen.len().unwrap(), 2);
    assert_eq!(seen.get::<i64>(1).unwrap(), 1);
    assert_eq!(seen.get::<i64>(2).unwrap(), 2);
    assert_eq!(observed(&old), 0);
}

#[tokio::test(start_paused = true)]
async fn selected_tick_discards_pending_replacement_old_schedule() {
    let old = registry("return function() table.insert(seen, 'old') end");
    let new = registry("return function() table.insert(seen, 'new') end");
    let (generation, mut receiver) = watch::channel(old.clone());
    let mut current = receiver.borrow_and_update().clone();
    let mut schedule = schedules(&current);
    tokio::time::advance(Duration::from_millis(100)).await;
    std::future::poll_fn(|cx| schedule[0].as_mut().unwrap().timer.poll_tick(cx)).await;
    generation.send_replace(new.clone());
    assert!(dispatch_selected(Selected::Tick(0), &config(), &mut receiver, &mut current, &mut schedule).await);
    assert_eq!(observed(&old), 0);
    assert_eq!(observed(&new), 0);
    assert_eq!(schedule[0].as_ref().unwrap().next, Instant::now() + Duration::from_millis(100));
    tokio::time::advance(Duration::from_millis(100)).await;
    std::future::poll_fn(|cx| schedule[0].as_mut().unwrap().timer.poll_tick(cx)).await;
    assert!(dispatch_selected(Selected::Tick(0), &config(), &mut receiver, &mut current, &mut schedule).await);
    assert_eq!(observed(&old), 0);
    assert_eq!(observed(&new), 1);
}

#[tokio::test(start_paused = true)]
async fn tick_instruction_budget_does_not_stop_runner() {
    let registry = registry("return function() table.insert(seen, 1); while true do end end");
    let (_generation, receiver) = watch::channel(registry.clone());
    let (_events, rx) = mpsc::unbounded_channel();
    let task = start(config(), receiver, rx).await;
    for count in 1..=2 {
        tokio::time::advance(Duration::from_millis(100)).await;
        until(|| observed(&registry) == count).await;
        assert_eq!(registry.lua.globals().get::<mlua::Table>("seen").unwrap().len().unwrap(), count);
    }
    task.abort();
}

#[tokio::test(start_paused = true)]
async fn tick_timeout_cleans_hook_and_allows_later_invocations() {
    let registry = registry("return function() table.insert(seen, 1); gate() end");
    registry.lua.globals().set("gate", registry.lua.create_async_function(|_, ()| async {
        std::future::pending::<mlua::Result<()>>().await
    }).unwrap()).unwrap();
    let (_generation, receiver) = watch::channel(registry.clone());
    let (_events, rx) = mpsc::unbounded_channel();
    let mut config = config();
    config.call_budget_ms = 250;
    let task = start(config, receiver, rx).await;
    tokio::time::advance(Duration::from_millis(100)).await;
    until(|| observed(&registry) == 1).await;
    tokio::time::advance(Duration::from_millis(250)).await;
    until(|| registry.claims.ticks[0].skipped.load(Ordering::Relaxed) == 2).await;
    assert_eq!(registry.claims.ticks[0].skipped.load(Ordering::Relaxed), 2);
    tokio::time::advance(Duration::from_millis(50)).await;
    until(|| observed(&registry) == 2).await;
    assert_eq!(registry.lua.globals().get::<mlua::Table>("seen").unwrap().len().unwrap(), 2);
    task.abort();
}

#[tokio::test(start_paused = true)]
async fn replacement_waits_for_running_tick_then_drops_old_schedule() {
    let old = registry("return function() table.insert(seen, 1); gate() end");
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let gate_fn = gate.clone();
    old.lua.globals().set("gate", old.lua.create_async_function(move |_, ()| {
        let gate = gate_fn.clone();
        async move { gate.acquire().await.unwrap().forget(); Ok(()) }
    }).unwrap()).unwrap();
    let new = adoption_event(registry("return function() table.insert(seen, 1) end"));
    let (generation, receiver) = watch::channel(old.clone());
    let (events, rx) = mpsc::unbounded_channel();
    let task = start(config(), receiver, rx).await;
    tokio::time::advance(Duration::from_millis(100)).await;
    until(|| observed(&old) == 1).await;
    generation.send_replace(new.clone());
    tokio::time::advance(Duration::from_millis(250)).await;
    assert_eq!(new.lua.globals().get::<mlua::Table>("seen").unwrap().len().unwrap(), 0);
    gate.add_permits(1);
    adopted(&events, &new).await;
    tokio::time::advance(Duration::from_millis(100)).await;
    until(|| observed(&new) == 1).await;
    assert_eq!(old.lua.globals().get::<mlua::Table>("seen").unwrap().len().unwrap(), 1);
    assert_eq!(new.lua.globals().get::<mlua::Table>("seen").unwrap().len().unwrap(), 1);
    task.abort();
}
