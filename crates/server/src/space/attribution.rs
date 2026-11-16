//! Space tick work attribution: the fixed tick phases, named mailbox jobs and
//! due timers, synchronous destiny delivery, tick lateness, rate-limited slow
//! reports and minute reception summaries. These are server diagnostics only:
//! nothing reaches the client, the database or a background task, and an actor
//! exit drops the open window.

use super::actor::{SLOW_LOCAL_PERIOD, SLOW_PROCESS_LIMIT, SLOW_TOP, SUMMARY_TOP, SUMMARY_WINDOW, TOP_SYSTEMS};
use crate::histogram::Histogram;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;
use tokio::time::Instant;

/// Server diagnostic policy: percentiles reported in summaries.
const P50: u64 = 50;
const P99: u64 = 99;

/// The mutually-exclusive slots of one `System::tick`, in execution order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(in crate::space) enum Phase {
    PruneObservers, Sovereignty, CombatUtc, Projections, Actions, Behavior, Collisions, Positions,
    Missiles, WarpExits, Snapshot, Cloak, Timers, Heat, Npcs, Drones, AdmitEffects, DeliveryFlush,
    DroneAnnouncements, FlushAcks,
}
/// Slot count of `Local::phase_ns`.
pub(in crate::space) const PHASE_COUNT: usize = 20;
const PHASES: [Phase; PHASE_COUNT] = [
    Phase::PruneObservers, Phase::Sovereignty, Phase::CombatUtc, Phase::Projections, Phase::Actions,
    Phase::Behavior, Phase::Collisions, Phase::Positions, Phase::Missiles, Phase::WarpExits,
    Phase::Snapshot, Phase::Cloak, Phase::Timers, Phase::Heat, Phase::Npcs, Phase::Drones,
    Phase::AdmitEffects, Phase::DeliveryFlush, Phase::DroneAnnouncements, Phase::FlushAcks,
];

pub(in crate::space) enum Work { Mailbox, Timer }

#[derive(Clone, Copy, Debug, Default)]
pub(in crate::space) struct Totals {
    pub(in crate::space) count: u64,
    pub(in crate::space) total_ns: u128,
    pub(in crate::space) max_ns: u128,
}
impl Totals {
    fn add(&mut self, ns: u128) { self.count += 1; self.total_ns += ns; self.max_ns = self.max_ns.max(ns); }
    fn absorb(&mut self, other: Self) {
        self.count += other.count;
        self.total_ns += other.total_ns;
        self.max_ns = self.max_ns.max(other.max_ns);
    }
}

#[derive(Default)]
pub(in crate::space) struct NameEntry {
    frame: u64,
    pub(in crate::space) recent: Totals,
    window: Totals,
}

/// Synchronous destiny delivery; already inside the enclosing work duration.
#[derive(Clone, Copy, Default)]
pub(in crate::space) struct Delivery {
    pub(in crate::space) events: u64,
    pub(in crate::space) copies: u64,
    pub(in crate::space) ns: u128,
}
impl Delivery {
    fn add(&mut self, events: u64, copies: u64, ns: u128) {
        self.events += events;
        self.copies += copies;
        self.ns += ns;
    }
}

#[derive(Default)]
pub(in crate::space) struct Window {
    pub(in crate::space) ticks: u64,
    pub(in crate::space) duration: Histogram,
    lateness: Histogram,
    duration_total_ns: u128,
    phase_ns: [u128; PHASE_COUNT],
    delivery: Delivery,
    /// Total mailbox work of every local in this window, including cold ones.
    pub(in crate::space) mailbox: Totals,
    /// Total due-timer work of every local in this window, including cold ones.
    pub(in crate::space) timers: Totals,
    pub(in crate::space) suppressed: u64,
    pub(in crate::space) pilots_max: u64,
}
impl Window {
    pub(in crate::space) fn record(&mut self, duration: Duration, lateness: Duration) {
        self.ticks += 1;
        self.duration.record(duration);
        self.lateness.record(lateness);
        self.duration_total_ns += duration.as_nanos();
    }
    /// Fold one delivered local window in; only piloted windows carry phases.
    fn absorb(&mut self, other: &Self, piloted: bool) {
        self.ticks += other.ticks;
        self.duration.merge(&other.duration);
        self.lateness.merge(&other.lateness);
        self.duration_total_ns += other.duration_total_ns;
        self.delivery.add(other.delivery.events, other.delivery.copies, other.delivery.ns);
        self.mailbox.absorb(other.mailbox);
        self.timers.absorb(other.timers);
        self.suppressed += other.suppressed;
        self.pilots_max = self.pilots_max.max(other.pilots_max);
        if piloted {
            for (slot, ns) in self.phase_ns.iter_mut().zip(other.phase_ns) { *slot += ns; }
        }
    }
}

