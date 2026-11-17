//! Server-only wall-clock diagnostics for database operations. Each finished
//! checkout, statement, transaction or commit is one timed sample, aggregated
//! per kind into fixed summary windows and per caller source location; samples
//! slower than the kind's threshold produce a throttled warning. Operations
//! cancelled or never polled record no sample; these counters are never sent
//! to a client, persisted or exported. Call sites beyond the fixed per-kind
//! tables still count toward every global statistic but are disclosed only as
//! overflow, so a window with overflow carries no complete site ranking.

use crate::histogram::Histogram;
use std::{future::Future, panic::Location, sync::{Mutex, OnceLock, atomic::{AtomicBool, Ordering}}, time::Duration};
use tokio::time::Instant;

/// Server diagnostic policy: a window closes on the first finished operation
/// at least this long after it opened; that operation starts the next window.
const WINDOW: Duration = Duration::from_secs(60);
/// Server diagnostic policy: call sites tracked exactly per kind.
const SITES_CAPACITY: usize = 512;
/// Server diagnostic policy: sites named per kind in a window summary.
const TOP_COUNT: usize = 5;
/// Server diagnostic policy: reported percentiles, over a base of PERCENT.
const PERCENTILES: [u64; 2] = [50, 99];
/// Server diagnostic policy: minimum warning spacing for one site-kind and
/// for one kind's overflow fallback.
const WARN_PERIOD: Duration = Duration::from_secs(10);
/// Server diagnostic policy: a checkout strictly slower than this warns.
const CHECKOUT_WARN: Duration = Duration::from_millis(50);
/// Server diagnostic policy: a statement strictly slower than this warns.
const STATEMENT_WARN: Duration = Duration::from_millis(100);
/// Server diagnostic policy: a transaction strictly slower than this warns.
const TRANSACTION_WARN: Duration = Duration::from_secs(1);
/// Server diagnostic policy: a commit strictly slower than this warns.
const COMMIT_WARN: Duration = Duration::from_millis(100);

/// Database operation category: a pool checkout, one statement on a borrowed
/// connection, a whole transaction scope, or the COMMIT that ends it.
#[derive(Clone, Copy)]
pub(super) enum Kind { Checkout, Statement, Transaction, Commit }

const KIND_COUNT: usize = 4;

impl Kind {
    const ALL: [Kind; KIND_COUNT] = [Kind::Checkout, Kind::Statement, Kind::Transaction, Kind::Commit];
    fn index(self) -> usize { self as usize }
    fn name(self) -> &'static str { ["checkout", "statement", "transaction", "commit"][self.index()] }
    fn warn_threshold(self) -> Duration {
        [CHECKOUT_WARN, STATEMENT_WARN, TRANSACTION_WARN, COMMIT_WARN][self.index()]
    }
}

/// One tracked call site of one kind; `warn_after` survives window rollover.
#[derive(Clone, Copy, Default)]
struct Site { location: Option<&'static Location<'static>>, count: u64, total: Duration, max: Duration, warn_after: Option<Instant> }
/// Call-site table sorted by complete `Location`, occupied prefix; identities
/// and warn throttles persist across windows.
struct Sites { entries: [Site; SITES_CAPACITY], occupied: usize }

impl Default for Sites {
    fn default() -> Self { Sites { entries: [Site::default(); SITES_CAPACITY], occupied: 0 } }
}

/// One ranked call site of a closed window.
#[derive(Clone, Copy)]
struct TopSite { location: &'static Location<'static>, count: u64, total: Duration, max: Duration }

fn outranks(candidate: &TopSite, current: &TopSite) -> bool {
    (candidate.total, std::cmp::Reverse(candidate.location))
        > (current.total, std::cmp::Reverse(current.location))
}

impl Sites {
    /// The slot for `caller`, inserted in sort order, or `None` when full.
    fn slot(&mut self, caller: &'static Location<'static>) -> Option<&mut Site> {
        let index = self.entries[..self.occupied]
            .binary_search_by(|site| site.location.cmp(&Some(caller)))
            .unwrap_or_else(|index| index);
        if index < self.occupied && self.entries[index].location == Some(caller) {
            return Some(&mut self.entries[index]);
        }
        if self.occupied == SITES_CAPACITY { return None; }
        self.entries.copy_within(index..self.occupied, index + 1);
        self.entries[index] = Site { location: Some(caller), ..Site::default() };
        self.occupied += 1;
        Some(&mut self.entries[index])
    }

    fn clear_window(&mut self) {
        for site in &mut self.entries[..self.occupied] {
            (site.count, site.total, site.max) = (0, Duration::ZERO, Duration::ZERO);
        }
    }

