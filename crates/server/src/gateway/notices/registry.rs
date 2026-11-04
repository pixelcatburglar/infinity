//! Generation-guarded ephemeral application stream registry.
use std::{collections::HashMap, sync::{Arc, Mutex}};
use bytes::Bytes;
use tokio::sync::{mpsc, oneshot};
use crate::{gateway::{notices::GatewayNotice, protocol::target_group::Target}, session::OnlineSession};

type Application = [u8; 16];
#[derive(Clone, Copy, PartialEq, Eq)]
struct Binding { user: i64, character: i64, session: i64 }
#[derive(Clone)]
pub(crate) struct NoticeHub(Arc<Mutex<State>>);
struct State { next: u64, applications: HashMap<Application, Entry> }
struct Entry { epoch: u64, leases: usize, binding: Option<Binding>, writer: Option<Writer> }
struct Writer { generation: u64, tx: mpsc::Sender<Bytes>, cancel: oneshot::Sender<()> }
pub(crate) struct Lease { hub: NoticeHub, application: Application, epoch: u64 }
pub(crate) struct Registration { lease: Lease, generation: u64, pub(crate) rx: mpsc::Receiver<Bytes>, pub(crate) cancelled: oneshot::Receiver<()> }
impl Default for NoticeHub { fn default() -> Self { Self::new() } }
impl NoticeHub {
    pub(crate) fn new() -> Self { Self(Arc::new(Mutex::new(State { next: 0, applications: HashMap::new() }))) }
    pub(in crate::gateway) fn lease(&self, application: Application) -> Lease {
        let mut state = self.0.lock().unwrap();
        if !state.applications.contains_key(&application) {
            state.next = state.next.wrapping_add(1);
            let epoch = state.next;
            state.applications.insert(application, Entry { epoch, leases: 0, binding: None, writer: None });
        }
        let entry = state.applications.get_mut(&application).unwrap();
        entry.leases += 1;
        Lease { hub: self.clone(), application, epoch: entry.epoch }
    }
    pub(crate) fn register(&self, application: Application, capacity: usize) -> Registration {
        let (tx, rx) = mpsc::channel(capacity);
        let (cancel, cancelled) = oneshot::channel();
        let mut state = self.0.lock().unwrap();
        state.next = state.next.wrapping_add(1);
        let generation = state.next;
        if !state.applications.contains_key(&application) {
            state.applications.insert(application, Entry { epoch: generation, leases: 0, binding: None, writer: None });
        }
        let entry = state.applications.get_mut(&application).unwrap();
        entry.leases += 1;
        if let Some(old) = entry.writer.take() { let _ = old.cancel.send(()); }
        entry.writer = Some(Writer { generation, tx, cancel });
        Registration { lease: Lease { hub: self.clone(), application, epoch: entry.epoch }, generation, rx, cancelled }
    }
    pub(crate) fn dispatch(&self, notice: GatewayNotice, resolver: impl Fn(i64) -> Option<OnlineSession>) {
        let mut frame: Option<Bytes> = None;
        let candidates: Vec<_> = {
            let state = self.0.lock().unwrap();
            state.applications.iter().filter_map(|(&application, entry)| entry.writer.as_ref().map(|writer| (application, entry.epoch, entry.binding, writer.generation, writer.tx.clone()))).collect()
        };
        for (application, epoch, binding, generation, tx) in candidates {
            let current = binding.and_then(|b| resolver(b.character).filter(|s| s.character_id == b.character && s.user_id == Some(b.user) && s.session_id == b.session));
            if binding.is_some() && current.is_none() {
                let mut state = self.0.lock().unwrap();
                if let Some(entry) = state.applications.get_mut(&application) {
                    if entry.epoch == epoch && entry.binding == binding { entry.binding = None; }
                }
            }
            if !matches_target(&notice.target, current.as_ref(), &application) { continue; }
            if tx.try_send(frame.get_or_insert_with(|| notice.frame()).clone()).is_err() { self.remove_writer(application, epoch, generation); }
        }
    }
    fn remove_writer(&self, application: Application, epoch: u64, generation: u64) {
        let mut state = self.0.lock().unwrap();
        if let Some(entry) = state.applications.get_mut(&application) {
            if entry.epoch == epoch && entry.writer.as_ref().is_some_and(|w| w.generation == generation) {
                if let Some(writer) = entry.writer.take() { let _ = writer.cancel.send(()); }
            }
        }
    }
}
impl Lease {
    pub(in crate::gateway) fn bind(&self, session: &OnlineSession, resolver: impl Fn(i64) -> Option<OnlineSession>) {
        let Some(user) = session.user_id else { return };
        let binding = Binding { user, character: session.character_id, session: session.session_id };
        let previous = {
            let state = self.hub.0.lock().unwrap();
            let Some(entry) = state.applications.get(&self.application).filter(|entry| entry.epoch == self.epoch) else { return };
            entry.binding
        };
        let is_current = |b: Binding| resolver(b.character).is_some_and(|s| s.character_id == b.character && s.user_id == Some(b.user) && s.session_id == b.session);
        if !is_current(binding) || previous.is_some_and(|old| old != binding && is_current(old)) { return; }
        let mut state = self.hub.0.lock().unwrap();
        if let Some(entry) = state.applications.get_mut(&self.application) {
            if entry.epoch == self.epoch && entry.binding == previous { entry.binding = Some(binding); }
        }
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        let mut state = self.hub.0.lock().unwrap();
        if let Some(entry) = state.applications.get_mut(&self.application) {
            if entry.epoch == self.epoch {
                entry.leases -= 1;
                if entry.leases == 0 { state.applications.remove(&self.application); }
            }
        }
    }
}
impl Drop for Registration {
    fn drop(&mut self) { self.lease.hub.remove_writer(self.lease.application, self.lease.epoch, self.generation); }
}
fn matches_target(target: &Target, session: Option<&OnlineSession>, application: &Application) -> bool {
    match target {
        Target::ApplicationInstanceUuid(id) => id.as_slice() == application,
        Target::SolarSystem(id) => session.and_then(|s| s.system_id) == Some(i64::from(*id)),
        Target::User(id) => session.and_then(|s| s.user_id) == Some(i64::from(*id)),
        Target::Character(id) => session.is_some_and(|s| s.character_id == i64::from(*id)),
        Target::Corporation(id) => session.and_then(|s| s.corporation_id) == Some(i64::from(*id)),
        Target::Alliance(id) => session.and_then(|s| s.alliance_id) == Some(i64::from(*id)),
    }
}
