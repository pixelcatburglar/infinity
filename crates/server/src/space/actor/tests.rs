use super::*;

#[tokio::test]
async fn interval_deadlines_align_to_wall_boundaries() {
    for (wall_ms, offset_ms) in [(12_345, 55), (12_945, 55), (12_300, 100), (12_399, 1)] {
        let base = Instant::now();
        let mut interval = aligned_interval(wall_ms, base);
        assert_eq!(interval.tick().await, base + Duration::from_millis(offset_ms));
    }
}

#[tokio::test]
async fn waits_follow_timer_execution_and_actor_lifetime() {
    crate::tests::init_config();
    assert!(!after(None, 100).await);
    let (tx, mut rx) = mpsc::unbounded_channel();
    let handle = SystemHandle { tx, gate: Arc::new(tokio::sync::Mutex::new(())), state: Arc::new(lifecycle::ActorState::default()) };
    let waiter = tokio::spawn(after(Some(handle.clone()), 100));
    let mut system = System::default();
    rx.recv().await.unwrap().run(&mut system);
    system.tick(99);
    assert!(!waiter.is_finished());
    system.tick(100);
    assert!(waiter.await.unwrap());

    let waiter = tokio::spawn(after(Some(handle.clone()), 100));
    rx.recv().await.unwrap().run(&mut system);
    drop(system);
    assert!(!waiter.await.unwrap());
    drop(rx);
    assert!(!after(Some(handle), 100).await);
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn idle_stop_preserves_work_and_finishes_cleanup() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = crate::tests::shared_sde(&db).await;
    let id = sde.map.public_systems().next().unwrap().solar_system_id;
    let mut system = System::new(id, Arc::default(), 0);
    system.projection_sde = Some(sde);
    let systems = Arc::new(Mutex::new(HashMap::new()));
    let handle = start_mode(system, id, Arc::downgrade(&systems), db, true, None);
    systems.lock().unwrap().insert(id, handle.clone());
    let extra = handle.clone();
    assert_eq!(handle.request_idle_stop(None).await.unwrap(), lifecycle::StopDecision::Busy("mailbox_or_sender_busy"));
    let (done, received) = tokio::sync::oneshot::channel();
    assert!(extra.send(Job::new(move |_| { let _ = done.send(()); })).is_ok());
    received.await.unwrap();
    drop(extra);
    assert_eq!(handle.request_idle_stop(None).await.unwrap(), lifecycle::StopDecision::Stopping);
    handle.wait_exited().await.unwrap();
    assert!(handle.is_closed() && handle.is_stopping());
    assert!(!systems.lock().unwrap().contains_key(&id));
}

#[tokio::test]
async fn disabled_stop_has_no_control() {
    let (tx, _rx) = mpsc::unbounded_channel();
    let handle = SystemHandle { tx, gate: Arc::new(tokio::sync::Mutex::new(())), state: Arc::new(lifecycle::ActorState::default()) };
    assert!(handle.request_idle_stop(None).await.is_err());
    assert!(handle.wait_exited().await.is_err());
    assert!(!handle.is_stopping());
}

