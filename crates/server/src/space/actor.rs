//! Per-system closure mailbox and wall-aligned timer/NPC driver for ballparks
//! whose destiny updates are consumed by `eve/client/script/remote/michelle.py`.
//! The actor owns in-memory `System` state; snapshot jobs persist pilot positions
//! to `game.items`, while NPC effects and deferred jobs run outside the actor.
//! Its `interval_at` starts at the next whole Unix 100 ms boundary and uses
//! `MissedTickBehavior::Skip`. Housekeeping runs every interval; destiny motion
//! advances only on whole-second boundaries. Mailbox messages never evolve balls.
//! The operator-configured mailbox budget controls depth warnings; slow or late
//! ticks and per-minute reception summaries are attributed through `attribution`.
//! A cold control channel accepts idle-stop requests when GM performance
//! testing or the operator-configured automatic idle unload is enabled;
//! otherwise no control channels are created and the existing mailbox and
//! tick processing remain in use. The idle window only counts consecutive
//! intervals without pilots or observers; maintenance work does not reset it,
//! and a refused automatic attempt retries at a fixed cold interval without
//! restarting the window.
//! This module does not decode the client protocol or perform database work
//! inside the actor; timers and non-position dynamic state are not persistent.
use super::*;
use std::sync::Weak;
use std::time::Duration;
use tokio::time::{Instant, MissedTickBehavior};
use tracing::{trace, warn};

pub(super) mod lifecycle;
mod retirement;

/// Server policy: the simulation tick interval.
const TICK_INTERVAL_MS: u64 = 100;
/// Server diagnostic policy: report candidates are ticks slower than the
/// duration bound or started later than the lateness bound; emission is
/// limited per system, then across the process.
const SLOW_TICK_DURATION: Duration = Duration::from_millis(20);
const SLOW_TICK_LATENESS: Duration = Duration::from_millis(50);
pub(super) const SLOW_LOCAL_PERIOD: Duration = Duration::from_secs(10);
const SLOW_PROCESS_PERIOD: Duration = Duration::from_secs(1);
pub(super) const SLOW_PROCESS_LIMIT: usize = 5;
/// Server diagnostic policy: reception window length and report list sizes.
pub(super) const SUMMARY_WINDOW: Duration = Duration::from_secs(60);
pub(super) const SLOW_TOP: usize = 3;
pub(super) const SUMMARY_TOP: usize = 5;
pub(super) const TOP_SYSTEMS: usize = 3;

/// One mailbox job or due timer plus its static creation-site name.
pub(super) struct Job {
    name: &'static str,
    callback: Box<dyn FnOnce(&mut System) + Send>,
}
impl Job {
    pub(super) fn new<F: FnOnce(&mut System) + Send + 'static>(f: F) -> Self {
        Self::named(std::any::type_name::<F>(), f)
    }
    pub(super) fn named<F: FnOnce(&mut System) + Send + 'static>(name: &'static str, f: F) -> Self {
        Self { name, callback: Box::new(f) }
    }
    /// Run as a mailbox job; returns the single post-run clock read.
    pub(super) fn run(self, system: &mut System) -> Instant {
        self.run_as(system, attribution::Work::Mailbox)
    }
    /// Run recording under `kind`; returns the single post-run clock read.
    pub(super) fn run_as(self, system: &mut System, kind: attribution::Work) -> Instant {
        let start = Instant::now();
        (self.callback)(system);
        let end = Instant::now();
        system.attribution.work(self.name, kind, end.saturating_duration_since(start).as_nanos());
        end
    }
}
#[derive(Clone)]
pub(super) struct SystemHandle {
    pub(super) tx: mpsc::UnboundedSender<Job>,
    pub(super) gate: Arc<tokio::sync::Mutex<()>>,
    pub(super) state: Arc<lifecycle::ActorState>,
}
impl SystemHandle {
    pub(super) fn is_closed(&self) -> bool { self.tx.is_closed() }
    pub(super) fn send(&self, job: Job) -> std::result::Result<(), mpsc::error::SendError<Job>> { self.tx.send(job) }
    pub(super) async fn with<R: Send + 'static>(&self, f: impl FnOnce(&mut System) -> R + Send + 'static) -> Option<R> {
        self.with_named(std::any::type_name_of_val(&f), f).await
    }
    pub(super) async fn with_named<R: Send + 'static, F: FnOnce(&mut System) -> R + Send + 'static>(&self, name: &'static str, f: F) -> Option<R> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.send(Job::named(name, move |s| { let _ = tx.send(f(s)); })).ok()?;
        rx.await.ok()
    }
}

pub(super) async fn after(handle: Option<SystemHandle>, delay_ms: u64) -> bool {
    let Some(handle) = handle else { return false };
    let (tx, rx) = tokio::sync::oneshot::channel();
    let timer = move |_: &mut System| { let _ = tx.send(()); };
    let name = std::any::type_name_of_val(&timer);
    if handle.send(Job::named(name, move |s| s.schedule_named(delay_ms, name, timer))).is_err() {
        return false;
    }
    rx.await.is_ok()
}