/// Per-actor attribution state: the open local window and rolling tables.
#[derive(Default)]
pub(in crate::space) struct Local {
    /// When the open local window started; `None` before the first tick.
    pub(in crate::space) started: Option<Instant>,
    pub(in crate::space) window: Window,
    /// This-tick phase nanoseconds, cleared on tick entry.
    pub(in crate::space) phase_ns: [u128; PHASE_COUNT],
    pub(in crate::space) jobs: HashMap<&'static str, NameEntry>,
    pub(in crate::space) timers: HashMap<&'static str, NameEntry>,
    pub(in crate::space) steps: i64,
    /// This-tick synchronous delivery, cleared on tick entry.
    pub(in crate::space) delivery: Delivery,
    /// Rolling frame serial; `NameEntry::recent` resets lazily on change.
    pub(in crate::space) frame: u64,
}
impl Local {
    /// Clear this-tick records on tick entry; the open window survives.
    pub(in crate::space) fn begin_tick(&mut self) {
        self.phase_ns = [0; PHASE_COUNT];
        self.steps = 0;
        self.delivery = Delivery::default();
    }
    pub(in crate::space) fn phase(&mut self, phase: Phase, ns: u128) {
        self.phase_ns[phase as usize] += ns;
        self.window.phase_ns[phase as usize] += ns;
    }
    /// Record one job or timer run under its static name.
    pub(in crate::space) fn work(&mut self, name: &'static str, kind: Work, ns: u128) {
        let (table, total) = match kind {
            Work::Mailbox => (&mut self.jobs, &mut self.window.mailbox),
            Work::Timer => (&mut self.timers, &mut self.window.timers),
        };
        total.add(ns);
        let entry = table.entry(name).or_default();
        if entry.frame != self.frame {
            entry.frame = self.frame;
            entry.recent = Totals::default();
        }
        entry.recent.add(ns);
        entry.window.add(ns);
    }
    pub(in crate::space) fn delivered(&mut self, events: u64, copies: u64, ns: u128) {
        self.delivery.add(events, copies, ns);
        self.window.delivery.add(events, copies, ns);
    }
    /// Emit the full slow report; called only after rate-limit admission.
    pub(in crate::space) fn slow_report(&self, system: i64, scheduled: Instant, started: Instant,
        last_mailbox_end: Option<Instant>, duration: Duration, lateness: Duration,
        mailbox_depth: usize, (balls, observers, pilots): (u64, u64, u64)) {
        let (own, runtime) = split_lateness(scheduled, started, last_mailbox_end);
        let (mailbox_count, mailbox_total_ns, mailbox_top) = recent_top::<SLOW_TOP>(&self.jobs, self.frame);
        let (timer_count, timer_total_ns, timer_top) = recent_top::<SLOW_TOP>(&self.timers, self.frame);
        tracing::warn!(
            target: "infinity_server::space::actor", system,
            duration_ns = duration.as_nanos() as u64, lateness_ns = lateness.as_nanos() as u64,
            own_lateness_ns = own.as_nanos() as u64, runtime_lateness_ns = runtime.as_nanos() as u64,
            steps = self.steps, balls, observers, pilots, mailbox_depth,
            top_phases = ?top::<SLOW_TOP, _>(PHASES.iter().map(|p| (*p, self.phase_ns[*p as usize]))),
            mailbox_count, mailbox_total_ns = mailbox_total_ns as u64, mailbox_top = ?mailbox_top,
            timer_count, timer_total_ns = timer_total_ns as u64, timer_top = ?timer_top,
            delivery_events = self.delivery.events,
            delivery_copies = self.delivery.copies,
            delivery_ns = self.delivery.ns as u64,
            "space tick attribution");
    }
}

