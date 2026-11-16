//! Per-listener connection admission: bounded simultaneous connections,
//! globally and per source address, plus per-source credential-failure history
//! that throttles repeated game logins. Each listener owns one `Admission`; a
//! game `Permit` lives only until its handshake completes, while the auxiliary
//! listeners hold theirs for the whole connection. Failure history is bounded
//! process-local memory; a table saturated by cooldowns fails open to the
//! ordinary credential check for new sources. No client protocol mirrors this.
//! Failure clocks are sampled under the state lock, so deque and eviction
//! timestamps follow the same serialized order as their mutations.

use anyhow::Result;
use std::collections::{HashMap, VecDeque};
use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::config::deployment::ServerConfig;

/// Server policy: distinct source addresses retained in the failure history.
const MAX_FAILURE_ADDRESSES: usize = 4096;

/// Monotonic `start + seconds`, or an error for an unrepresentable deadline.
pub(crate) fn deadline(start: Instant, seconds: u64) -> Result<Instant> {
    start.checked_add(Duration::from_secs(seconds)).ok_or_else(|| anyhow::anyhow!("deadline of {seconds}s is not representable"))
}

/// One source's failures inside the sliding window (at most `threshold - 1`
/// event times), its last failure and an optional cooldown start.
struct Failure {
    events: VecDeque<Instant>,
    last_failure: Instant,
    cooldown: Option<Instant>,
}

#[derive(Default)]
struct State {
    total: usize,
    active: HashMap<IpAddr, usize>,
    failures: HashMap<IpAddr, Failure>,
    warned: Option<Instant>,
}

/// One listener's admission gate.
pub(crate) struct Admission {
    maximum: usize,
    per_address: usize,
    state: Mutex<State>,
}

/// One held connection slot; dropping releases the global and per-address count.
pub(crate) struct Permit {
    admission: Arc<Admission>,
    address: IpAddr,
}

impl Admission {
    pub(crate) fn new(maximum: usize, per_address: usize) -> Arc<Admission> {
        Arc::new(Admission { maximum, per_address, state: Mutex::new(State::default()) })
    }

    /// Take one slot when the global and per-address counts both have room.
    pub(crate) fn try_acquire(self: &Arc<Self>, address: IpAddr) -> Option<Permit> {
        let mut state = crate::poison::lock_or_exit(self.state.lock());
        if state.total >= self.maximum || state.active.get(&address).copied().unwrap_or(0) >= self.per_address {
            return None;
        }
        state.total += 1;
        *state.active.entry(address).or_insert(0) += 1;
        Some(Permit { admission: self.clone(), address })
    }
}

/// Reset an expired cooldown and drop window-aged events; report whether the
/// record sits inside an active cooldown.
fn cooling(failure: &mut Failure, window: Duration, cooldown: Duration, now: Instant) -> bool {
    if failure.cooldown.is_some_and(|s| now.duration_since(s) >= cooldown) {
        failure.events.clear();
        failure.cooldown = None;
    }
    while failure.events.front().is_some_and(|t| now.duration_since(*t) >= window) {
        failure.events.pop_front();
    }
    failure.cooldown.is_some()
}

impl Permit {
    /// Whether this source is cooling down; sample time while holding the state lock.
    pub(crate) fn blocked(&self, policy: &ServerConfig) -> bool {
        self.blocked_with_clock(policy, Instant::now)
    }

    fn blocked_with_clock(&self, policy: &ServerConfig, clock: impl FnOnce() -> Instant) -> bool {
        let mut state = crate::poison::lock_or_exit(self.admission.state.lock());
        let now = clock();
        let Some(failure) = state.failures.get_mut(&self.address) else { return false };
        cooling(failure, Duration::from_secs(policy.login_failure_window_secs), Duration::from_secs(policy.login_failure_cooldown_secs), now)
    }

    /// Count one confirmed credential failure with a lock-serialized timestamp.
    pub(crate) fn record_failure(&self, policy: &ServerConfig) {
        self.record_failure_with_clock(policy, Instant::now)
    }

    fn record_failure_with_clock(&self, policy: &ServerConfig, clock: impl FnOnce() -> Instant) {
        let mut state = crate::poison::lock_or_exit(self.admission.state.lock());
        let now = clock();
        let (window, cooldown) = (Duration::from_secs(policy.login_failure_window_secs), Duration::from_secs(policy.login_failure_cooldown_secs));
        if !state.failures.contains_key(&self.address) {
            if state.failures.len() >= MAX_FAILURE_ADDRESSES {
                state.failures.retain(|_, f| !(f.cooldown.is_none_or(|s| now.duration_since(s) >= cooldown) && f.events.iter().all(|t| now.duration_since(*t) >= window)));
            }
            if state.failures.len() >= MAX_FAILURE_ADDRESSES {
                // Evict the oldest non-cooldown record, ties by address order.
                if let Some(victim) = state.failures.iter()
                    .filter(|(_, f)| f.cooldown.is_none_or(|s| now.duration_since(s) >= cooldown))
                    .min_by_key(|(address, f)| (f.last_failure, **address)).map(|(address, _)| *address)
                {
                    state.failures.remove(&victim);
                }
            }
            if state.failures.len() >= MAX_FAILURE_ADDRESSES {
                // Every retained source is cooling down: untracked sources keep
                // the plain credential check, warning at most once per window.
                if state.warned.is_none_or(|t| now.duration_since(t) >= window) {
                    state.warned = Some(now);
                    tracing::warn!("login failure history is all cooldowns; new sources stay unthrottled");
                }
                return;
            }
            state.failures.insert(self.address, Failure { events: VecDeque::with_capacity(policy.login_failure_threshold.saturating_sub(1)), last_failure: now, cooldown: None });
        }
        let failure = state.failures.get_mut(&self.address).expect("failure record present");
        if cooling(failure, window, cooldown, now) {
            return; // a late failure does not extend a running cooldown
        }
        failure.last_failure = now;
        if failure.events.len() + 1 >= policy.login_failure_threshold {
            // The threshold-th failure starts the cooldown instead of queueing.
            failure.events.clear();
            failure.cooldown = Some(now);
        } else {
            failure.events.push_back(now);
        }
    }
}

impl Drop for Permit {
    fn drop(&mut self) {
        let mut state = crate::poison::lock_or_exit(self.admission.state.lock());
        state.total -= 1;
        if let Some(count) = state.active.get_mut(&self.address) {
            *count -= 1;
            if *count == 0 {
                state.active.remove(&self.address);
            }
        }
    }
}

#[cfg(all(test, feature = "tests-runtime"))]
mod tests;