async fn stop_fixture() -> (System, SystemHandle, Arc<Mutex<HashMap<i64, SystemHandle>>>, mpsc::UnboundedReceiver<Job>) {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = crate::tests::shared_sde(&db).await;
    let id = sde.map.public_systems().next().unwrap().solar_system_id;
    let mut system = System::new(id, Arc::default(), 0);
    system.projection_sde = Some(sde);
    let (tx, rx) = mpsc::unbounded_channel();
    let (stop, _stop_rx) = mpsc::unbounded_channel();
    let (_exit, exited) = tokio::sync::watch::channel(None);
    let state = lifecycle::ActorState::share(Some(lifecycle::Control { stop, exited, stopping: std::sync::atomic::AtomicBool::new(false) }));
    let handle = SystemHandle { tx, gate: Arc::new(tokio::sync::Mutex::new(())), state };
    let systems = Arc::new(Mutex::new(HashMap::from([(id, handle.clone())])));
    assert_eq!(system.unload_blocked(), None);
    assert!(system.combat_facts.is_none() && rx.is_empty() && handle.tx.strong_count() == 2 && !handle.is_stopping());
    (system, handle, systems, rx)
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn cancel_during_worker_probe_keeps_actor_live() {
    let (mut system, handle, systems, mut rx) = stop_fixture().await;
    let id = system.id;
    let registry = Arc::downgrade(&systems);
    let gate = Arc::downgrade(&handle.gate);
    let (probes, mut effects, mut deferred) = crate::space::npc::worker::tests::controlled_probes();
    let (reply, receipt) = tokio::sync::oneshot::channel();
    let decision = lifecycle::try_idle_stop(reply, &probes, &mut system, &mut rx, &registry, &gate, &handle.state, id);
    let (stopped, ()) = tokio::join!(decision, async {
        let effect = effects.recv().await.unwrap();
        let pending = deferred.recv().await.unwrap();
        effect.send(true).unwrap(); drop(receipt); pending.send(true).unwrap();
    });
    assert!(!stopped && !rx.is_closed() && !handle.is_stopping());
    assert!(systems.lock().unwrap().get(&id).unwrap().tx.same_channel(&handle.tx));
    let (result, ()) = tokio::join!(handle.with(|s| s.id), async { rx.recv().await.unwrap().run(&mut system); });
    assert_eq!(result, Some(id));
    let (reply, receipt) = tokio::sync::oneshot::channel();
    let decision = lifecycle::try_idle_stop(reply, &probes, &mut system, &mut rx, &registry, &gate, &handle.state, id);
    let (stopped, ()) = tokio::join!(decision, async {
        effects.recv().await.unwrap().send(true).unwrap();
        deferred.recv().await.unwrap().send(true).unwrap();
    });
    assert!(stopped && rx.is_closed() && handle.is_stopping());
    assert_eq!(receipt.await.unwrap().unwrap(), lifecycle::StopDecision::Stopping);
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn queued_arrival_prevents_stop_and_executes() {
    let (mut system, handle, systems, mut rx) = stop_fixture().await;
    let id = system.id;
    let registry = Arc::downgrade(&systems);
    let gate = Arc::downgrade(&handle.gate);
    let (probes, mut effects, mut deferred) = crate::space::npc::worker::tests::controlled_probes();
    let pilots = system.registry.clone();
    assert_eq!(pilots.unload_blocked(id), None);
    let (reply, receipt) = tokio::sync::oneshot::channel();
    let decision = lifecycle::try_idle_stop(reply, &probes, &mut system, &mut rx, &registry, &gate, &handle.state, id);
    let (stopped, ()) = tokio::join!(decision, async {
        let effect = effects.recv().await.unwrap();
        let pending = deferred.recv().await.unwrap();
        pilots.queues.lock().unwrap().entry(id).or_default().waiting.push(1);
        effect.send(true).unwrap();
        pending.send(true).unwrap();
    });
    assert!(!stopped && !rx.is_closed() && !handle.is_stopping());
    assert_eq!(receipt.await.unwrap().unwrap(), lifecycle::StopDecision::Busy("jump_queue"));
    pilots.queues.lock().unwrap().remove(&id);
    assert_eq!(pilots.unload_blocked(id), None);
    let (ack, received) = tokio::sync::oneshot::channel();
    assert!(handle.send(Job::new(move |s| {
        let arrival = s.register_warp_arrival(Box::new(move |s| s.id = id + 1));
        let _ = ack.send(arrival);
    })).is_ok());
    assert_eq!(handle.tx.strong_count(), 2);
    let (reply, receipt) = tokio::sync::oneshot::channel();
    let decision = lifecycle::try_idle_stop(reply, &probes, &mut system, &mut rx, &registry, &gate, &handle.state, id);
    let (stopped, ()) = tokio::join!(decision, async {
        effects.recv().await.unwrap().send(true).unwrap();
        deferred.recv().await.unwrap().send(true).unwrap();
    });
    assert!(!stopped && !rx.is_closed() && !handle.is_stopping());
    assert_eq!(receipt.await.unwrap().unwrap(), lifecycle::StopDecision::Busy("mailbox_or_sender_busy"));
    rx.recv().await.unwrap().run(&mut system);
    let arrival = received.await.unwrap();
    assert_eq!(system.unload_blocked(), Some("warp"));
    system.complete_warp_arrival(arrival);
    assert_eq!(system.id, id + 1);
}

#[test]
fn summary_cost_excluded_from_tick() {
    const CHILD: &str = "INFINITY_ACTOR_SUMMARY_TEST_CHILD";
    const NAME: &str = "space::actor::tests::summary_cost_excluded_from_tick";
    // Other actor tests share the process summary window and slow-report limit.
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", NAME, "--nocapture"]).env(CHILD, "1").output().unwrap();
        assert!(output.status.success(), "{}\n{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
        return;
    }
    #[derive(Clone, Copy, PartialEq, Debug)]
    enum Field { U64(u64), I64(i64), Bool(bool), Other }
    struct Visit<'a>(&'a mut Vec<(&'static str, Field)>);
    impl tracing::field::Visit for Visit<'_> {
        fn record_debug(&mut self, f: &tracing::field::Field, _: &dyn std::fmt::Debug) { self.0.push((f.name(), Field::Other)); }
        fn record_u64(&mut self, f: &tracing::field::Field, v: u64) { self.0.push((f.name(), Field::U64(v))); }
        fn record_i64(&mut self, f: &tracing::field::Field, v: i64) { self.0.push((f.name(), Field::I64(v))); }
        fn record_bool(&mut self, f: &tracing::field::Field, v: bool) { self.0.push((f.name(), Field::Bool(v))); }
    }
    struct Capture { tx: mpsc::UnboundedSender<(tracing::Level, Vec<(&'static str, Field)>)>,
        ready: Arc<std::sync::Barrier>, done: Arc<std::sync::Barrier>, fired: std::sync::atomic::AtomicBool }
    impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for Capture {
        fn on_event(&self, event: &tracing::Event<'_>, _: tracing_subscriber::layer::Context<'_, S>) {
            let mut fields = Vec::new(); event.record(&mut Visit(&mut fields));
            let level = *event.metadata().level();
            let expected = match level {
                tracing::Level::INFO => fields.iter().any(|(name, _)| *name == "window_ns"),
                tracing::Level::TRACE => fields.iter().any(|(name, _)| *name == "duration_us"),
                tracing::Level::WARN => fields.iter().any(|(name, _)| *name == "duration_ns"),
                _ => false,
            };
            if event.metadata().target() != "infinity_server::space::actor" || !expected { return; }
            if level == tracing::Level::INFO
                && !self.fired.swap(true, std::sync::atomic::Ordering::SeqCst) { self.ready.wait(); self.done.wait(); }
            let _ = self.tx.send((level, fields));
        }
    }
    use tracing_subscriber::layer::SubscriberExt as _;
    let field = |events: &[(&'static str, Field)], name| events.iter().find(|(n, _)| *n == name).unwrap().1;
    let (tx, mut rx) = mpsc::unbounded_channel();
    let (ready, done) = (Arc::new(std::sync::Barrier::new(2)), Arc::new(std::sync::Barrier::new(2)));
    let _dispatch = tracing::subscriber::set_default(tracing_subscriber::Registry::default()
        .with(Capture { tx, ready: ready.clone(), done: done.clone(), fired: false.into() }));
    let runtime = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
    let control = std::thread::spawn({
        let (ready, done) = (ready.clone(), done.clone());
        let handle = runtime.handle().clone();
        move || { ready.wait(); handle.block_on(async { tokio::time::advance(Duration::from_millis(100)).await }); done.wait(); }
    });
    let db = crate::db::Db::unconnected_publication_fixture(([127, 0, 0, 1], 1).into()); crate::tests::init_config();
    runtime.block_on(async {
        tokio::time::pause(); let mut system = System::new(1, Arc::default(), now_ms());
        system.attribution.started = Some(Instant::now() - SUMMARY_WINDOW); system.attribution.window.record(Duration::from_millis(2), Duration::ZERO);
        let systems = Arc::new(Mutex::new(HashMap::new())); let handle = start_mode(system, 1, Arc::downgrade(&systems), db.clone(), true, None);
        systems.lock().unwrap().insert(1, handle.clone());
        let mut exited = handle.state.control.as_ref().unwrap().exited.clone();
        let (info, trace, warn) = (rx.recv().await.unwrap(), rx.recv().await.unwrap(), rx.recv().await.unwrap());
        assert_eq!((field(&trace.1, "duration_us"), field(&warn.1, "duration_ns")), (Field::U64(0), Field::U64(0)));
        assert!(matches!((field(&trace.1, "lateness_us"), field(&warn.1, "lateness_ns")),
            (Field::U64(us), Field::U64(ns)) if us * 1_000 == ns && ns >= 100_000_000));
        assert_eq!((field(&warn.1, "own_lateness_ns"), field(&warn.1, "runtime_lateness_ns")),
            (Field::U64(0), field(&warn.1, "lateness_ns")));
        for name in ["system", "duration_ns", "lateness_ns", "own_lateness_ns", "runtime_lateness_ns", "steps",
            "balls", "observers", "pilots", "mailbox_depth", "top_phases", "mailbox_count", "mailbox_total_ns",
            "mailbox_top", "timer_count", "timer_total_ns", "timer_top", "delivery_events", "delivery_copies", "delivery_ns"] {
            assert!(warn.1.iter().any(|(n, _)| *n == name), "warn missing {name}");
        }
        for name in ["system", "window_ns", "partial", "ticks", "duration_total_ns", "duration_p50_ns", "duration_p99_ns",
            "duration_max_ns", "lateness_p50_ns", "lateness_p99_ns", "lateness_max_ns", "top_phases", "top_mailbox",
            "top_timers", "top_systems", "mailbox_count", "mailbox_total_ns", "timer_count", "timer_total_ns",
            "delivery_events", "delivery_copies", "delivery_ns", "suppressed"] {
            assert!(info.1.iter().any(|(n, _)| *n == name), "info missing {name}");
        }
        assert!(!warn.1.iter().chain(info.1.iter()).any(|(n, _)| *n == "report" || *n == "summary"));
        assert_eq!((field(&warn.1, "system"), field(&info.1, "system"), field(&info.1, "partial"), field(&info.1, "ticks")),
            (Field::I64(1), Field::I64(1), Field::Bool(true), Field::U64(1)));
        assert!(matches!((field(&info.1, "duration_total_ns"), field(&info.1, "duration_p99_ns"),
            field(&info.1, "duration_max_ns"), field(&info.1, "window_ns")),
            (Field::U64(2_000_000), Field::U64(2_000_000), Field::U64(2_000_000), Field::U64(w))
                if w >= SUMMARY_WINDOW.as_nanos() as u64));
        systems.lock().unwrap().remove(&1); drop(handle);
        let _ = tokio::time::timeout(Duration::from_secs(5), exited.changed()).await; assert!(exited.borrow().is_some());
    });
    control.join().unwrap(); db.assert_publication_fixture_unused();
}

#[tokio::test]
async fn idle_window_epochs_and_retry_cadence() {
    let start = Instant::now();
    let timeout = Duration::from_secs(10);
    let mut idle = lifecycle::Idle::new(timeout, false, start);
    assert_eq!(idle.request_due(start), None);
    // A same-instant visit still rotates the epoch; continued idleness does not.
    idle.observe(true, start);
    idle.observe(false, start);
    idle.observe(false, start);
    let due = start + timeout;
    assert_eq!(idle.request_due(due), Some(1));
    assert_eq!(idle.request_due(due), None);
    idle.completed(due);
    let retry = due + lifecycle::IDLE_UNLOAD_RETRY_INTERVAL;
    assert_eq!(idle.request_due(retry - Duration::from_secs(1)), None);
    // The fixed cold interval re-arms the same epoch without reopening the window.
    assert_eq!(idle.request_due(retry), Some(1));
    idle.observe(true, retry);
    assert!(!idle.matches(1) && idle.request_due(retry + timeout).is_none());
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn stale_idle_epoch_is_busy_without_touching_manual_stop() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = crate::tests::shared_sde(&db).await;
    let id = sde.map.public_systems().next().unwrap().solar_system_id;
    let mut system = System::new(id, Arc::default(), 0);
    system.projection_sde = Some(sde);
    let systems = Arc::new(Mutex::new(HashMap::new()));
    let handle = start_mode(system, id, Arc::downgrade(&systems), db, true, Some(Duration::from_secs(60)));
    systems.lock().unwrap().insert(id, handle.clone());
    let extra = handle.clone();
    assert_eq!(handle.request_idle_stop(Some(1)).await.unwrap(), lifecycle::StopDecision::Busy("idle_changed"));
    assert_eq!(handle.request_idle_stop(Some(0)).await.unwrap(), lifecycle::StopDecision::Busy("mailbox_or_sender_busy"));
    drop(extra);
}
