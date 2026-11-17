//! Process-local ownership of live account sessions.
//!
//! A replacement cancels the previous registration and waits for its completion
//! before becoming active; takeover can instead be disabled to refuse duplicates.
//! This corresponds to the client's usurp handling in
//! `carbon/common/script/net/machoNet.py`. The registry contains only in-memory
//! account state and cannot coordinate sessions across server processes.
//!
//! A registration's stop reason is `Active`, `Replaced`, `CallerDropped` or
//! `ActorFailed`; the first non-`Active` reason wins. `Replaced` is the only
//! reason that emits the client's usurp close. `ActorFailed` is the old
//! session's ordinary close after its system actor failed.
//!
//! Keep the registration alive through all connection cleanup, including socket
//! drop and online-count decrement. Dropping a pending registration is safe: its
//! completion remains ordered behind its predecessor, even if its waiting future
//! or its owning task is cancelled. Dropping an active registration before its
//! cleanup has actually finished would incorrectly allow a replacement to proceed.
//! An active registration may expose its notification sender only for the
//! duration of its client transport; this process-local lookup is not a durable
//! delivery mechanism and does not reach other server processes.

use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, OnceLock};
use tokio::sync::watch;

struct Entry {
    registrations: VecDeque<Pending>,
}

struct Pending {
    id: u64,
    // This generation's stop channel; a caller guard may retain it after completion.
    cancelled: watch::Sender<StopReason>,
    // This generation's one completion channel. Pending keeps this sender;
    // the registration holds the other owner and records the real result.
    completion: watch::Sender<Completion>,
    dropped: bool,
    sender: Option<crate::space::NotifyTx>,
}

/// Why a session stops; the first non-`Active` reason wins.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StopReason { Active, Replaced, CallerDropped, ActorFailed }

/// Cleanup evidence on the registration's one completion channel.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Completion { Pending, Finished { leave_ok: bool, close_ok: bool } }

/// The exact old stop sender and the same completion channel.
#[derive(Clone)]
pub(crate) struct FailureTarget {
    stop: watch::Sender<StopReason>,
    completion: watch::Receiver<Completion>,
}

/// The cleanup owner closed the channel without recording a result.
#[derive(Debug)]
pub(crate) struct CleanupOwnerLost;

#[derive(Default)]
struct State {
    accounts: HashMap<i64, Entry>,
    next_id: u64,
}

fn state() -> &'static Mutex<State> {
    static STATE: OnceLock<Mutex<State>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(State::default()))
}

/// A session's ownership guard. Dropping it acknowledges completion only after
/// all earlier registrations for the account have completed.
pub struct Registration {
    account_id: i64,
    id: u64,
    predecessor: Option<watch::Receiver<Completion>>,
    cancelled: watch::Receiver<StopReason>,
    completion: watch::Sender<Completion>,
}

/// Writes `CallerDropped` when the owning caller drops it armed; `SessionRun`
/// disarms it once the session's own result has been observed.
pub(crate) struct CallerGuard {
    sender: Option<watch::Sender<StopReason>>,
}

/// Removes only the sender attached to this registration's generation.
pub(crate) struct SenderGuard {
    account_id: i64,
    id: u64,
}

/// Return the active account transport's sender, if one is attached.
pub(crate) fn sender(account_id: i64) -> Option<crate::space::NotifyTx> {
    let state = crate::poison::lock_or_exit(state().lock());
    let pending = state.accounts.get(&account_id)?.registrations.front()?;
    if pending.dropped { return None; }
    pending.sender.clone()
}

/// Atomically install a registration, or refuse an existing account without takeover.
pub fn register(account_id: i64, takeover: bool) -> Option<Registration> {
    let mut state = crate::poison::lock_or_exit(state().lock());
    if !takeover && state.accounts.contains_key(&account_id) {
        return None;
    }
    let (cancelled_tx, cancelled_rx) = watch::channel(StopReason::Active);
    let (completion_tx, _) = watch::channel(Completion::Pending);
    let id = state.next_id;
    state.next_id = id.checked_add(1).expect("live session registration IDs exhausted");
    let entry = state.accounts.entry(account_id).or_insert_with(|| Entry {
        registrations: VecDeque::new(),
    });
    let predecessor = entry.registrations.back().map(|previous| {
        request_stop(&previous.cancelled, StopReason::Replaced);
        previous.completion.subscribe()
    });
    entry.registrations.push_back(Pending {
        id,
        cancelled: cancelled_tx,
        completion: completion_tx.clone(),
        dropped: false,
        sender: None,
    });
    Some(Registration { account_id, id, predecessor, cancelled: cancelled_rx, completion: completion_tx })
}

/// Advance an `Active` reason to `to`; an earlier stop reason always wins.
fn request_stop(sender: &watch::Sender<StopReason>, to: StopReason) {
    sender.send_if_modified(|reason| {
        if *reason == StopReason::Active { *reason = to; true } else { false }
    });
}

