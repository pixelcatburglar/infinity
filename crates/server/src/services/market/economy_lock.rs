//! In-memory wait and hold diagnostics for the process-wide economy mutex.
//!
//! These counters are server-only diagnostics: they exist to investigate lock
//! contention in the running server and are never sent to a client, written to
//! the database, or exported to another process. Acquisitions close summary
//! windows; the runtime probe also checks the current holder without the gate.
//! The statistics keep caller source locations only, no player data.

use crate::histogram::Histogram;
use std::panic::Location;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;
use tokio::time::Instant;

/// Diagnostic policy: a summary window ends at the first successful
/// acquisition at least this long after the window started.
const WINDOW: Duration = Duration::from_secs(60);
/// Diagnostic policy: distinct call sites tracked exactly before overflow.
const SITES_CAPACITY: usize = 512;
/// Diagnostic policy: how many call sites a summary names.
const TOP_COUNT: usize = 5;
/// Diagnostic policy: percentiles reported, over a base of PERCENT.
const P50: u64 = 50;
const P99: u64 = 99;
/// Server diagnostic policy: initial hold limit and repeated report period.
const HOLDER_REPORT_PERIOD: Duration = Duration::from_secs(10);
struct Holder { caller: &'static Location<'static>, acquired: Instant, next_report: Instant }

/// One tracked call site. `location` is `None` only in never-occupied slots.
#[derive(Clone, Copy)]
struct Site {
    location: Option<&'static Location<'static>>,
    count: u64,
    hold: Duration,
}

/// Call-site table sorted by location with an occupied prefix. Identities are
/// kept across windows; only per-window counts and holds reset.
struct Sites {
    entries: [Site; SITES_CAPACITY],
    occupied: usize,
}

impl Default for Sites {
    fn default() -> Self {
        Sites {
            entries: [Site { location: None, count: 0, hold: Duration::ZERO }; SITES_CAPACITY],
            occupied: 0,
        }
    }
}

impl Sites {
    fn slot(&mut self, caller: &'static Location<'static>) -> Option<&mut Site> {
        let occupied = &self.entries[..self.occupied];
        let index = occupied
            .binary_search_by(|site| site.location.cmp(&Some(caller)))
            .unwrap_or_else(|index| index);
        if index < self.occupied && self.entries[index].location == Some(caller) {
            return Some(&mut self.entries[index]);
        }
        if self.occupied == SITES_CAPACITY {
            return None;
        }
        self.entries.copy_within(index..self.occupied, index + 1);
        self.entries[index] = Site { location: Some(caller), count: 0, hold: Duration::ZERO };
        self.occupied += 1;
        Some(&mut self.entries[index])
    }

    fn clear_window(&mut self) {
        for site in &mut self.entries[..self.occupied] {
            site.count = 0;
            site.hold = Duration::ZERO;
        }
    }

    /// Exact ranking of the sites with samples this window: descending total
    /// hold, ties by location.
    fn top(&self) -> [Option<TopSite>; TOP_COUNT] {
        let mut top: [Option<TopSite>; TOP_COUNT] = [None; TOP_COUNT];
        let mut len = 0;
        for site in &self.entries[..self.occupied] {
            if site.count == 0 {
                continue;
            }
            let candidate = TopSite { location: site.location.unwrap(), count: site.count, hold: site.hold };
            let mut index = len.min(TOP_COUNT - 1);
            if len == TOP_COUNT && !outranks(&candidate, &top[index].unwrap()) {
                continue;
            }
            while index > 0 && outranks(&candidate, &top[index - 1].unwrap()) {
                top[index] = top[index - 1];
                index -= 1;
            }
            top[index] = Some(candidate);
            len = (len + 1).min(TOP_COUNT);
        }
        top
    }
}

fn outranks(candidate: &TopSite, current: &TopSite) -> bool {
    (candidate.hold, std::cmp::Reverse(candidate.location))
        > (current.hold, std::cmp::Reverse(current.location))
}

/// Per-window statistics for the economy mutex. `Default` reads no clock and
/// leaves the first window unstarted; it opens on the first acquisition.
#[derive(Default)]
pub(super) struct Stats {
    start: Option<Instant>,
    count: u64,
    wait: Histogram,
    hold: Histogram,
    hold_total: Duration,
    sites: Sites,
    site_overflow_count: u64,
    site_overflow_hold: Duration,
    holder: Option<Holder>,
}

pub(super) fn state() -> &'static Mutex<Stats> {
    static STATS: OnceLock<Mutex<Stats>> = OnceLock::new();
    STATS.get_or_init(Default::default)
}

pub(crate) fn check_holder(now: Instant) {
    let overdue = crate::poison::lock_or_exit(state().lock()).overdue(now);
    if let Some((caller, held)) = overdue {
        tracing::error!(file = caller.file(), line = caller.line(), column = caller.column(),
            held_ns = held.as_nanos(), "economy lock held too long");
    }
}

impl Stats {
    fn overdue(&mut self, now: Instant) -> Option<(&'static Location<'static>, Duration)> {
        let holder = self.holder.as_mut()?;
        let held = now.saturating_duration_since(holder.acquired);
        if held <= HOLDER_REPORT_PERIOD || now < holder.next_report { return None; }
        holder.next_report = now + HOLDER_REPORT_PERIOD;
        Some((holder.caller, held))
    }