    /// Sites with samples this window ranked by descending `total`; ties keep
    /// the earlier source position.
    fn top(&self) -> [Option<TopSite>; TOP_COUNT] {
        let mut top: [Option<TopSite>; TOP_COUNT] = [None; TOP_COUNT];
        for site in &self.entries[..self.occupied] {
            let (Some(location), true) = (site.location, site.count != 0) else { continue };
            let candidate = TopSite { location, count: site.count, total: site.total, max: site.max };
            if let Some(index) = top.iter().position(|old| old.is_none_or(|old| outranks(&candidate, &old))) {
                top[index..].rotate_right(1);
                top[index] = Some(candidate);
            }
        }
        top
    }
}

/// One kind's per-window statistics; `overflow_warn` throttles warnings for
/// untracked sites and persists across windows like the per-site throttles.
#[derive(Default)]
struct KindStats {
    count: u64, total: Duration, histogram: Histogram, sites: Sites,
    overflow_count: u64, overflow_total: Duration, overflow_max: Duration, overflow_warn: Option<Instant>,
}

/// One kind's closed-window numbers; `top` is exact over tracked sites and
/// incomplete while `overflow_count` is nonzero.
struct KindSummary {
    count: u64, p50: Duration, p99: Duration, max: Duration, total: Duration,
    top: [Option<TopSite>; TOP_COUNT],
    overflow_count: u64, overflow_total: Duration, overflow_max: Duration,
}

impl KindStats {
    /// Clears this kind's window totals while site identities and warn
    /// throttles persist.
    fn reset_window(&mut self) {
        self.count = 0;
        self.total = Duration::ZERO;
        self.histogram = Histogram::default();
        self.sites.clear_window();
        (self.overflow_count, self.overflow_total, self.overflow_max) = (0, Duration::ZERO, Duration::ZERO);
    }
    fn snapshot(&self) -> KindSummary {
        KindSummary {
            count: self.count, p50: self.histogram.percentile(self.count, PERCENTILES[0]),
            p99: self.histogram.percentile(self.count, PERCENTILES[1]), max: self.histogram.max, total: self.total,
            top: self.sites.top(), overflow_count: self.overflow_count,
            overflow_total: self.overflow_total, overflow_max: self.overflow_max,
        }
    }
}

/// Process-wide timing statistics. `Default` reads no clock: the first
/// finished operation opens the first window.
#[derive(Default)]
struct Stats { start: Option<Instant>, kinds: [KindStats; KIND_COUNT] }

fn state() -> &'static Mutex<Stats> {
    static STATS: OnceLock<Mutex<Stats>> = OnceLock::new();
    STATS.get_or_init(Default::default)
}

/// One closed window's per-kind snapshot, logged by the boundary sample.
struct Summary { window: Duration, kinds: [KindSummary; KIND_COUNT] }

impl Stats {
    /// Ends the open window when `now` is at least WINDOW after it opened;
    /// the next sample belongs to the new window.
    fn close_window(&mut self, now: Instant) -> Option<Summary> {
        let elapsed = now.saturating_duration_since(self.start?);
        if elapsed < WINDOW { return None; }
        let summary = Summary { window: elapsed, kinds: Kind::ALL.map(|kind| self.kinds[kind.index()].snapshot()) };
        for kind in &mut self.kinds { kind.reset_window(); }
        self.start = Some(now);
        Some(summary)
    }

    /// One finished operation; the first one opens the window. Untracked sites
    /// still count globally and warn through the kind's fallback throttle.
    fn sample(&mut self, now: Instant, kind: Kind, caller: &'static Location<'static>, elapsed: Duration) -> Option<Warn> {
        self.start.get_or_insert(now);
        let stats = &mut self.kinds[kind.index()];
        stats.count += 1;
        stats.total += elapsed;
        stats.histogram.record(elapsed);
        let (warn_after, overflow) = match stats.sites.slot(caller) {
            Some(site) => {
                site.count += 1;
                site.total += elapsed;
                site.max = site.max.max(elapsed);
                (&mut site.warn_after, false)
            }
            None => {
                stats.overflow_count += 1;
                stats.overflow_total += elapsed;
                stats.overflow_max = stats.overflow_max.max(elapsed);
                (&mut stats.overflow_warn, true)
            }
        };
        let due = warn_after.is_none_or(|next| now >= next);
        if elapsed > kind.warn_threshold() && due { *warn_after = Some(now + WARN_PERIOD); }
        (elapsed > kind.warn_threshold() && due).then_some(Warn { kind, caller, elapsed, overflow })
    }

