//! Actor-owned physical Hub projection, development-index notices and recovery
//! readiness. Persisted snapshots are read through the sovereignty DAO before
//! publishing a system actor. Capture links and score evolution are owned by
//! the links engine; index points ride the same committed publication and are
//! projected in the actor from cached rows, never from actor-side SQL or Lua.
mod entity;
pub(crate) mod links;
pub(crate) mod indices;

use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
use crate::{dao::sovereignty::{capture, development, policy::{self, PolicyRow}, sources::SourceChange, structures, systems},
    services::sovereignty::{CommittedPublication, HubPublication, indices::model::{DevelopmentPublication, DevelopmentView, IndexCapability, IndexUnavailable}},
    space::{System, World, actor::Job}};
use anyhow::{ensure, Context};

pub(super) struct State {
    pub(super) version: Option<(i64, i64)>,
    pub(super) hub: Option<i64>,
    pub(super) alliance: Option<i64>,
    pub(super) corporation: Option<i64>,
    links: links::State,
    pub(super) policies: Vec<PolicyRow>,
    pub(super) readiness: Option<Arc<AtomicBool>>,
    /// Latest committed Hub/system snapshot used for index projections.
    pub(super) committed: Option<HubPublication>,
    /// Installed development-index capability clone.
    pub(super) capability: IndexCapability,
    /// Newest committed development row and its policy history.
    pub(super) development: Option<crate::services::sovereignty::indices::model::DevelopmentPublication>,
    /// Last projected development view announced to this system's pilots.
    pub(super) development_view: Option<DevelopmentView>,
}
impl Default for State {
    fn default() -> Self {
        Self { version: None, hub: None, alliance: None, corporation: None, links: Default::default(),
            policies: Vec::new(), readiness: None, committed: None,
            capability: IndexCapability::Unavailable(IndexUnavailable::NeverActivated),
            development: None, development_view: None }
    }
}

pub(crate) fn is_ready(world: &World) -> bool { world.sovereignty_ready.load(Ordering::Acquire) }
pub(crate) fn open(world: &World) { world.sovereignty_ready.store(true, Ordering::Release); }
pub(crate) fn close(world: &World) {
    world.sovereignty_ready.store(false, Ordering::Release);
    let handles: Vec<_> = crate::poison::lock_or_exit(world.systems.lock()).values().cloned().collect();
    for handle in handles { let _ = handle.send(Job::new(links::close)); }
}

pub(super) async fn cold(world: &World, id: i64) -> anyhow::Result<(Option<HubPublication>, Vec<PolicyRow>, indices::Cold)> {
    // The installed capability is cloned under the world slot only; cold never
    // resolves retained packages or runs Lua inside the market guard.
    let mut capability = indices::policy(world);
    let mut indices_development = None;
    if let IndexCapability::Ready(history) = &capability {
        match development::read(&world.db, id).await {
            Ok(row) => {
                indices_development = Some(DevelopmentPublication {
                    system_id: id, row, policies: history.clone(),
                });
            }
            Err(error) => {
                tracing::warn!(system = id, %error, "development state unavailable for cold projection");
                capability = IndexCapability::Invalid(format!("invalid development state for system {id}: {error:#}").into());
            }
        }
    }
    let cold = indices::Cold { capability, development: indices_development };
    let system = systems::read(&world.db, id).await?;
    let structure = structures::for_system(&world.db, id).await?;
    let target = capture::for_system(&world.db, id).await?;
    let Some(system) = system else {
        ensure!(structure.is_none() && target.is_none(), "orphan sovereignty Hub state");
        return Ok((None, Vec::new(), cold));
    };
    ensure!(system.system_id == id && system.ownership_epoch >= 0 && system.revision >= 0, "invalid sovereignty system version");
    let hub = if let Some(item) = system.hub_item_id {
        ensure!(is_ready(world), "sovereignty recovery is not ready");
        ensure!(structure.as_ref().is_some_and(|row| row.item_id == item), "missing sovereignty Hub structure");
        let mut rows = structures::snapshots(&world.db, Some(id), None).await?;
        ensure!(rows.len() == 1, "missing or duplicate sovereignty Hub item");
        Some(rows.remove(0))
    } else {
        ensure!(structure.is_none() && target.is_none() && system.claim_corporation_id.is_none() && system.alliance_id.is_none(), "orphan sovereignty Hub state");
        None
    };
    let policies = match system.alliance_id {
        Some(alliance) => vec![policy::read(&world.db, alliance).await?.context("missing alliance capital policy")?],
        None => Vec::new(),
    };
    if let Some(hub) = &hub {
        ensure!(hub.system == system, "inconsistent sovereignty system snapshot");
        entity::ball(&world.sde, hub, target.as_ref().context("missing Hub capture target")?, crate::space::now_ms())?;
    }
    Ok((Some(HubPublication { system, hub, capture: target }), policies, cold))
}