/// Recent-frame totals and the top `N` names of one work table.
fn recent_top<const N: usize>(table: &HashMap<&'static str, NameEntry>, frame: u64)
    -> (u64, u128, [Option<(&'static str, Totals)>; N]) {
    let (mut count, mut total_ns) = (0, 0);
    let ranked = top::<N, _>(table.iter()
        .filter(|(_, entry)| entry.frame == frame)
        .map(|(&name, entry)| {
            count += entry.recent.count;
            total_ns += entry.recent.total_ns;
            (name, entry.recent.total_ns)
        }));
    (count, total_ns, ranked.map(|slot| slot.map(|(name, _)| (name, table[name].recent))))
}

/// Rolling `N`-per-period admission ring.
pub(in crate::space) struct Limiter<const N: usize> {
    slots: [Option<Instant>; N],
    next: usize,
}
impl<const N: usize> Default for Limiter<N> {
    fn default() -> Self { Self { slots: [None; N], next: 0 } }
}
impl<const N: usize> Limiter<N> {
    /// Admit when the oldest outstanding slot is empty or outside the period.
    pub(in crate::space) fn allow(&mut self, now: Instant, period: Duration) -> bool {
        if self.slots[self.next].is_none_or(|at| now.saturating_duration_since(at) >= period) {
            self.slots[self.next] = Some(now);
            self.next = (self.next + 1) % N;
            return true;
        }
        false
    }
}

/// Admit one slow-report candidate: the per-system slot is consumed first,
/// then the process ring; a rejection at either level counts exactly once.
pub(in crate::space) fn admit_slow(now: Instant, local: &mut Limiter<1>, suppressed: &mut u64,
    global: impl FnOnce() -> bool) -> bool {
    let admitted = local.allow(now, SLOW_LOCAL_PERIOD) && global();
    if !admitted { *suppressed += 1; }
    admitted
}

/// Split tick lateness: `own` is the part the last mailbox job can explain (an
/// upper bound), `runtime` the residual. The pair sums to the full lateness.
pub(in crate::space) fn split_lateness(scheduled: Instant, started: Instant,
    last_mailbox_end: Option<Instant>) -> (Duration, Duration) {
    let late = started.saturating_duration_since(scheduled);
    let own = last_mailbox_end.map_or(Duration::ZERO, |end| end.saturating_duration_since(scheduled).min(late));
    (own, late - own)
}

/// The `N` highest-value items, ties broken by ascending key.
pub(in crate::space) fn top<const N: usize, K: Ord>(items: impl Iterator<Item = (K, u128)>)
    -> [Option<(K, u128)>; N] {
    let mut best: [Option<(K, u128)>; N] = std::array::from_fn(|_| None);
    let mut len = 0;
    for (key, value) in items {
        let mut index = 0;
        while index < len {
            let slot = best[index].as_ref().unwrap();
            if value > slot.1 || (value == slot.1 && key < slot.0) { break; }
            index += 1;
        }
        if index == N { continue; }
        let mut slot = len.min(N - 1);
        while slot > index { best[slot] = best[slot - 1].take(); slot -= 1; }
        best[index] = Some((key, value));
        len = (len + 1).min(N);
    }
    best
}

#[derive(Clone, Copy, Default)]
struct SystemTotals {
    tick_total_ns: u128,
    pilots_max: u64,
}

#[derive(Default)]
pub(in crate::space) struct Process {
    start: Option<Instant>,
    reported: bool,
    pub(in crate::space) window: Window,
    mailbox: HashMap<&'static str, Totals>,
    timers: HashMap<&'static str, Totals>,
    systems: HashMap<i64, SystemTotals>,
}
impl Process {
    /// Merge one complete local window; when the process window reaches
    /// `SUMMARY_WINDOW` of actual elapsed time, return its summary and reset.
    pub(in crate::space) fn merge(&mut self, now: Instant, system: i64, local: &mut Local) -> Option<Summary> {
        let start = self.start.unwrap_or_else(|| local.started.unwrap_or(now));
        self.start = Some(start);
        let piloted = local.window.pilots_max > 0;
        self.window.absorb(&local.window, piloted);
        for (names, totals) in [(&mut local.jobs, &mut self.mailbox), (&mut local.timers, &mut self.timers)] {
            for (&name, entry) in names.iter_mut() {
                if piloted { totals.entry(name).or_default().absorb(entry.window); }
                entry.window = Totals::default();
            }
        }
        if piloted {
            let totals = self.systems.entry(system).or_default();
            totals.tick_total_ns += local.window.duration_total_ns;
            totals.pilots_max = totals.pilots_max.max(local.window.pilots_max);
        }
        local.window = Window::default();
        local.started = Some(now);
        let elapsed = now.saturating_duration_since(start);
        if elapsed < SUMMARY_WINDOW { return None; }
        let summary = Summary {
            window_ns: elapsed.as_nanos(),
            partial: !self.reported,
            top_phases: top::<SUMMARY_TOP, _>(PHASES.iter().map(|p| (*p, self.window.phase_ns[*p as usize]))),
            top_mailbox: top::<SUMMARY_TOP, _>(self.mailbox.iter().map(|(&name, t)| (name, t.total_ns)))
                .map(|slot| slot.map(|(name, _)| (name, self.mailbox[name]))),
            top_timers: top::<SUMMARY_TOP, _>(self.timers.iter().map(|(&name, t)| (name, t.total_ns)))
                .map(|slot| slot.map(|(name, _)| (name, self.timers[name]))),
            top_systems: top::<TOP_SYSTEMS, _>(self.systems.iter().map(|(&id, s)| (id, s.tick_total_ns)))
                .map(|slot| slot.map(|(id, ns)| (id, ns, self.systems[&id].pilots_max))),
            window: std::mem::take(&mut self.window),
        };
        self.reported = true;
        self.mailbox.clear();
        self.timers.clear();
        self.systems.clear();
        self.start = Some(now);
        Some(summary)
    }
}

pub(in crate::space) struct Summary {
    pub(in crate::space) window_ns: u128,
    pub(in crate::space) partial: bool,
    pub(in crate::space) window: Window,
    pub(in crate::space) top_phases: [Option<(Phase, u128)>; SUMMARY_TOP],
    pub(in crate::space) top_mailbox: [Option<(&'static str, Totals)>; SUMMARY_TOP],
    pub(in crate::space) top_timers: [Option<(&'static str, Totals)>; SUMMARY_TOP],
    pub(in crate::space) top_systems: [Option<(i64, u128, u64)>; TOP_SYSTEMS],
}
impl Summary {
    /// Emit this report as one INFO line on the actor's diagnostics target.
    pub(in crate::space) fn log(&self, system: i64) {
        tracing::info!(
            target: "infinity_server::space::actor", system,
            window_ns = self.window_ns as u64, partial = self.partial,
            ticks = self.window.ticks,
            duration_total_ns = self.window.duration_total_ns as u64,
            duration_p50_ns = self.window.duration.percentile(self.window.ticks, P50).as_nanos() as u64,
            duration_p99_ns = self.window.duration.percentile(self.window.ticks, P99).as_nanos() as u64,
            duration_max_ns = self.window.duration.max.as_nanos() as u64,
            lateness_p50_ns = self.window.lateness.percentile(self.window.ticks, P50).as_nanos() as u64,
            lateness_p99_ns = self.window.lateness.percentile(self.window.ticks, P99).as_nanos() as u64,
            lateness_max_ns = self.window.lateness.max.as_nanos() as u64,
            top_phases = ?self.top_phases, top_mailbox = ?self.top_mailbox,
            top_timers = ?self.top_timers, top_systems = ?self.top_systems,
            mailbox_count = self.window.mailbox.count,
            mailbox_total_ns = self.window.mailbox.total_ns as u64,
            timer_count = self.window.timers.count,
            timer_total_ns = self.window.timers.total_ns as u64,
            delivery_events = self.window.delivery.events,
            delivery_copies = self.window.delivery.copies,
            delivery_ns = self.window.delivery.ns as u64,
            suppressed = self.window.suppressed,
            "space tick summary");
    }
}

/// Process-wide reception window and rolling slow-emission ring.
static PROCESS: OnceLock<Mutex<Process>> = OnceLock::new();
static SLOW: OnceLock<Mutex<Limiter<SLOW_PROCESS_LIMIT>>> = OnceLock::new();

fn process() -> &'static Mutex<Process> {
    PROCESS.get_or_init(|| Mutex::new(Process::default()))
}
/// The process-wide slow-report ring; independent of the summary lock.
pub(in crate::space) fn slow_limit() -> &'static Mutex<Limiter<SLOW_PROCESS_LIMIT>> {
    SLOW.get_or_init(|| Mutex::new(Limiter::default()))
}

/// Close a completed local window into the process reception window. Called at
/// each actor tick start; returns a summary when the process window closed.
pub(in crate::space) fn close_window(local: &mut Local, now: Instant, system: i64) -> Option<Summary> {
    match local.started {
        Some(started) if now.saturating_duration_since(started) >= SUMMARY_WINDOW => {
            crate::poison::lock_or_exit(process().lock()).merge(now, system, local)
        }
        None => { local.started = Some(now); None }
        Some(_) => None,
    }
}

#[cfg(all(test, feature = "tests-space"))] mod tests;
