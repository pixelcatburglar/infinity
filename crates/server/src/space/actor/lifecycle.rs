//! In-memory checkpoint and optional explicit-stop control for a system actor.
//! This server-side bookkeeping mirrors no client module; the idle decision uses
//! actor state, SDE-backed scene checks and the existing snapshot context, but
//! performs no database work. It probes workers and checks the mailbox and
//! registry identity before stopping; an independent supervisor confirms exit.
//! The actor-local `Idle` window additionally drives one in-flight automatic
//! stop request at a time; nothing here forces a busy actor to stop.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::Instant;
use super::SystemHandle;

/// Server policy: after an automatic idle-unload attempt completes, the next
/// cold check runs at this fixed interval; the window itself is not restarted.
pub(super) const IDLE_UNLOAD_RETRY_INTERVAL: Duration = Duration::from_secs(60);

#[derive(Debug, PartialEq, Eq)]
pub(in crate::space) enum StopDecision { Busy(&'static str), Stopping }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::space) enum ExitOutcome { Clean, Failed }

pub(super) struct Request {
    pub(super) reply: oneshot::Sender<anyhow::Result<StopDecision>>,
    /// Automatic attempts carry their idle epoch; manual stops pass `None`.
    pub(super) idle_epoch: Option<u64>,
}

/// Actor-local idle observation on the shared tokio clock. `since` records the
/// start of the current uninterrupted pilot- and observer-free window; `epoch`
/// rotates on every occupied-to-free transition so equal timestamps cannot
/// alias an older automatic request; `retry_since` bounds automatic attempts
/// to one per fixed interval measured from the last completed attempt.
pub(super) struct Idle {
    timeout: Duration,
    since: Option<Instant>,
    epoch: u64,
    retry_since: Option<Instant>,
}

impl Idle {
    pub(super) fn new(timeout: Duration, occupied: bool, now: Instant) -> Self {
        Self { timeout, since: (!occupied).then_some(now), epoch: 0, retry_since: None }
    }
    /// An occupied observation clears the window; the transition back to free
    /// starts a new epoch, while continued idleness never refreshes it.
    pub(super) fn observe(&mut self, occupied: bool, now: Instant) {
        if occupied {
            self.since = None;
        } else if self.since.is_none() {
            self.epoch = self.epoch.checked_add(1).expect("idle epoch overflow");
            self.since = Some(now);
        }
    }
    /// Returns the current epoch once the full window elapsed and the fixed
    /// retry interval since the last attempt also elapsed; marks the attempt.
    pub(super) fn request_due(&mut self, now: Instant) -> Option<u64> {
        let due = self.since.is_some_and(|since| now.saturating_duration_since(since) >= self.timeout)
            && self.retry_since.is_none_or(|at| now.saturating_duration_since(at) >= IDLE_UNLOAD_RETRY_INTERVAL);
        if due { self.retry_since = Some(now); }
        due.then_some(self.epoch)
    }
    /// Record when an attempt completed; the idle window is not restarted.
    pub(super) fn completed(&mut self, at: Instant) { self.retry_since = Some(at); }
    pub(super) fn matches(&self, epoch: u64) -> bool {
        self.since.is_some() && self.epoch == epoch
    }
}
pub(super) struct Control {
    pub(super) stop: mpsc::UnboundedSender<Request>,
    pub(super) exited: watch::Receiver<Option<ExitOutcome>>,
    pub(super) stopping: AtomicBool,
}

#[derive(Default)]
pub(in crate::space) struct ActorState {
    pub(in crate::space) written: AtomicU64,
    pub(super) control: Option<Control>,
    failed: AtomicBool,
}

impl SystemHandle {
    pub(in crate::space) async fn request_idle_stop(&self, idle_epoch: Option<u64>) -> anyhow::Result<StopDecision> {
        let control = self.state.control.as_ref().ok_or_else(|| anyhow::anyhow!("idle stop disabled"))?;
        let (tx, rx) = oneshot::channel();
        control.stop.send(Request { reply: tx, idle_epoch }).map_err(|_| anyhow::anyhow!("space actor stop channel closed"))?;
        rx.await?
    }

    pub(in crate::space) async fn wait_exited(&self) -> anyhow::Result<()> {
        let control = self.state.control.as_ref().ok_or_else(|| anyhow::anyhow!("idle stop disabled"))?;
        let mut exited = control.exited.clone();
        loop {
            match *exited.borrow_and_update() {
                Some(ExitOutcome::Clean) => return Ok(()),
                Some(ExitOutcome::Failed) => anyhow::bail!("space actor exit failed"),
                None => {}
            }
            exited.changed().await.map_err(|_| anyhow::anyhow!("space actor exit notification closed"))?;
        }
    }

    pub(in crate::space) fn is_stopping(&self) -> bool {
        self.state.control.as_ref().is_some_and(|control| control.stopping.load(Ordering::Acquire))
    }
}

impl ActorState {
    pub(in crate::space) fn failed(&self) -> bool { self.failed.load(Ordering::Acquire) }
    pub(super) fn fail(&self) { self.failed.store(true, Ordering::Release); }
    pub(super) fn share(control: Option<Control>) -> Arc<Self> {
        Arc::new(Self { control, ..Self::default() })
    }
}

pub(super) async fn try_idle_stop(
    reply: oneshot::Sender<anyhow::Result<StopDecision>>,
    probes: &super::super::npc::worker::Probes,
    system: &mut super::super::System,
    receiver: &mut mpsc::UnboundedReceiver<super::Job>,
    systems: &std::sync::Weak<std::sync::Mutex<std::collections::HashMap<i64, SystemHandle>>>,
    gate: &std::sync::Weak<tokio::sync::Mutex<()>>,
    state: &ActorState,
    id: i64,
) -> bool {
    if reply.is_closed() { return false; }
    let reason = match probes.busy_reason().await {
        Ok(Some(reason)) => Some(reason),
        Ok(None) => system.registry.unload_blocked(id).or_else(|| system.unload_blocked()).or_else(|| {
            system.bounty_checkpoint_pending().then_some("npc_bounty_pending")
        }).or_else(|| {
            system.combat_facts.as_ref().filter(|facts| facts.is_dirty() || facts.batch_count() != 0)
                .map(|_| "combat_checkpoint_pending")
        }),
        Err(error) => { let _ = reply.send(Err(error)); return false; }
    };
    if let Some(reason) = reason {
        let _ = reply.send(Ok(StopDecision::Busy(reason)));
        return false;
    }
    if reply.is_closed() { return false; }
    let Some(systems) = systems.upgrade() else {
        let _ = reply.send(Err(anyhow::anyhow!("space registry closed")));
        return false;
    };
    let systems = crate::poison::lock_or_exit(systems.lock());
    let Some(current) = systems.get(&id).filter(|h| std::sync::Weak::ptr_eq(gate, &std::sync::Arc::downgrade(&h.gate))) else {
        let _ = reply.send(Err(anyhow::anyhow!("space actor identity changed")));
        return false;
    };
    if !receiver.is_empty() || current.tx.strong_count() != 2 {
        let _ = reply.send(Ok(StopDecision::Busy("mailbox_or_sender_busy")));
        return false;
    }
    if reply.send(Ok(StopDecision::Stopping)).is_err() { return false; }
    state.control.as_ref().unwrap().stopping.store(true, Ordering::Release);
    receiver.close();
    true
}
