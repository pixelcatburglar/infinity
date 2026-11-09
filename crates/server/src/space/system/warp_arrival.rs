//! Actor-owned once-only warp arrival callbacks, shared across fleet members.
//! The client receives ordinary destiny updates; callbacks and references are transient.

use super::*;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_ARRIVAL: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub(crate) struct WarpArrivalId(u64);

pub(super) struct ArrivalEntry {
    callback: Timer,
    references: usize,
}

pub(crate) struct WarpArrivalGuard {
    id: WarpArrivalId,
    sender: mpsc::WeakUnboundedSender<actor::Job>,
}

impl WarpArrivalGuard {
    pub(crate) fn id(&self) -> WarpArrivalId { self.id }
}

impl Drop for WarpArrivalGuard {
    fn drop(&mut self) {
        if let Some(sender) = self.sender.upgrade() {
            let id = self.id;
            let _ = sender.send(Box::new(move |system| system.release_warp_arrival(id)));
        }
    }
}

impl World {
    pub(crate) async fn begin_warp_arrival(&self, system: i64, callback: Timer) -> Option<WarpArrivalGuard> {
        let handle = self.running(system)?;
        let sender = handle.tx.downgrade();
        handle.with(move |actor| {
            let id = actor.register_warp_arrival(callback);
            WarpArrivalGuard { id, sender }
        }).await
    }
}

impl System {
    pub(crate) fn register_warp_arrival(&mut self, callback: Timer) -> WarpArrivalId {
        let id = WarpArrivalId(NEXT_ARRIVAL.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
            .expect("warp arrival identifier exhausted"));
        self.warp_arrivals.insert(id, ArrivalEntry { callback, references: 1 });
        id
    }

    pub(crate) fn retain_warp_arrival(&mut self, id: WarpArrivalId) -> bool {
        let Some(entry) = self.warp_arrivals.get_mut(&id) else { return false };
        entry.references = entry.references.checked_add(1).expect("warp arrival references exhausted");
        true
    }

    pub(crate) fn release_warp_arrival(&mut self, id: WarpArrivalId) {
        let Some(entry) = self.warp_arrivals.get_mut(&id) else { return };
        entry.references -= 1;
        if entry.references == 0 { self.warp_arrivals.remove(&id); }
    }

    pub(crate) fn complete_warp_arrival(&mut self, id: WarpArrivalId) {
        if let Some(entry) = self.warp_arrivals.remove(&id) { (entry.callback)(self); }
    }
}

#[cfg(test)] mod tests;
