//! Process-local ownership of live account sessions.
//!
//! A replacement cancels the previous registration and waits for its completion
//! before becoming active; takeover can instead be disabled to refuse duplicates.
//! This corresponds to the client's usurp handling in
//! `carbon/common/script/net/machoNet.py`. The registry contains only in-memory
//! account state and cannot coordinate sessions across server processes.
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
    cancelled: watch::Sender<bool>,
    // Closing this channel signals completion, in predecessor order.
    _completion: watch::Sender<()>,
    completion: watch::Receiver<()>,
    dropped: bool,
    sender: Option<crate::space::NotifyTx>,
}

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
    predecessor: Option<watch::Receiver<()>>,
    cancelled: watch::Receiver<bool>,
}

/// Removes only the sender attached to this registration's generation.
pub(crate) struct SenderGuard {
    account_id: i64,
    id: u64,
}

/// Return the active account transport's sender, if one is attached.
pub(crate) fn sender(account_id: i64) -> Option<crate::space::NotifyTx> {
    let state = state().lock().expect("live session registry poisoned");
    let pending = state.accounts.get(&account_id)?.registrations.front()?;
    if pending.dropped { return None; }
    pending.sender.clone()
}

/// Atomically install a registration, or refuse an existing account without takeover.
pub fn register(account_id: i64, takeover: bool) -> Option<Registration> {
    let mut state = state().lock().expect("live session registry poisoned");
    if !takeover && state.accounts.contains_key(&account_id) {
        return None;
    }
    let (cancelled_tx, cancelled_rx) = watch::channel(false);
    let (completion_tx, completion_rx) = watch::channel(());
    let id = state.next_id;
    state.next_id = id.checked_add(1).expect("live session registration IDs exhausted");
    let entry = state.accounts.entry(account_id).or_insert_with(|| Entry {
        registrations: VecDeque::new(),
    });
    let predecessor = entry.registrations.back().map(|previous| {
        previous.cancelled.send_replace(true);
        previous.completion.clone()
    });
    entry.registrations.push_back(Pending {
        id,
        cancelled: cancelled_tx,
        _completion: completion_tx,
        completion: completion_rx,
        dropped: false,
        sender: None,
    });
    Some(Registration { account_id, id, predecessor, cancelled: cancelled_rx })
}

impl Registration {
    /// Attach a transport only after this uncancelled generation becomes active.
    pub(crate) fn attach_sender(&self, sender: crate::space::NotifyTx) -> Option<SenderGuard> {
        let mut state = state().lock().expect("live session registry poisoned");
        let pending = state.accounts.get_mut(&self.account_id)?.registrations.front_mut()?;
        if self.predecessor.is_some() || pending.id != self.id || pending.dropped
            || *pending.cancelled.borrow() || pending.sender.is_some()
        {
            return None;
        }
        pending.sender = Some(sender);
        Some(SenderGuard { account_id: self.account_id, id: self.id })
    }

    /// Wait until the previous registration and its predecessors have completed.
    /// This future is cancellation-safe and may be awaited again if interrupted.
    /// Always wait before acting on cancellation or starting session work.
    pub async fn wait_for_predecessor(&mut self) {
        if let Some(predecessor) = &mut self.predecessor {
            // The sender is only dropped, never updated.
            let _ = predecessor.changed().await;
            self.predecessor = None;
        }
    }

    /// Wait for a replacement. This future is cancellation-safe.
    pub async fn cancelled(&mut self) {
        while !*self.cancelled.borrow_and_update() {
            if self.cancelled.changed().await.is_err() {
                break;
            }
        }
    }

    /// Whether a replacement has already registered.
    pub fn is_cancelled(&self) -> bool {
        *self.cancelled.borrow()
    }
}

impl Drop for Registration {
    fn drop(&mut self) {
        let mut state = state().lock().expect("live session registry poisoned");
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

impl Drop for SenderGuard {
    fn drop(&mut self) {
        let mut state = state().lock().expect("live session registry poisoned");
        if let Some(pending) = state.accounts.get_mut(&self.account_id)
            .and_then(|entry| entry.registrations.iter_mut().find(|item| item.id == self.id))
        {
            pending.sender = None;
        }
    }
}

#[cfg(test)]
mod tests;
