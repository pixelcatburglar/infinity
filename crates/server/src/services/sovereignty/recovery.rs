//! Startup and shutdown barriers for durable Hub obligations and capture anchors.
//! Retained authored packages are validated before taking market; all recovery
//! mutations and cancellation notices commit together before actor admission opens.
use std::{collections::BTreeMap, sync::Arc};
use crate::{client::filetime_now, dao::sovereignty::{capture, history::{self, HistoryRow}, policy, sources, structures, systems::{self, SystemChange}}, db::outbox::{dispatch_world, Outbox}, scheduler, services::{db_err, market, CallError}};
use super::{jobs, notifications::{self, HubTransition}, stale_error as stale, CapitalPublication, CommittedPublication, HubPublication, Resources, TransitionKind};

fn corrupt() -> CallError { db_err(anyhow::anyhow!("invalid sovereignty recovery state")) }

/// Close admission, drain the existing market fence, then commit recovery atomically.
/// Startup must call `open` only after this returns successfully.
pub(crate) async fn recover(resources: &Resources) -> Result<(), CallError> {
    crate::space::sovereignty::close(&resources.world);
    let now = filetime_now();
    loop {
        match recover_once(resources, now).await {
            Err(error) if super::is_stale(&error) => continue,
            result => return result,
        }
    }
}

async fn recover_once(resources: &Resources, now: i64) -> Result<(), CallError> {
    let hubs = structures::snapshots(&resources.db, None, None).await.map_err(db_err)?;
    let targets = capture::list(&resources.db).await.map_err(db_err)?;
    let mut packages: Vec<_> = hubs.into_iter()
        .map(|hub| (hub.structure.content_namespace, hub.structure.content_version))
        .chain(targets.into_iter().map(|target| (target.content_namespace, target.content_version)))
        .collect();
    packages.sort_unstable();
    packages.dedup();
    super::validate_retained(&packages.iter().map(|(namespace, version)| (namespace.as_str(), version.as_str())).collect::<Vec<_>>()).await?;
    let packages = Arc::new(packages);
    let _market = market::lock().await;
    let world = resources.world.clone();
    resources.db.transaction(|tx, outbox: &mut Outbox| {
        let packages = Arc::clone(&packages);
        Box::pin(async move {
            let contains = |namespace: &str, version: &str| packages.binary_search_by(|(n, v)| (n.as_str(), v.as_str()).cmp(&(namespace, version))).is_ok();
            let hubs = structures::snapshots(tx, None, None).await.map_err(db_err)?;
            let targets = capture::list(tx).await.map_err(db_err)?;
            if hubs.iter().any(|h| !contains(&h.structure.content_namespace, &h.structure.content_version))
                || targets.iter().any(|t| !contains(&t.content_namespace, &t.content_version)) { return Err(stale()); }
            if hubs.len() != targets.len() { return Err(corrupt()); }
            let ids = sources::all_character_ids(tx).await.map_err(db_err)?;
            let changes = sources::invalidate(tx, &ids, now).await.map_err(db_err)?;
            let mut publications = Vec::with_capacity(targets.len());
            let mut capitals = BTreeMap::new();
            for (target, hub) in targets.into_iter().zip(&hubs) {
                let expected = systems::lock(tx, target.system_id).await.map_err(db_err)?;
                let target = capture::read(tx, target.target_item_id).await.map_err(db_err)?.ok_or_else(corrupt)?;
                if !contains(&target.content_namespace, &target.content_version) { return Err(stale()); }
                if hub.structure.item_id != target.target_item_id || hub.system.system_id != target.system_id
                    || expected != hub.system || expected.hub_item_id != Some(target.target_item_id) || expected.ownership_epoch != target.ownership_epoch { return Err(corrupt()); }
                if let Some(alliance) = expected.alliance_id {
                    if !capitals.contains_key(&alliance) {
                        let row = policy::read(tx, alliance).await.map_err(db_err)?.ok_or_else(corrupt)?;
                        let previous = row.current_capital_system;
                        capitals.insert(alliance, CapitalPublication::new(row, previous));
                    }
                }
                let updated = systems::replace(tx, &expected, &SystemChange {
                    hub_item_id: expected.hub_item_id, claim_corporation_id: expected.claim_corporation_id,
                    alliance_id: expected.alliance_id, claim_time: expected.claim_time, ownership_epoch: expected.ownership_epoch,
                }).await.map_err(db_err)?.ok_or_else(stale)?;
                let mut after_hub = hub.clone();
                after_hub.system = updated.clone();
                if let Some(due) = hub.structure.self_destruct_due_at {
                    let actor = hub.structure.self_destruct_actor.ok_or_else(corrupt)?;
                    if !structures::set_destruct(tx, hub.structure.item_id, Some(due), None, None).await.map_err(db_err)? { return Err(stale()); }
                    scheduler::cancel(tx, &jobs::destruct_id(hub.structure.item_id)).await.map_err(db_err)?;
                    after_hub.structure.self_destruct_due_at = None;
                    after_hub.structure.self_destruct_actor = None;
                    let mut history = HistoryRow { history_id: 0, system_id: expected.system_id, hub_item_id: hub.structure.item_id,
                        old_corporation_id: expected.claim_corporation_id, new_corporation_id: expected.claim_corporation_id,
                        old_alliance_id: expected.alliance_id, new_alliance_id: expected.alliance_id,
                        old_epoch: expected.ownership_epoch, new_epoch: expected.ownership_epoch,
                        reason: TransitionKind::RecoveryCancelled.as_str().to_owned(), at: now, actor_id: Some(actor),
                        operation_key: format!("recovery-cancel:{}:{}", hub.structure.item_id, expected.revision),
                        content_namespace: hub.structure.content_namespace.clone(), content_version: hub.structure.content_version.clone() };
                    history.history_id = history::insert(tx, &history).await.map_err(db_err)?.ok_or_else(corrupt)?;
                    notifications::transition(tx, outbox, &HubTransition { history, before: Some(hub.clone()), after: Some(after_hub.clone()), capital: None, destruct_deadline: None }).await?;
                }
                let mut after_target = target.clone();
                after_target.scoring_team = None;
                after_target.rate = 0.0;
                after_target.blocked = false;
                after_target.score_sim_time = now;
                after_target.checkpoint_wall_time = now;
                if !capture::checkpoint(tx, target.revision, &after_target).await.map_err(db_err)? { return Err(stale()); }
                after_target.revision = after_target.revision.checked_add(1).ok_or_else(corrupt)?;
                publications.push(HubPublication { system: updated, hub: Some(after_hub), capture: Some(after_target) });
            }
            outbox.sovereignty(CommittedPublication { sources: changes, hubs: publications, capitals: capitals.into_values().collect() });
            Ok(())
        })
    }, |value, outbox| async move { dispatch_world(value, outbox, &world) }).await
}
