//! Guarded neutral Hub capture checkpoints against committed source generations.
//! Capture rows mirror the Entosis score observed by space; game sovereignty and
//! organization rows are the authority. Offline progression and ownership claims
//! are not performed here.
use std::{collections::{BTreeMap, BTreeSet}, sync::Arc};

use crate::{dao::{corporations::registry, sovereignty::{capture as dao, sources::SourceChange, structures, systems}},
    db::{executor::Executor, outbox::Outbox}, services::{db_err, CallError}};
use super::{authority, CaptureObservation, HubPublication, Resources};
use dao::CaptureRow;

#[derive(Debug)]
pub(crate) enum CheckpointOutcome {
    Accepted(CaptureRow),
    Rejected { current: HubPublication, changes: Vec<SourceChange> },
}

/// Persist one guarded batch. All dependencies are checked before any score is written.
pub(crate) async fn checkpoint(resources: Resources, observations: Vec<CaptureObservation>) -> Result<Vec<CheckpointOutcome>, CallError> {
    if observations.is_empty() { return Ok(Vec::new()); }
    if !crate::space::sovereignty::is_ready(&resources.world) {
        return Err(db_err(anyhow::anyhow!("sovereignty unavailable")));
    }
    let observations = Arc::new(observations);
    resources.db.transaction(|tx, _: &mut Outbox| {
        let observations = Arc::clone(&observations);
        Box::pin(async move { checkpoint_in(tx, &observations).await })
    }, |result, _| async move { Ok(result) }).await
}

pub(super) async fn checkpoint_in(ex: &(impl Executor + ?Sized), observations: &[CaptureObservation]) -> Result<Vec<CheckpointOutcome>, CallError> {
    let mut order: Vec<_> = (0..observations.len()).collect();
    order.sort_unstable_by_key(|&i| (observations[i].target.system_id, observations[i].target.target_item_id));
    if order.windows(2).any(|w| observations[w[0]].target.system_id == observations[w[1]].target.system_id) {
        return Err(db_err(anyhow::anyhow!("duplicate checkpoint system")));
    }
    let mut alliances = BTreeSet::new();
    let mut corps = BTreeSet::new();
    let mut characters = BTreeSet::new();
    for contributor in observations.iter().flat_map(|o| &o.contributors) {
        let contributor = &contributor.contributor;
        alliances.insert(contributor.alliance_id);
        corps.insert(contributor.corporation_id);
        characters.insert(contributor.character_id);
    }
    let alliances: Vec<_> = alliances.into_iter().collect();
    let characters: Vec<_> = characters.into_iter().collect();
    let current_orgs: BTreeMap<_, _> = crate::dao::characters::roles::authorities(ex, &characters).await.map_err(db_err)?
        .into_iter().map(|a| (a.character, a.corporation)).collect();
    corps.extend(current_orgs.values().copied());
    let corps: Vec<_> = corps.into_iter().collect();
    let admitted = registry::lock_admission(ex, &alliances, &corps, &characters).await.map_err(db_err)?;
    let locked_orgs: BTreeMap<_, _> = crate::dao::characters::roles::authorities(ex, &characters).await.map_err(db_err)?
        .into_iter().map(|a| (a.character, a.corporation)).collect();
    let mut systems = BTreeMap::new();
    for &i in &order {
        let id = observations[i].target.system_id;
        systems.insert(id, systems::lock(ex, id).await.map_err(db_err)?);
    }
    let mut current = BTreeMap::new();
    for &i in &order {
        let id = observations[i].target.system_id;
        let target = dao::lock_for_system(ex, id).await.map_err(db_err)?;
        let hub = structures::snapshots(ex, Some(id), None).await.map_err(db_err)?.into_iter().next();
        current.insert(id, HubPublication { system: systems[&id].clone(), hub, capture: target });
    }
    let mut changes = Vec::with_capacity(observations.len());
    let mut valid = admitted && current_orgs == locked_orgs;
    for observation in observations {
        let publication = &current[&observation.target.system_id];
        let target = &observation.target;
        let system = &publication.system;
        let anchor_valid = publication.capture.as_ref().is_some_and(|a| a.target_item_id == target.target_item_id
            && a.system_id == target.system_id && a.ownership_epoch == target.ownership_epoch && a.revision == target.revision
            && a.defender_team == target.defender_team && a.content_namespace == target.content_namespace
            && a.content_version == target.content_version && a.capture_time_seconds == target.capture_time_seconds
            && a.defensive_regen_rate == target.defensive_regen_rate && a.checkpoint_interval_ms == target.checkpoint_interval_ms);
        valid &= system.revision == observation.system_revision && system.ownership_epoch == target.ownership_epoch
            && system.hub_item_id == Some(target.target_item_id) && system.alliance_id.is_none()
            && system.claim_corporation_id.is_none() && publication.hub.as_ref().is_some_and(|hub|
                hub.structure.item_id == target.target_item_id && hub.structure.content_namespace == target.content_namespace
                && hub.structure.content_version == target.content_version) && anchor_valid;
        let validation = authority::validate_contributors(ex, &observation.contributors, target.score_sim_time).await?;
        valid &= validation.valid;
        changes.push(validation.changes);
    }
    if !valid {
        return Ok(observations.iter().zip(changes).map(|(o, changes)| CheckpointOutcome::Rejected {
            current: current[&o.target.system_id].clone(), changes,
        }).collect());
    }
    let mut accepted = BTreeMap::new();
    for &i in &order {
        let target = &observations[i].target;
        if !dao::checkpoint(ex, target.revision, target).await.map_err(db_err)? {
            return Err(super::stale_error());
        }
        let anchor = dao::read(ex, target.target_item_id).await.map_err(db_err)?
            .ok_or_else(|| db_err(anyhow::anyhow!("checkpoint anchor disappeared")))?;
        accepted.insert(target.system_id, anchor);
    }
    Ok(observations.iter().map(|o| CheckpointOutcome::Accepted(accepted[&o.target.system_id].clone())).collect())
}