impl Registration {
    /// Attach a transport only after this uncancelled generation becomes active.
    pub(crate) fn attach_sender(&self, sender: crate::space::NotifyTx) -> Option<SenderGuard> {
        let mut state = crate::poison::lock_or_exit(state().lock());
        let pending = state.accounts.get_mut(&self.account_id)?.registrations.front_mut()?;
        if self.predecessor.is_some() || pending.id != self.id || pending.dropped
            || *pending.cancelled.borrow() != StopReason::Active || pending.sender.is_some()
        {
            return None;
        }
        pending.sender = Some(sender);
        Some(SenderGuard { account_id: self.account_id, id: self.id })
    }

    /// Clone this registration's watch sender by exact ID, never by position.
    /// After completion, a retained guard owns only this old stop channel;
    /// dropping it cannot cancel a replacement registration.
    pub(crate) fn caller_guard(&self) -> CallerGuard {
        let state = crate::poison::lock_or_exit(state().lock());
        let pending = state.accounts.get(&self.account_id)
            .and_then(|entry| entry.registrations.iter().find(|item| item.id == self.id))
            .expect("live session registration missing");
        CallerGuard { sender: Some(pending.cancelled.clone()) }
    }

    /// The exact stop sender already cloned into `caller`, plus this
    /// registration's completion channel. No second registry lock.
    pub(crate) fn failure_target(&self, caller: &CallerGuard) -> FailureTarget {
        FailureTarget { stop: caller.sender.clone().expect("caller guard is armed"), completion: self.completion.subscribe() }
    }

    /// Record the real leave and close results on the same completion channel.
    pub(crate) fn record_cleanup(&self, leave_ok: bool, close_ok: bool) {
        self.completion.send_replace(Completion::Finished { leave_ok, close_ok });
    }

    /// Wait until the previous registration and its predecessors have completed.
    /// This future is cancellation-safe and may be awaited again if interrupted.
    /// Always wait before acting on cancellation or starting session work.
    pub async fn wait_for_predecessor(&mut self) {
        if let Some(predecessor) = &mut self.predecessor {
            // A Finished value only wakes the wait; every sender must drop.
            loop {
                if predecessor.changed().await.is_err() { break; }
            }
            self.predecessor = None;
        }
    }

    /// Wait for a stop reason. This future is cancellation-safe.
    pub async fn cancelled(&mut self) {
        while *self.cancelled.borrow_and_update() == StopReason::Active {
            if self.cancelled.changed().await.is_err() {
                break;
            }
        }
    }

    /// Whether a stop reason has been requested.
    pub fn is_cancelled(&self) -> bool {
        *self.cancelled.borrow() != StopReason::Active
    }

    /// Whether a newer registration took over: the only usurp-close reason.
    pub(crate) fn is_replaced(&self) -> bool {
        *self.cancelled.borrow() == StopReason::Replaced
    }
}

impl Drop for Registration {
    fn drop(&mut self) {
        let mut state = crate::poison::lock_or_exit(state().lock());
        let entry = state.accounts.get_mut(&self.account_id).expect("live session entry missing");
        let registration = entry.registrations.iter_mut().find(|item| item.id == self.id)
            .expect("live session registration missing");
        registration.dropped = true;
        while entry.registrations.front().is_some_and(|item| item.dropped) {
            entry.registrations.pop_front();
        }
        if entry.registrations.is_empty() {
            state.accounts.remove(&self.account_id);
        }
    }
}

impl FailureTarget {
    /// Mark this exact session `ActorFailed` unless an earlier reason won.
    pub(crate) fn fail_actor(&self) { request_stop(&self.stop, StopReason::ActorFailed); }

    /// Wait until every owner of the same completion channel has dropped.
    /// A recorded `Finished` only wakes this wait; a closed `Pending` is proof loss.
    pub(crate) async fn completed(&mut self) -> Result<(bool, bool), CleanupOwnerLost> {
        loop {
            if self.completion.changed().await.is_err() { break; }
        }
        match *self.completion.borrow() {
            Completion::Finished { leave_ok, close_ok } => Ok((leave_ok, close_ok)),
            Completion::Pending => Err(CleanupOwnerLost),
        }
    }
}

impl CallerGuard {
    /// Let go without a reason: the session's result was already observed.
    pub(crate) fn disarm(&mut self) {
        self.sender = None;
    }
}

impl Drop for CallerGuard {
    fn drop(&mut self) {
        if let Some(sender) = &self.sender {
            request_stop(sender, StopReason::CallerDropped);
        }
    }
}

impl Drop for SenderGuard {
    fn drop(&mut self) {
        let mut state = crate::poison::lock_or_exit(state().lock());
        if let Some(pending) = state.accounts.get_mut(&self.account_id)
            .and_then(|entry| entry.registrations.iter_mut().find(|item| item.id == self.id))
        {
            pending.sender = None;
        }
    }
}

#[cfg(all(test, feature = "tests-runtime"))]
mod tests;