pub(super) fn initialize(system: &mut System, snapshot: Option<HubPublication>, policies: Vec<PolicyRow>, ready: Arc<AtomicBool>, indices: indices::Cold) -> anyhow::Result<()> {
    system.sovereignty.readiness = Some(ready);
    system.sovereignty.policies = policies;
    system.sovereignty.capability = indices.capability;
    system.sovereignty.development = indices.development;
    if let Some(snapshot) = snapshot { apply(system, snapshot, true, &[])?; }
    if system.sovereignty.development_view.is_none() {
        if let Ok(indices::Projection::Ready(view)) = indices::projected(system, crate::client::filetime_now()) {
            system.sovereignty.development_view = Some(view);
        }
    }
    Ok(())
}

fn apply(system: &mut System, snapshot: HubPublication, cold: bool, policies: &[PolicyRow]) -> anyhow::Result<()> {
    let version = (snapshot.system.ownership_epoch, snapshot.system.revision);
    if system.sovereignty.version.is_some_and(|old| version <= old) { return Ok(()); }
    ensure!(snapshot.system.system_id == system.id, "wrong sovereignty system");
    ensure!(version.0 >= 0 && version.1 >= 0, "invalid sovereignty system version");
    let ball = match (&snapshot.hub, &snapshot.capture) {
        (Some(hub), Some(target)) => {
            ensure!(hub.system == snapshot.system, "inconsistent sovereignty publication");
            Some(entity::ball(system.sde().context("missing system SDE")?, hub, target, system.now_ms())?)
        }
        (None, None) if snapshot.system.hub_item_id.is_none()
            && snapshot.system.claim_corporation_id.is_none() && snapshot.system.alliance_id.is_none() => None,
        _ => anyhow::bail!("incomplete sovereignty publication"),
    };
    let previous = system.sovereignty.hub;
    let next = ball.as_ref().map(|b| b.id);
    ensure!(previous == next || next.is_none_or(|id| !system.contains(id)), "Hub item collides with existing ball");
    if previous != next { if let Some(id) = previous { system.remove_ball(id); } }
    if let Some(ball) = ball {
        if previous == next {
            let id = ball.id;
            system.command(id, move |current, stamp, _| {
                current.owner_id = ball.owner_id; current.corp_id = ball.corp_id;
                current.alliance_id = ball.alliance_id; current.slim = ball.slim;
                vec![crate::space::event(stamp, "OnSlimItemChange", vec![marshal::Value::Int(id), current.slim_item()])]
            });
        } else if cold { system.insert(ball); } else { system.add_ship(ball); }
    }
    let alliance_changed = system.sovereignty.alliance != snapshot.system.alliance_id;
    let ownership_changed = alliance_changed || system.sovereignty.corporation != snapshot.system.claim_corporation_id;
    system.sovereignty.alliance = snapshot.system.alliance_id;
    system.sovereignty.corporation = snapshot.system.claim_corporation_id;
    let alliance = system.sovereignty.alliance;
    system.sovereignty.policies.retain(|policy| Some(policy.alliance_id) == alliance);
    for policy in policies.iter().filter(|policy| Some(policy.alliance_id) == alliance) {
        if system.sovereignty.policies.iter().any(|old| old.revision >= policy.revision) { continue; }
        system.sovereignty.policies.clear();
        system.sovereignty.policies.push(policy.clone());
    }
    system.sovereignty.committed = Some(snapshot.clone());
    let projection = indices::projected(system, crate::client::filetime_now());
    if let Ok(indices::Projection::Ready(view)) = &projection {
        system.sovereignty.development_view = Some(view.clone());
    }
    if !cold { notify(&system.registry, &snapshot, structure_rows(&snapshot, &system.sovereignty.policies, projection), ownership_changed, alliance_changed); }
    system.sovereignty.version = Some(version);
    system.sovereignty.hub = next;
    links::install(system, snapshot.capture);
    Ok(())
}