    /// One finished operation with an injectable `now`; returns the window it
    /// closed and the warning its sample earned, both logged after the lock.
    fn record_at(&mut self, now: Instant, kind: Kind, caller: &'static Location<'static>, elapsed: Duration) -> (Option<Summary>, Option<Warn>) {
        (self.close_window(now), self.sample(now, kind, caller, elapsed))
    }
}

static STARTUP_COMPLETE: AtomicBool = AtomicBool::new(false);
pub(crate) fn startup_complete() { STARTUP_COMPLETE.store(true, Ordering::Relaxed); }
/// One slow-operation warning, logged after the stats lock is released: INFO with `phase = "startup"` until `startup_complete`, then WARN.
struct Warn { kind: Kind, caller: &'static Location<'static>, elapsed: Duration, overflow: bool }

impl Warn {
    fn log(&self) {
        if STARTUP_COMPLETE.load(Ordering::Relaxed) { tracing::warn!(kind = self.kind.name(), file = self.caller.file(), line = self.caller.line(), column = self.caller.column(), duration_ns = self.elapsed.as_nanos(), threshold_ns = self.kind.warn_threshold().as_nanos(), overflow = self.overflow, "database operation slow"); }
        else { tracing::info!(kind = self.kind.name(), file = self.caller.file(), line = self.caller.line(), column = self.caller.column(), duration_ns = self.elapsed.as_nanos(), threshold_ns = self.kind.warn_threshold().as_nanos(), overflow = self.overflow, phase = "startup", "database operation slow"); }
    }
}

impl Summary {
    fn log(&self) {
        tracing::info!(window_ns = self.window.as_nanos(), per_kind = %PerKind(self), "database timing summary");
    }
}

/// Borrowed single-line rendering of every kind's summary; no allocation.
struct PerKind<'a>(&'a Summary);

impl std::fmt::Display for PerKind<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (kind, s) in Kind::ALL.into_iter().zip(&self.0.kinds) {
            if kind.index() > 0 { f.write_str(" ")?; }
            write!(f, "{}={{count={},p50_ns={},p99_ns={},max_ns={},total_ns={},top_complete={},overflow={{count={},total_ns={},max_ns={}}},top=[", kind.name(), s.count, s.p50.as_nanos(), s.p99.as_nanos(), s.max.as_nanos(), s.total.as_nanos(), s.overflow_count == 0, s.overflow_count, s.overflow_total.as_nanos(), s.overflow_max.as_nanos())?;
            for (index, site) in s.top.iter().flatten().enumerate() {
                write!(f, "{}{}:{}:{},{},{},{}", if index == 0 { "" } else { "," }, site.location.file(), site.location.line(), site.location.column(), site.count, site.total.as_nanos(), site.max.as_nanos())?;
            }
            f.write_str("]}")?;
        }
        Ok(())
    }
}

/// Times `future` as one `kind` operation at the caller's source location,
/// captured synchronously. The clock starts on the first poll, so a future
/// dropped unpolled or cancelled records no sample.
#[track_caller]
pub(super) fn measure<T: Send, F: Future<Output = T> + Send>(kind: Kind, future: F) -> impl Future<Output = T> + Send {
    measure_at(kind, Location::caller(), future)
}

/// Times `future` under an explicit `caller`, for wrappers that forward the
/// operation's origin. Every completed outcome, value or error, is a sample.
pub(super) fn measure_at<T: Send, F: Future<Output = T> + Send>(kind: Kind, caller: &'static Location<'static>, future: F) -> impl Future<Output = T> + Send {
    async move {
        let started = Instant::now();
        let output = future.await;
        record(kind, caller, started.elapsed());
        output
    }
}

/// Records one finished operation of `kind` at `caller`. A closed window's
/// summary and a slow warning are logged after the stats lock releases.
pub(super) fn record(kind: Kind, caller: &'static Location<'static>, elapsed: Duration) {
    let (summary, warn) = crate::poison::lock_or_exit(state().lock())
        .record_at(Instant::now(), kind, caller, elapsed);
    if let Some(summary) = summary { summary.log(); }
    if let Some(warn) = warn { warn.log(); }
}

/// Records a finished transaction's COMMIT and whole-scope durations as one
/// sample each under a single lock acquisition and window rollover.
pub(super) fn record_commit_transaction(caller: &'static Location<'static>, commit: Duration, transaction: Duration) {
    let now = Instant::now();
    let (summary, warns) = {
        let mut stats = crate::poison::lock_or_exit(state().lock());
        (stats.close_window(now), [stats.sample(now, Kind::Commit, caller, commit), stats.sample(now, Kind::Transaction, caller, transaction)])
    };
    if let Some(summary) = summary { summary.log(); }
    warns.into_iter().flatten().for_each(|warn| warn.log());
}

#[cfg(all(test, feature = "tests-storage"))]
mod tests;
