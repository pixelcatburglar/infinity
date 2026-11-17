//! Retirement of one failed system actor after its admitted work finishes.
//! The damaged system is dropped before the waits, and the tombstone stays
//! until every exact old owner has proof. No automatic restart.

use std::sync::{Arc, Weak};

use super::super::{System, containment::{ContainmentFailure, DisconnectTarget, PilotBinding}};
use super::lifecycle::ActorState;
use super::SystemHandle;

/// One admitted pilot and the binding that names its exact session.
struct Membership {
    character: i64,
    binding: Arc<PilotBinding>,
}

/// Stop the exact pilots and wait for admitted work, then remove this tombstone.
pub(super) async fn retire(system: System, id: i64, state: Arc<ActorState>, workers: super::super::npc::worker::Tasks,
    systems: Weak<std::sync::Mutex<std::collections::HashMap<i64, SystemHandle>>>)
{
    let memberships = memberships(&system, id);
    let world = system.world.upgrade();
    let mut headless = tokio::task::JoinSet::new();
    for membership in &memberships {
        match membership.binding.target().cloned() {
            Some(DisconnectTarget::Tcp(target)) => target.fail_actor(),
            Some(DisconnectTarget::Headless(pilots)) => {
                let (pilots, world, sender, character) =
                    (pilots.upgrade(), world.clone(), membership.binding.sender().clone(), membership.character);
                match (pilots, world) {
                    (Some(pilots), Some(world)) => {
                        headless.spawn(async move { pilots.retire_generation(&world, character, &sender).await.map_err(|_| ()) });
                    }
                    _ => { headless.spawn(async { Err(()) }); }
                }
            }
            None => {}
        }
    }
    drop(system);
    let tcp = tcp_completions(&memberships);
    let headless = async move {
        while let Some(result) = headless.join_next().await {
            result.map_err(|_| ())??;
        }
        Ok::<(), ()>(())
    };
    let (workers_ok, tcp_ok, headless_ok) = tokio::join!(workers.join(), tcp, headless);
    let _market = crate::services::market::lock().await;
    let identity_ok = {
        let systems = systems.upgrade();
        systems.as_ref().is_some_and(|systems| {
            let systems = crate::poison::lock_or_exit(systems.lock());
            systems.get(&id).is_some_and(|handle| Arc::ptr_eq(&handle.state, &state) && handle.state.failed())
        })
    };
    let owners_ok = {
        let _bindings = world.as_ref().map(|world| world.bindings.bindings_guard());
        memberships.iter().all(|membership| PilotBinding::strong_count(&membership.binding) == 1)
    };
    let failure = if !workers_ok { Some(ContainmentFailure::WorkerOwnershipLost) }
        else if tcp_ok.is_err() { Some(ContainmentFailure::CleanupOwnerLost) }
        else if headless_ok.is_err() { Some(ContainmentFailure::HeadlessOwnerUnproven) }
        else if !identity_ok { Some(ContainmentFailure::RegistryIdentityChanged) }
        else if !owners_ok { Some(ContainmentFailure::PilotOwnerStillLive) }
        else { None };
    if let Some(failure) = failure {
        let character = memberships.first().map(|membership| membership.character);
        drop(_market);
        crate::poison::containment_lost(failure, id, character);
    }
    if let Some(systems) = systems.upgrade() {
        let mut systems = crate::poison::lock_or_exit(systems.lock());
        if systems.get(&id).is_some_and(|handle| Arc::ptr_eq(&handle.state, &state)) { systems.remove(&id); }
    }
    drop(_market);
    drop(memberships);
}

/// Every real pilot of the failed actor carries its exact binding; a missing,
/// mismatched or targetless binding is a lost proof, never a skipped pilot.
fn memberships(system: &System, id: i64) -> Vec<Membership> {
    system.pilot_bindings().map(|(character, binding)| {
        let Some(binding) = binding else {
            crate::poison::containment_lost(ContainmentFailure::MissingPilotTarget, id, Some(character));
        };
        if binding.character() != character || binding.target().is_none() {
            crate::poison::containment_lost(ContainmentFailure::MissingPilotTarget, id, Some(character));
        }
        Membership { character, binding }
    }).collect()
}

async fn tcp_completions(memberships: &[Membership]) -> Result<(), ()> {
    let mut waits = tokio::task::JoinSet::new();
    for membership in memberships {
        let Some(DisconnectTarget::Tcp(mut target)) = membership.binding.target().cloned() else { continue };
        waits.spawn(async move { target.completed().await.map(|_| ()).map_err(|_| ()) });
    }
    while let Some(result) = waits.join_next().await {
        if result.is_err() || result.ok().is_some_and(|proof| proof.is_err()) { return Err(()); }
    }
    Ok(())
}

/// Marks the old actor failed unless the child disarms it on a normal exit.
pub(super) struct ChildSystem {
    state: Arc<ActorState>,
    failed: bool,
}

impl ChildSystem {
    pub(super) fn new(state: Arc<ActorState>) -> Self { Self { state, failed: true } }
    pub(super) fn disarm(&mut self) { self.failed = false; }
}

impl Drop for ChildSystem {
    fn drop(&mut self) { if self.failed { self.state.fail(); } }
}