/// Encode the structure rows for one publication. A whole-unavailable
/// capability sends the real base fields without a multiplier; a pending
/// refresh or broken projection sends no fabricated rows.
fn structure_rows(snapshot: &HubPublication, policies: &[PolicyRow], projection: Result<indices::Projection, crate::services::CallError>) -> Option<Vec<marshal::Value>> {
    let hubs = snapshot.hub.as_ref().map(std::slice::from_ref).unwrap_or(&[]);
    let encoded = match projection {
        Ok(indices::Projection::Ready(view)) => crate::services::sovereignty::wire::structures(hubs, policies, Some(std::slice::from_ref(&view))),
        Ok(indices::Projection::Unavailable) => crate::services::sovereignty::wire::structures(hubs, policies, None),
        Ok(indices::Projection::Pending) => {
            tracing::debug!(system = snapshot.system.system_id, "development structure update awaits policy replay");
            return None;
        }
        Err(error) => {
            tracing::warn!(system = snapshot.system.system_id, ?error, "development projection failed");
            return None;
        }
    };
    match encoded {
        Ok(rows) => Some(rows),
        Err(error) => {
            tracing::warn!(system = snapshot.system.system_id, ?error, "sovereignty structure projection failed");
            None
        }
    }
}
fn notify(registry: &crate::space::Pilots, snapshot: &HubPublication, rows: Option<Vec<marshal::Value>>, ownership_changed: bool, alliance_changed: bool) {
    let id = snapshot.system.system_id;
    let claim = crate::services::sovereignty::wire::system_claim(&snapshot.system);
    for pilot in registry.online_pilots() {
        if let Some(rows) = &rows {
            registry.notify_pilot(pilot, "OnSolarSystemSovStructuresUpdated", vec![marshal::Value::Int(id), marshal::Value::List(rows.clone())]);
        }
        if ownership_changed {
            registry.notify_pilot(pilot, "ProcessSovStatusChanged", vec![marshal::Value::Int(id), claim.clone()]);
        }
        if alliance_changed {
            registry.notify_pilot(pilot, "OnSovereigntyChanged", vec![marshal::Value::Int(id), snapshot.system.alliance_id.map(marshal::Value::Int).unwrap_or(marshal::Value::None)]);
        }
    }
}

