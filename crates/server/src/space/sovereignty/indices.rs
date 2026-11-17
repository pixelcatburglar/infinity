//! The world's derived development-index capability and actor projections.
//! The slot caches a compiled `PolicyHistory` installed outside every market,
//! database and actor lock; it is never the policy authority. Cold loads only
//! clone the ready `Arc` and read the system's own committed row; no retained
//! resolve, reopen or Lua compile happens on the load path.
use crate::{client::filetime_now, dao::lua_content::activations::ActivationRow,
    services::{CallError, sovereignty::indices::{
        activation::PolicyHistory,
        model::{DevelopmentPublication, DevelopmentView, IndexCapability},
        rules,
    }},
    space::{System, World, actor::Job}};

/// Development-index inputs an actor receives on cold load.
pub(in crate::space) struct Cold {
    pub capability: IndexCapability,
    pub development: Option<DevelopmentPublication>,
}

/// A clone of the latest installed capability; never the authority.
pub(crate) fn policy(world: &World) -> IndexCapability {
    crate::poison::lock_or_exit(world.indices.lock()).clone()
}

/// Replace the capability and propagate it to every loaded actor under a short
/// lock. Installation never manufactures state: projections still need
/// committed facts, which arrive as ordinary development publications.
/// Startup installs before recovery; later installations run under the Lua
/// host's reload guard, which serializes successful and unavailable callbacks.
pub(crate) fn install_policy(world: &World, capability: IndexCapability) {
    {
        let mut installed = crate::poison::lock_or_exit(world.indices.lock());
        if let (IndexCapability::Ready(old), IndexCapability::Ready(next)) = (&*installed, &capability) {
            if head(next) < head(old) { return; }
        }
        *installed = capability.clone();
    }
    let handles: Vec<_> = crate::poison::lock_or_exit(world.systems.lock()).values().cloned().collect();
    for handle in handles {
        let capability = capability.clone();
        let _ = handle.send(Job::new(move |system| {
            if let (IndexCapability::Ready(old), IndexCapability::Ready(next)) = (&system.sovereignty.capability, &capability) {
                if head(next) < head(old) { return; }
            }
            if let (Some(committed), IndexCapability::Ready(next)) = (&system.sovereignty.development, &capability) {
                if head(next) < head(&committed.policies) { return; }
            }
            system.sovereignty.capability = capability;
            if let Ok(Projection::Ready(view)) = projected(system, filetime_now()) {
                system.sovereignty.development_view = Some(view);
            }
        }));
    }
}

/// What the actor can project: a real view, a whole-capability omission, or a
/// wait for the maintenance worker to reproject committed facts against a
/// backdated activation.
pub(super) enum Projection {
    Ready(DevelopmentView),
    Unavailable,
    Pending,
}

fn head(history: &PolicyHistory) -> i64 {
    history.identities.last().map(|activation: &ActivationRow| activation.ordinal).unwrap_or(0)
}

/// Project the actor's cached committed facts at `now`. A newer policy segment
/// whose start falls at or before the stored anchor is never guessed at: the
/// worker replays those facts and publishes a fresh row instead.
pub(super) fn projected(system: &System, now: i64) -> Result<Projection, CallError> {
    let development = system.sovereignty.development.as_ref();
    let history = match (&system.sovereignty.capability, development) {
        (IndexCapability::Unavailable(_), _) => return Ok(Projection::Unavailable),
        (IndexCapability::Invalid(diagnostic), _) => {
            return Err(CallError::Internal(anyhow::anyhow!("development indices invalid: {diagnostic}")));
        }
        (IndexCapability::Ready(installed), Some(committed)) if head(&committed.policies) > head(installed) => &committed.policies,
        (IndexCapability::Ready(installed), _) => installed,
    };
    if let Some(row) = development.and_then(|committed| committed.row.as_ref()) {
        if history.segments.iter().any(|segment| segment.ordinal > row.activation_ordinal && segment.starts_at <= row.evaluated_at) {
            return Ok(Projection::Pending);
        }
    }
    let committed = system.sovereignty.committed.as_ref().map(|publication| &publication.system);
    let capital = system.sovereignty.policies.first();
    Ok(Projection::Ready(rules::project(development.and_then(|committed| committed.row.as_ref()), committed, capital, history, system.id, now)?))
}

/// Accept a committed development publication; an older policy head or row
/// revision never displaces a newer cached one.
pub(super) fn apply_development(system: &mut System, publication: DevelopmentPublication) {
    let stale = system.sovereignty.development.as_ref().is_some_and(|old| {
        let (old_head, new_head) = (head(&old.policies), head(&publication.policies));
        new_head < old_head || (new_head == old_head
            && publication.row.as_ref().map(|row| row.revision).unwrap_or(-1) <= old.row.as_ref().map(|row| row.revision).unwrap_or(-1))
    });
    if stale { return; }
    system.sovereignty.development = Some(publication);
    refresh(system);
}

/// Reproject and tell this system's pilots what actually changed: one index
/// invalidation for trend or level movement, and one structure update
/// when the level set or defense multiplier moved. A missing baseline, an
/// unavailable capability or a pending refresh sends nothing.
pub(super) fn refresh(system: &mut System) {
    let view = match projected(system, filetime_now()) {
        Ok(Projection::Ready(view)) => view,
        Ok(Projection::Pending) => {
            tracing::debug!(system = system.id, "development projection awaits policy replay");
            return;
        }
        Ok(Projection::Unavailable) => return,
        Err(error) => {
            tracing::warn!(system = system.id, ?error, "development projection failed");
            return;
        }
    };
    let Some(old) = system.sovereignty.development_view.replace(view.clone()) else { return };
    let indices_changed = old.military_increasing != view.military_increasing || old.industrial_increasing != view.industrial_increasing
        || old.strategic_increasing != view.strategic_increasing
        || old.military.level != view.military.level || old.industrial.level != view.industrial.level
        || old.strategic.level != view.strategic.level;
    if indices_changed {
        for pilot in system.pilots().filter_map(|ball| ball.char_id) {
            system.registry.notify_pilot(pilot, "OnSolarSystemDevIndexChanged", vec![marshal::Value::Int(system.id)]);
        }
    }
    if old.military.level == view.military.level && old.industrial.level == view.industrial.level
        && old.strategic.level == view.strategic.level && old.defense_multiplier == view.defense_multiplier { return; }
    let Some(hub) = system.sovereignty.committed.as_ref().and_then(|publication| publication.hub.as_ref()) else { return };
    match crate::services::sovereignty::wire::structures(std::slice::from_ref(hub), &system.sovereignty.policies, Some(std::slice::from_ref(&view))) {
        Ok(rows) => for pilot in system.pilots().filter_map(|ball| ball.char_id) {
            system.registry.notify_pilot(pilot, "OnSolarSystemSovStructuresUpdated", vec![marshal::Value::Int(system.id), marshal::Value::List(rows.clone())]);
        },
        Err(error) => tracing::warn!(system = system.id, ?error, "development structure projection failed"),
    }
}
