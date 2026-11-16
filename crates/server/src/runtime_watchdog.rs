//! Server-only RPC, deferred, scheduled and progress poll diagnostics; no client protocol or persistent data.
//! The runtime probe shares workers and cannot observe while they are all blocked.
use std::{cmp::Reverse, collections::{hash_map::Entry, HashMap}, future::{poll_fn, Future}};
use std::{fmt, sync::{Mutex, OnceLock}, time::Duration};
use tokio::time::{Instant, MissedTickBehavior};
use crate::histogram::Histogram;

/// Server diagnostic policy: slow synchronous poll threshold.
const SLOW_POLL: Duration = Duration::from_millis(20);
/// Server diagnostic policy: slow completion threshold.
const SLOW_TOTAL: Duration = Duration::from_secs(1);
/// Server diagnostic policy: reporting window duration.
const WINDOW: Duration = Duration::from_secs(60);
/// Server diagnostic policy: maximum distinct RPC names per window.
const CAPACITY: usize = 512;
/// Server diagnostic policy: number of ranked entries per measure.
const TOP: usize = 5;
/// Server diagnostic policy: reported percentiles.
const PERCENTILES: [u64; 2] = [50, 99];
/// Server diagnostic policy: runtime wake sampling period.
const PROBE_PERIOD: Duration = Duration::from_millis(100);
/// Server diagnostic policy: economy holder inspection period.
const HOLDER_PERIOD: Duration = Duration::from_secs(1);

#[derive(Default, Clone, Copy)]
pub(crate) struct Timing {
    total: Duration, polls: u64,
    poll_sum: Duration, poll_max: Duration,
}
impl Timing {
    fn slow(&self) -> bool { self.poll_max > SLOW_POLL || self.total > SLOW_TOTAL }
    fn warn(&self, kind: &str, name: impl fmt::Display) {
        if self.slow() {
            tracing::warn!(kind, name = %name, total_ns = self.total.as_nanos(), polls = self.polls,
                poll_sum_ns = self.poll_sum.as_nanos(), poll_max_ns = self.poll_max.as_nanos(), "runtime slow future");
        }
    }
}
pub(crate) async fn measure<F: Future>(future: F) -> (F::Output, Timing) {
    let started = Instant::now();
    let mut timing = Timing::default();
    let mut future = std::pin::pin!(future);
    poll_fn(|cx| {
        let start = Instant::now();
        let result = future.as_mut().poll(cx);
        let end = Instant::now();
        let elapsed = end - start;
        timing.polls += 1;
        timing.poll_sum += elapsed;
        timing.poll_max = timing.poll_max.max(elapsed);
        if result.is_ready() { timing.total = end - started; }
        result.map(|output| (output, timing))
    }).await
}
pub(crate) async fn watch<F: Future>(kind: &'static str, name: &str, future: F) -> F::Output {
    let (output, timing) = measure(future).await;
    timing.warn(kind, name);
    output
}

#[derive(Default, Debug)]
struct Polls { poll_sum_ns: u128, poll_max_ns: u128 }
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
enum Name<'a> { Rpc(&'a str, &'a str), Overflow(&'static str) } // Real names win equal-duration ties.
#[derive(Default)]
struct RpcWindow {
    start: Option<Instant>, count: u64, total: Histogram,
    names: HashMap<(String, String), Polls>,
    overflow: Polls, overflow_calls: u64,
}
impl RpcWindow {
    fn record(&mut self, now: Instant, key: (String, String), timing: Timing) -> Option<(Duration, Self)> {
        let elapsed = now - *self.start.get_or_insert(now);
        let closed = (elapsed >= WINDOW && self.count != 0).then(|| (elapsed,
            std::mem::replace(self, Self { start: Some(now), ..Self::default() })));
        self.count += 1;
        self.total.record(timing.total);
        let room = self.names.len() < CAPACITY;
        let entry = match self.names.entry(key) {
            Entry::Occupied(entry) => entry.into_mut(),
            Entry::Vacant(entry) if room => entry.insert(Polls::default()),
            Entry::Vacant(_) => {
                self.overflow_calls += 1;
                &mut self.overflow
            }
        };
        entry.poll_sum_ns += timing.poll_sum.as_nanos();
        entry.poll_max_ns = entry.poll_max_ns.max(timing.poll_max.as_nanos());
        closed
    }
    fn top<'a>(&'a self, maximum: bool) -> [Option<(Name<'a>, &'a Polls)>; TOP] {
        let mut top = [None; TOP];
        let rank = |(name, polls): (Name<'a>, &'a Polls)| (
            if maximum { polls.poll_max_ns } else { polls.poll_sum_ns }, Reverse(name));
        let names = self.names.iter().map(|((service, method), polls)| (Name::Rpc(service, method), polls));
        for entry in names.chain((self.overflow_calls != 0).then_some((Name::Overflow("<overflow>"), &self.overflow))) {
            if let Some(index) = top.iter().position(|old| old.is_none_or(|old| rank(entry) > rank(old))) {
                top[index..].rotate_right(1);
                top[index] = Some(entry);
            }
        }
        top
    }
}
fn state() -> &'static Mutex<RpcWindow> {
    static STATE: OnceLock<Mutex<RpcWindow>> = OnceLock::new();
    STATE.get_or_init(Default::default)
}

pub(crate) fn rpc(key: (String, String), timing: Timing) {
    timing.warn("rpc", format_args!("{}.{}", key.0, key.1));
    let closed = crate::poison::lock_or_exit(state().lock()).record(Instant::now(), key, timing); // Unlock before logging.
    if let Some((elapsed, window)) = closed {
        tracing::info!(window_ns = elapsed.as_nanos(), count = window.count,
            total_p50_ns = window.total.percentile(window.count, PERCENTILES[0]).as_nanos(),
            total_p99_ns = window.total.percentile(window.count, PERCENTILES[1]).as_nanos(),
            total_max_ns = window.total.max.as_nanos(), overflow_calls = window.overflow_calls,
            overflow_poll_sum_ns = window.overflow.poll_sum_ns, overflow_poll_max_ns = window.overflow.poll_max_ns,
            top_poll_sum = ?window.top(false), top_poll_max = ?window.top(true), "rpc timing summary");
    }
}

pub(crate) async fn probe() {
    let mut start = Instant::now();
    let mut inspect_at = start + HOLDER_PERIOD;
    let mut interval = tokio::time::interval_at(start + PROBE_PERIOD, PROBE_PERIOD);
    interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let (mut lateness, mut count) = (Histogram::default(), 0u64);
    loop {
        let scheduled = interval.tick().await;
        let now = Instant::now();
        lateness.record(now.saturating_duration_since(scheduled));
        count += 1;
        if now >= inspect_at {
            crate::services::market::economy_lock::check_holder(now);
            inspect_at = now + HOLDER_PERIOD;
        }
        if now - start >= WINDOW {
            tracing::info!(window_ns = (now - start).as_nanos(), samples = count,
                lateness_p50_ns = lateness.percentile(count, PERCENTILES[0]).as_nanos(),
                lateness_p99_ns = lateness.percentile(count, PERCENTILES[1]).as_nanos(),
                lateness_max_ns = lateness.max.as_nanos(), "runtime scheduling summary");
            (start, lateness, count) = (now, Histogram::default(), 0);
        }
    }
}
#[cfg(all(test, feature = "tests-runtime"))] mod tests;