/// Verify the joined state around the separate Hub and index reads. A concurrent
/// ownership, capital or points revision suppresses only the structure update.
async fn broadcast_unloaded(world: Arc<World>, committed: HubPublication) {
    let id = committed.system.system_id;
    let before = development::observations(&world.db, &[id]).await;
    let latest = async {
        let system = systems::read(&world.db, id).await?;
        let hub = structures::snapshots(&world.db, Some(id), None).await?.into_iter().next();
        let capital = match system.as_ref().and_then(|system| system.alliance_id) {
            Some(alliance) => policy::read(&world.db, alliance).await?,
            None => None,
        };
        Ok::<_, crate::db::Error>((system.map(|system| HubPublication { system, hub, capture: None }), capital))
    }.await;
    let (snapshot, policies) = match latest {
        Ok((Some(snapshot), capital)) => (snapshot, capital.into_iter().collect::<Vec<_>>()),
        Ok((None, _)) => {
            tracing::warn!(system = id, "sovereignty broadcast lost its system row");
            (committed, Vec::new())
        }
        Err(error) => {
            tracing::warn!(system = id, %error, "sovereignty broadcast read failed");
            (committed, Vec::new())
        }
    };
    let hubs = snapshot.hub.as_ref().map(std::slice::from_ref).unwrap_or(&[]);
    let projected = crate::services::sovereignty::queries::development_views(&world.db, &world.sde, std::slice::from_ref(&id), crate::client::filetime_now()).await;
    let after = development::observations(&world.db, &[id]).await;
    let coherent = match (&before, &after) {
        (Ok(before), Ok(after)) if before.len() == 1 && before == after => {
            let observation = &before[0];
            observation.system.as_ref() == Some(&snapshot.system)
                && observation.capital.as_ref() == policies.first()
                && snapshot.hub.as_ref().is_none_or(|hub| hub.system == snapshot.system)
                && snapshot.system.hub_item_id == snapshot.hub.as_ref().map(|hub| hub.structure.item_id)
        }
        _ => false,
    };
    let rows = if !coherent {
        tracing::warn!(system = id, "sovereignty broadcast snapshot changed during projection");
        None
    } else {
        match projected.and_then(|development| crate::services::sovereignty::wire::structures(hubs, &policies, development.as_deref())) {
            Ok(rows) => Some(rows),
            Err(error) => {
                tracing::warn!(system = id, ?error, "sovereignty structure projection failed");
                None
            }
        }
    };
    notify(&world.pilots, &snapshot, rows, true, true);
}

pub(crate) fn publish(world: &World, publication: CommittedPublication) {
    if !publication.sources.is_empty() {
        let changes: Arc<[SourceChange]> = publication.sources.into();
        let handles: Vec<_> = crate::poison::lock_or_exit(world.systems.lock()).values().cloned().collect();
        for handle in handles {
            let changes = changes.clone();
            let _ = handle.send(Job::new(move |system| links::invalidate(system, &changes)));
        }
    }
    if publication.capitals.iter().any(|capital| capital.changed) {
        for pilot in world.online_pilots() {
            if world.online_session(pilot).is_some_and(|session| publication.capitals.iter()
                .any(|capital| capital.changed && session.alliance_id == Some(capital.policy.alliance_id))) {
                world.notify_pilot(pilot, "OnUpdateCapitalInfo", vec![]);
            }
        }
    }
    let policies: Arc<[PolicyRow]> = publication.capitals.into_iter().map(|capital| capital.policy).collect::<Vec<_>>().into();
    if !policies.is_empty() {
        let handles: Vec<_> = crate::poison::lock_or_exit(world.systems.lock()).values().cloned().collect();
        for handle in handles {
            let policies = policies.clone();
            let _ = handle.send(Job::new(move |system| {
                let alliance = system.sovereignty.alliance;
                for policy in policies.iter().filter(|policy| Some(policy.alliance_id) == alliance) {
                    if system.sovereignty.policies.iter().any(|old| old.revision >= policy.revision) { continue; }
                    system.sovereignty.policies.clear();
                    system.sovereignty.policies.push(policy.clone());
                }
                // A capital change moves the defense multiplier without touching points.
                indices::refresh(system);
            }));
        }
    }
    for snapshot in publication.hubs {
        if world.running(snapshot.system.system_id).is_none() {
            if let Some(world) = world.shared() {
                tokio::spawn(async move { broadcast_unloaded(world, snapshot).await });
            }
            continue;
        }
        let policies = policies.clone();
        world.post(snapshot.system.system_id, move |system| {
            if let Err(error) = apply(system, snapshot, false, &policies) { tracing::error!(%error, "invalid committed sovereignty publication"); }
        });
    }
    // Committed index facts never load an actor; they project inside the loop.
    for publication in publication.development {
        world.post(publication.system_id, move |system| indices::apply_development(system, publication));
    }
}

#[cfg(all(test, feature = "tests-space"))] mod tests;