    /// Called by each successful acquisition while it holds the economy gate.
    /// Closes the current window and returns its summary when the window is at
    /// least WINDOW old and nonempty; an empty window never reports.
    fn on_acquire(&mut self, now: Instant) -> Option<Summary> {
        let Some(start) = self.start else {
            self.start = Some(now);
            return None;
        };
        let window = now - start;
        if window < WINDOW || self.count == 0 {
            return None;
        }
        let top_sites = if self.site_overflow_count == 0 {
            self.sites.top()
        } else {
            // A window with overflow has no exact global ranking.
            [None; TOP_COUNT]
        };
        let summary = Summary {
            count: self.count,
            window,
            wait_p50: self.wait.percentile(self.count, P50),
            wait_p99: self.wait.percentile(self.count, P99),
            wait_max: self.wait.max,
            hold_p50: self.hold.percentile(self.count, P50),
            hold_p99: self.hold.percentile(self.count, P99),
            hold_max: self.hold.max,
            hold_total: self.hold_total,
            top_sites_complete: self.site_overflow_count == 0,
            site_overflow_count: self.site_overflow_count,
            site_overflow_hold: self.site_overflow_hold,
            top_sites,
        };
        self.start = Some(now);
        self.count = 0;
        self.wait = Histogram::default();
        self.hold = Histogram::default();
        self.hold_total = Duration::ZERO;
        self.sites.clear_window();
        self.site_overflow_count = 0;
        self.site_overflow_hold = Duration::ZERO;
        Some(summary)
    }

    /// One completed operation, recorded by its guard's Drop while the raw
    /// economy guard is still held. Callers that overflow the site table still
    /// count toward every global statistic.
    fn record(&mut self, caller: &'static Location<'static>, wait: Duration, hold: Duration) {
        self.count += 1;
        self.wait.record(wait);
        self.hold.record(hold);
        self.hold_total += hold;
        match self.sites.slot(caller) {
            Some(site) => {
                site.count += 1;
                site.hold += hold;
            }
            None => {
                self.site_overflow_count += 1;
                self.site_overflow_hold += hold;
            }
        }
    }
}

#[derive(Clone, Copy)]
struct TopSite {
    location: &'static Location<'static>,
    count: u64,
    hold: Duration,
}

/// One closed window's summary; owned by the acquisition that closes it.
struct Summary {
    count: u64,
    window: Duration,
    wait_p50: Duration,
    wait_p99: Duration,
    wait_max: Duration,
    hold_p50: Duration,
    hold_p99: Duration,
    hold_max: Duration,
    hold_total: Duration,
    top_sites_complete: bool,
    site_overflow_count: u64,
    site_overflow_hold: Duration,
    top_sites: [Option<TopSite>; TOP_COUNT],
}

impl Summary {
    fn utilization(&self) -> f64 {
        self.hold_total.as_secs_f64() / self.window.as_secs_f64()
    }
    fn log(&self) {
        tracing::info!(
            count = self.count,
            window_ns = self.window.as_nanos(),
            wait_p50_ns = self.wait_p50.as_nanos(),
            wait_p99_ns = self.wait_p99.as_nanos(),
            wait_max_ns = self.wait_max.as_nanos(),
            hold_p50_ns = self.hold_p50.as_nanos(),
            hold_p99_ns = self.hold_p99.as_nanos(),
            hold_max_ns = self.hold_max.as_nanos(),
            hold_total_ns = self.hold_total.as_nanos(),
            utilization = self.utilization(),
            top_sites_complete = self.top_sites_complete,
            site_overflow_count = self.site_overflow_count,
            site_overflow_hold_ns = self.site_overflow_hold.as_nanos(),
            top_sites = %TopSites(&self.top_sites),
            "economy lock summary"
        );
    }
}

struct TopSites<'a>(&'a [Option<TopSite>; TOP_COUNT]);

impl std::fmt::Display for TopSites<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("[")?;
        let mut first = true;
        for site in self.0.iter().flatten() {
            if !first {
                f.write_str(",")?;
            }
            first = false;
            write!(f, "{}:{}:{},{},{}", site.location.file(), site.location.line(),
                site.location.column(), site.count, site.hold.as_nanos())?;
        }
        f.write_str("]")
    }
}

/// Owned wrapper around the original borrowed economy mutex guard. It carries
/// no lock API of its own; hold time ends when the wrapper is dropped, before
/// the raw guard unlocks the gate.
pub(crate) struct Guard {
    stats: &'static Mutex<Stats>,
    caller: &'static Location<'static>,
    acquired: Instant,
    wait: Duration,
    _gate: tokio::sync::MutexGuard<'static, ()>,
}

impl Drop for Guard {
    /// Records the completed operation while the raw guard is still held, so
    /// the window's samples are complete before the gate unlocks. Hold time is
    /// measured to Drop entry; the short accounting tail before the raw guard
    /// unlocks is not included.
    fn drop(&mut self) {
        let dropped = Instant::now();
        let hold = dropped - self.acquired;
        let mut stats = crate::poison::lock_or_exit(self.stats.lock());
        stats.holder = None;
        stats.record(self.caller, self.wait, hold);
    }
}

/// Acquires the economy gate for `caller`, recording queue wait and window
/// rotation. A window that closes here is summarized once on the log.
pub(super) async fn acquire(
    gate: &'static tokio::sync::Mutex<()>,
    stats: &'static Mutex<Stats>,
    caller: &'static Location<'static>,
) -> Guard {
    let wait_started = Instant::now();
    let gate = gate.lock().await;
    let acquired = Instant::now();
    let wait = acquired - wait_started;
    let summary = {
        let mut stats = crate::poison::lock_or_exit(stats.lock());
        let summary = stats.on_acquire(acquired);
        stats.holder = Some(Holder { caller, acquired, next_report: acquired + HOLDER_REPORT_PERIOD });
        summary
    };
    let guard = Guard { stats, caller, acquired, wait, _gate: gate };
    if let Some(summary) = summary {
        summary.log();
    }
    guard
}

#[cfg(all(test, feature = "tests-economy"))]
mod tests;