impl World {
    /// Send one named job to a running actor; never loads a missing system.
    pub(super) fn post_named<F: FnOnce(&mut System) + Send + 'static>(&self, id: i64, name: &'static str, f: F) {
        if let Some(handle) = self.running(id) { let _ = handle.send(Job::named(name, f)); }
    }
}

fn aligned_interval(wall_ms: i64, instant: Instant) -> tokio::time::Interval {
    let offset = next_aligned_ms(wall_ms, TICK_INTERVAL_MS as i64) - wall_ms;
    let mut interval = tokio::time::interval_at(
        instant + Duration::from_millis(offset as u64),
        Duration::from_millis(TICK_INTERVAL_MS),
    );
    interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
    interval
}

pub(super) fn start(system: System, id: i64, systems: Weak<Mutex<HashMap<i64, SystemHandle>>>, db: Db) -> SystemHandle {
    let idle_secs = crate::config::game().space.idle_unload_secs;
    let enabled = crate::config::game().gm.performance_testing || idle_secs != 0;
    let idle_after = (idle_secs != 0).then(|| Duration::from_secs(idle_secs as u64));
    start_mode(system, id, systems, db, enabled, idle_after)
}

fn start_mode(mut system: System, id: i64, systems: Weak<Mutex<HashMap<i64, SystemHandle>>>, db: Db, enabled: bool, idle_after: Option<Duration>) -> SystemHandle {
    let workers = match npc::worker::start(&mut system, enabled) {
        npc::worker::Workers::Production { tasks } => (None, tasks),
        npc::worker::Workers::Controlled { probes, tasks } => (Some(probes), tasks),
    };
    let (probes, tasks) = workers;
    let (control, mut stop_rx, exit_tx) = if enabled {
        let (stop, rx) = mpsc::unbounded_channel();
        let (exit_tx, exited) = tokio::sync::watch::channel(None);
        (Some(lifecycle::Control { stop, exited, stopping: std::sync::atomic::AtomicBool::new(false) }), Some(rx), Some(exit_tx))
    } else { (None, None, None) };
    let (tx, mut rx) = mpsc::unbounded_channel::<Job>();
    let identity = tx.downgrade();
    let handle = SystemHandle { tx, gate: Arc::new(tokio::sync::Mutex::new(())), state: lifecycle::ActorState::share(control) };
    let gate = Arc::downgrade(&handle.gate);
    let state = handle.state.clone();
    let actor_state = state.clone();
    let actor_systems = systems.clone();
    system.snapshot_context = Some(snapshot::Context {
        tx: handle.tx.downgrade(), gate: handle.gate.clone(), state: handle.state.clone(), db,
    });
    let owned = Arc::new(tokio::sync::Mutex::new(system));
    let child_system = owned.clone();
    let mut child_guard = retirement::ChildSystem::new(actor_state.clone());
    let task = tokio::spawn(async move {
        let mut system = child_system.lock().await;
        let mut interval = aligned_interval(now_ms(), Instant::now());
        let mut slow_limiter = attribution::Limiter::<1>::default();
        let mut last_mailbox_end = None;
        let mut idle = idle_after.map(|timeout| lifecycle::Idle::new(timeout, system.unload_occupied(), Instant::now()));
        let mut unload_task: Option<tokio::task::JoinHandle<Instant>> = None;
        loop {
            tokio::select! {
                request = async {
                    match &mut stop_rx {
                        Some(rx) => rx.recv().await,
                        None => std::future::pending().await,
                    }
                }, if enabled => {
                    let Some(request) = request else { stop_rx = None; continue };
                    if let Some(epoch) = request.idle_epoch {
                        if idle.as_ref().is_none_or(|i| !i.matches(epoch)) {
                            let _ = request.reply.send(Ok(lifecycle::StopDecision::Busy("idle_changed")));
                            continue;
                        }
                    }
                    if lifecycle::try_idle_stop(request.reply, probes.as_ref().expect("controlled workers"), &mut system,
                        &mut rx, &actor_systems, &gate, &actor_state, id).await { child_guard.disarm(); break; }
                },
                message = rx.recv() => match message {
                    Some(job) => {
                        let end = job.run(&mut system);
                        last_mailbox_end = Some(end);
                        if let Some(idle) = &mut idle { idle.observe(system.unload_occupied(), end); }
                    }
                    None => { child_guard.disarm(); break; }
                },
                scheduled = interval.tick() => {
                    let boundary = Instant::now();
                    if let Some(summary) = attribution::close_window(&mut system.attribution, boundary, id) {
                        summary.log(id);
                    }
                    let depth = rx.len();
                    let start = Instant::now();
                    let lateness = start.saturating_duration_since(scheduled);
                    system.tick(now_ms());
                    if let Some(idle) = &mut idle {
                        let end = Instant::now();
                        idle.observe(system.unload_occupied(), end);
                        if unload_task.is_none() {
                            if let Some(epoch) = idle.request_due(end) {
                                unload_task = Some(unload::request_auto_unload(system.world.clone(), id, gate.clone(), epoch));
                            }
                        }
                    }
                    let duration = start.elapsed();
                    system.attribution.window.record(duration, lateness);
                    let counts = system.attribution_counts();
                    system.attribution.window.pilots_max = system.attribution.window.pilots_max.max(counts.2);
                    trace!(system = id, duration_us = duration.as_micros() as u64,
                        lateness_us = lateness.as_micros() as u64, mailbox_depth = depth, "space tick");
                    if (duration > SLOW_TICK_DURATION || lateness > SLOW_TICK_LATENESS)
                        && attribution::admit_slow(start, &mut slow_limiter, &mut system.attribution.window.suppressed,
                            || crate::poison::lock_or_exit(attribution::slow_limit().lock()).allow(start, SLOW_PROCESS_PERIOD)) {
                        system.attribution.slow_report(id, scheduled, start, last_mailbox_end,
                            duration, lateness, depth, counts);
                    }
                    system.attribution.frame += 1;
                    if depth as u128 > crate::config::game().space.mailbox_budget as u128 { warn!(system = id, mailbox_depth = depth, "space mailbox exceeds budget"); }
                },
                finished = async {
                    match &mut unload_task {
                        Some(task) => task.await,
                        None => std::future::pending().await,
                    }
                } => {
                    let at = match finished {
                        Ok(at) => at,
                        Err(error) => {
                            warn!(system = id, %error, "space idle unload task failed");
                            Instant::now()
                        }
                    };
                    if let Some(idle) = &mut idle { idle.completed(at); }
                    unload_task = None;
                }
            }
        }
    });
    tokio::spawn(async move {
        let actor_ok = match task.await {
            Ok(()) => true,
            Err(error) => { warn!(system = id, %error, "space actor stopped"); false }
        };
        let mut system = Arc::into_inner(owned).expect("actor supervisor joins the only other owner").into_inner();
        let explicit = exit_tx.is_some() && state.control.as_ref().is_some_and(|c| c.stopping.load(std::sync::atomic::Ordering::Acquire));
        if !actor_ok && !explicit {
            retirement::retire(system, id, state, tasks, systems).await;
            return;
        }
        // Only a proven explicit stop may move a certified NPC cohort into the
        // owning world's sleep store; every other exit drops it with the
        // system, and a failed move contradicts the final stop proof.
        let (payload, world) = if actor_ok && explicit {
            let payload = system.take_sleeping_npcs().unwrap_or_else(|_reason| {
                crate::poison::containment_lost(crate::space::containment::ContainmentFailure::ExplicitStopProofContradiction, id, None)
            });
            (payload, Some(system.world.clone()))
        } else { (None, None) };
        // Dropping the recovered system closes both worker sender endpoints so
        // the joins below observe a real end of stream.
        drop(system);
        let workers_ok = tasks.join().await;
        let mut cleaned = false;
        if let Some(systems) = systems.upgrade() {
            let mut systems = crate::poison::lock_or_exit(systems.lock());
            if systems.get(&id).is_some_and(|h| identity.upgrade().is_some_and(|tx| tx.same_channel(&h.tx))) {
                // Inside this same-instance critical section, the sleep entry
                // must land before the registry removes the old handle so a
                // cold load always sees exactly one owner of the cohort.
                let placed = match payload {
                    None => true,
                    Some(payload) => match world.and_then(|weak| weak.upgrade()) {
                        Some(world) => {
                            let mut sleeping = crate::poison::lock_or_exit(world.sleeping_npcs.lock());
                            if let std::collections::hash_map::Entry::Vacant(entry) = sleeping.entry(id) {
                                let survivors = payload.survivors.len();
                                entry.insert(payload);
                                tracing::debug!(system = id, survivors, "space NPC cohort sleeping");
                                true
                            } else {
                                // Impossible by construction; the existing entry
                                // stays the sole owner and the exit reports Failed.
                                tracing::error!(system = id, "sleeping NPC entry already exists");
                                false
                            }
                        }
                        // The owner world is gone; same-process NPC state is not
                        // persistent, so the payload drops with it.
                        None => false,
                    },
                };
                if placed {
                    systems.remove(&id);
                    cleaned = true;
                }
            }
        }
        if let Some(exit_tx) = exit_tx {
            let outcome = if actor_ok && workers_ok && cleaned { lifecycle::ExitOutcome::Clean } else { lifecycle::ExitOutcome::Failed };
            let _ = exit_tx.send(Some(outcome));
        }
        if explicit && !(actor_ok && workers_ok && cleaned) {
            crate::poison::containment_lost(crate::space::containment::ContainmentFailure::ExplicitStopProofContradiction, id, None);
        }
    });
    handle
}

#[cfg(all(test, feature = "tests-space"))] mod tests;
