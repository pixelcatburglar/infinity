//! Actor-owned physical Hub projection and recovery readiness.
//! Persisted snapshots are read through the sovereignty DAO before publishing a system actor.
//! Capture links and score evolution are owned by the links engine.
mod entity;
pub(crate) mod links;

use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
use crate::{dao::sovereignty::{capture, policy::{self, PolicyRow}, sources::SourceChange, structures, systems},
    services::sovereignty::{CommittedPublication, HubPublication}, space::{System, World}};
use anyhow::{ensure, Context};

#[derive(Default)]
pub(super) struct State {
    pub(super) version: Option<(i64, i64)>,
    pub(super) hub: Option<i64>,
    pub(super) alliance: Option<i64>,
    pub(super) corporation: Option<i64>,
    links: links::State,
    pub(super) policies: Vec<PolicyRow>,
    pub(super) readiness: Option<Arc<AtomicBool>>,
}

pub(crate) fn is_ready(world: &World) -> bool { world.sovereignty_ready.load(Ordering::Acquire) }
pub(crate) fn open(world: &World) { world.sovereignty_ready.store(true, Ordering::Release); }
pub(crate) fn close(world: &World) {
    world.sovereignty_ready.store(false, Ordering::Release);
    let handles: Vec<_> = world.systems.lock().unwrap().values().cloned().collect();
    for handle in handles { let _ = handle.send(Box::new(links::close)); }
}

pub(super) async fn cold(world: &World, id: i64) -> anyhow::Result<(Option<HubPublication>, Vec<PolicyRow>)> {
    let system = systems::read(&world.db, id).await?;
    let structure = structures::for_system(&world.db, id).await?;
    let target = capture::for_system(&world.db, id).await?;
    let Some(system) = system else {
        ensure!(structure.is_none() && target.is_none(), "orphan sovereignty Hub state");
        return Ok((None, Vec::new()));
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
    Ok((Some(HubPublication { system, hub, capture: target }), policies))
}

pub(super) fn initialize(system: &mut System, snapshot: Option<HubPublication>, policies: Vec<PolicyRow>, ready: Arc<AtomicBool>) -> anyhow::Result<()> {
    system.sovereignty.readiness = Some(ready);
    system.sovereignty.policies = policies;
    if let Some(snapshot) = snapshot { apply(system, snapshot, true, &[])?; }
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
    if !cold { notify(&system.registry, &snapshot, &system.sovereignty.policies, ownership_changed, alliance_changed); }
    system.sovereignty.version = Some(version);
    system.sovereignty.hub = next;
    links::install(system, snapshot.capture);
    Ok(())
}
fn notify(registry: &crate::space::Pilots, snapshot: &HubPublication, policies: &[PolicyRow], ownership_changed: bool, alliance_changed: bool) {
    let id = snapshot.system.system_id;
    let rows = crate::services::sovereignty::wire::structures(snapshot.hub.as_ref().map(std::slice::from_ref).unwrap_or(&[]), policies);
    let claim = crate::services::sovereignty::wire::system_claim(&snapshot.system);
    for pilot in registry.online_pilots() {
        registry.notify_pilot(pilot, "OnSolarSystemSovStructuresUpdated", vec![marshal::Value::Int(id), marshal::Value::List(rows.clone())]);
        if ownership_changed {
            registry.notify_pilot(pilot, "ProcessSovStatusChanged", vec![marshal::Value::Int(id), claim.clone()]);
        }
        if alliance_changed {
            registry.notify_pilot(pilot, "OnSovereigntyChanged", vec![marshal::Value::Int(id), snapshot.system.alliance_id.map(marshal::Value::Int).unwrap_or(marshal::Value::None)]);
        }
    }
}


pub(crate) fn publish(world: &World, publication: CommittedPublication) {
    if !publication.sources.is_empty() {
        let changes: Arc<[SourceChange]> = publication.sources.into();
        let handles: Vec<_> = world.systems.lock().unwrap().values().cloned().collect();
        for handle in handles {
            let changes = changes.clone();
            let _ = handle.send(Box::new(move |system| links::invalidate(system, &changes)));
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
        let handles: Vec<_> = world.systems.lock().unwrap().values().cloned().collect();
        for handle in handles {
            let policies = policies.clone();
            let _ = handle.send(Box::new(move |system| {
                let alliance = system.sovereignty.alliance;
                for policy in policies.iter().filter(|policy| Some(policy.alliance_id) == alliance) {
                    if system.sovereignty.policies.iter().any(|old| old.revision >= policy.revision) { continue; }
                    system.sovereignty.policies.clear();
                    system.sovereignty.policies.push(policy.clone());
                }
            }));
        }
    }
    for snapshot in publication.hubs {
        if world.running(snapshot.system.system_id).is_none() {
            notify(&world.pilots, &snapshot, &policies, true, true);
            continue;
        }
        let policies = policies.clone();
        world.post(snapshot.system.system_id, move |system| {
            if let Err(error) = apply(system, snapshot, false, &policies) { tracing::error!(%error, "invalid committed sovereignty publication"); }
        });
    }
}

#[cfg(test)] mod tests;
