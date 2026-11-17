//! In-memory admission tokens and pilot bindings for a retiring system actor.
//! A captured token refuses the actor it saw fail, even after that tombstone
//! is gone. Bindings name the exact session generation to stop. No tables.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};

use crate::services::CallError;

use super::{NotifyTx, World, actor::lifecycle::ActorState};

/// A captured actor is retiring; new admission into it is refused.
#[derive(Debug)]
pub(crate) struct ActorRetiring;

impl std::fmt::Display for ActorRetiring {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("space session is retiring")
    }
}

/// A closed proof is missing. Ordinary cleanup errors are not this failure.
#[derive(Debug)]
pub(crate) enum ContainmentFailure { MissingPilotTarget, CleanupOwnerLost, HeadlessOwnerUnproven, WorkerOwnershipLost, RegistryIdentityChanged, PilotOwnerStillLive, ExplicitStopProofContradiction }
impl std::fmt::Display for ContainmentFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { write!(f, "{self:?}") }
}
impl std::error::Error for ContainmentFailure {}
impl std::error::Error for ActorRetiring {}

/// The actor identity observed before an entry took the market lock.
#[derive(Clone)]
pub(crate) struct ActorAdmission {
    system: i64,
    state: Option<Arc<ActorState>>,
}

impl ActorAdmission {
    /// Refuse when the captured actor has failed. A later actor does not revive it.
    pub(crate) fn check(&self) -> Result<(), ActorRetiring> {
        if self.state.as_ref().is_some_and(|state| state.failed()) { Err(ActorRetiring) } else { Ok(()) }
    }

    pub(super) fn system(&self) -> i64 { self.system }
}
/// Where an exact pilot generation is stopped.
#[derive(Clone)]
pub(crate) enum DisconnectTarget {
    Tcp(crate::live_sessions::FailureTarget),
    Headless(Weak<crate::lua::host::pilots::Pilots>),
}

/// One live session's notification sender and its disconnect target.
pub(crate) struct PilotBinding {
    character: i64,
    sender: NotifyTx,
    target: Option<DisconnectTarget>,
    registry: Weak<Pilots>,
}

impl PilotBinding {
    pub(crate) fn sender(&self) -> &NotifyTx { &self.sender }
    pub(super) fn character(&self) -> i64 { self.character }
    pub(crate) fn target(&self) -> Option<&DisconnectTarget> { self.target.as_ref() }
    pub(super) fn strong_count(this: &Arc<Self>) -> usize { Arc::strong_count(this) }
}

impl Drop for PilotBinding {
    fn drop(&mut self) {
        let Some(registry) = self.registry.upgrade() else { return };
        let mut bindings = crate::poison::lock_or_exit(registry.bindings.lock());
        if bindings.get(&self.character).is_some_and(|weak| weak.as_ptr() == self) {
            bindings.remove(&self.character);
        }
    }
}

/// Weak pilot bindings. The live client or headless pilot owns the strong root.
pub(crate) struct Pilots {
    bindings: Mutex<HashMap<i64, Weak<PilotBinding>>>,
}

impl Default for Pilots {
    fn default() -> Self { Self { bindings: Mutex::new(HashMap::new()) } }
}

impl Pilots {
    pub(in crate::space) fn binding(&self, character: i64) -> Option<Arc<PilotBinding>> {
        let bindings = crate::poison::lock_or_exit(self.bindings.lock());
        let found = bindings.get(&character).and_then(Weak::upgrade);
        drop(bindings);
        found
    }

    /// Serialize the retirement barrier against binding inserts and releases
    /// while strong ownership is audited.
    pub(in crate::space) fn bindings_guard(&self) -> std::sync::MutexGuard<'_, HashMap<i64, Weak<PilotBinding>>> {
        crate::poison::lock_or_exit(self.bindings.lock())
    }
}
impl World {
    /// Capture the current actor, or `None` when the system is not loaded.
    /// A failed actor is refused immediately; a closed actor without an
    /// explicit stop in flight is treated the same without waiting for its
    /// supervisor to explain the closure.
    pub(crate) fn admission(&self, system: i64) -> Result<ActorAdmission, ActorRetiring> {
        let systems = crate::poison::lock_or_exit(self.systems.lock());
        let Some(handle) = systems.get(&system) else { return Ok(ActorAdmission { system, state: None }) };
        if handle.state.failed() || (handle.is_closed() && !handle.is_stopping()) {
            return Err(ActorRetiring);
        }
        Ok(ActorAdmission { system, state: Some(handle.state.clone()) })
    }
    pub(crate) fn bind_pilot(&self, character: i64, sender: NotifyTx, target: Option<DisconnectTarget>, current: Option<&Arc<PilotBinding>>)
        -> Result<Arc<PilotBinding>, CallError>
    {
        if let Some(current) = current.filter(|binding| binding.character == character && binding.sender.same_channel(&sender)) {
            return Ok(Arc::clone(current));
        }
        let mut bindings = crate::poison::lock_or_exit(self.bindings.bindings.lock());
        if bindings.get(&character).is_some_and(|existing| existing.strong_count() > 0) {
            drop(bindings);
            return Err(CallError::Internal(anyhow::anyhow!("pilot binding occupied")));
        }
        let binding = Arc::new(PilotBinding { character, sender, target, registry: Arc::downgrade(&self.bindings) });
        bindings.insert(character, Arc::downgrade(&binding));
        drop(bindings);
        Ok(binding)
    }
}
