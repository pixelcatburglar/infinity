//! Guarded Hub claims, corporation transfers and durable self-destruction.
//! Client entry points are `menuFunctions.py` transfer and destruct actions;
//! ownership, captures and history use `game.sov_*` and physical `game.items`.
//! Owned-Hub enemy capture and ordinary capital changes are not supported.
use infinity_core::consts::OWNER_UNOWNED;
use crate::{
    dao::{items::sovereignty as items, sovereignty::{capture::{self, CaptureRow}, history::{self, HistoryRow}, structures::HubRow, systems::{self, SystemChange}}},
    db::executor::Executor,
    lua::content::PinnedPolicy,
    services::{db_err, CallError},
};
use super::HubPublication;
use std::sync::Arc;

pub(crate) struct Neutralized {
    pub publication: HubPublication,
    pub history: HistoryRow,
}

async fn fence_target(ex: &(impl Executor + ?Sized), target: &mut CaptureRow) -> Result<(), CallError> {
    if !capture::checkpoint(ex, target.revision, target).await.map_err(db_err)? {
        return Err(super::stale_error());
    }
    target.revision = target.revision.checked_add(1).ok_or_else(|| db_err(anyhow::anyhow!("capture revision overflow")))?;
    Ok(())
}

/// Resolve retained obligations before market or row locks; only current policy admits writes.
async fn retained_for_hub(db: &crate::db::Db, hub: &HubRow) -> Result<(String, String), CallError> {
    let target = capture::read(db, hub.structure.item_id).await.map_err(db_err)?
        .ok_or_else(|| db_err(anyhow::anyhow!("Hub capture target missing")))?;
    super::validate_retained(&[(&hub.structure.content_namespace, &hub.structure.content_version), (&target.content_namespace, &target.content_version)]).await?;
    Ok((target.content_namespace, target.content_version))
}


/// Caller holds organization, policy and system admission and must roll back on error.
pub(crate) async fn neutralize_locked(
    ex: &(impl Executor + ?Sized), before: &HubRow, target: &CaptureRow,
    neutral_owner: i64, policy: &PinnedPolicy, now: i64, actor: Option<i64>, operation_key: &str,
) -> Result<Neutralized, CallError> {
    let stale = super::stale_error;
    let system = &before.system;
    if system.hub_item_id != Some(before.structure.item_id)
        || system.claim_corporation_id != Some(before.owner_id) || system.alliance_id.is_none()
        || target.target_item_id != before.structure.item_id || target.system_id != system.system_id
        || target.ownership_epoch != system.ownership_epoch
        || before.structure.system_id != system.system_id
        || ![before.x, before.y, before.z].into_iter().all(|v| v.is_some_and(f64::is_finite)) {
        return Err(db_err(anyhow::anyhow!("invalid neutralization state")));
    }
    let epoch = system.ownership_epoch.checked_add(1).ok_or_else(|| db_err(anyhow::anyhow!("ownership epoch overflow")))?;
    let revision = target.revision.checked_add(1).ok_or_else(|| db_err(anyhow::anyhow!("capture revision overflow")))?;
    let mut history = HistoryRow {
        history_id: 0, system_id: system.system_id, hub_item_id: before.structure.item_id,
        old_corporation_id: system.claim_corporation_id, new_corporation_id: None,
        old_alliance_id: system.alliance_id, new_alliance_id: None,
        old_epoch: system.ownership_epoch, new_epoch: epoch, reason: super::TransitionKind::Neutralized.as_str().to_owned(),
        at: now, actor_id: actor, operation_key: operation_key.to_owned(),
        content_namespace: policy.namespace().to_owned(), content_version: policy.version().to_owned(),
    };
    history.history_id = history::insert(ex, &history).await.map_err(db_err)?.ok_or_else(|| db_err(anyhow::anyhow!("duplicate neutralization history")))?;
    // The CAS locks the checkpoint before deleting it, fencing in-flight actor checkpoints.
    if !capture::checkpoint(ex, target.revision, target).await.map_err(db_err)?
        || !capture::remove(ex, target.target_item_id).await.map_err(db_err)? {
        return Err(stale());
    }
    if let Some(due) = before.structure.self_destruct_due_at {
        if before.structure.self_destruct_actor.is_none()
            || !crate::dao::sovereignty::structures::set_destruct(ex, before.structure.item_id, Some(due), None, None).await.map_err(db_err)? {
            return Err(stale());
        }
        crate::scheduler::cancel(ex, &super::jobs::destruct_id(before.structure.item_id)).await.map_err(db_err)?;
    }
    if !items::transfer_owner(ex, before.structure.item_id, system.system_id, before.type_id, before.owner_id, neutral_owner).await.map_err(db_err)? {
        return Err(stale());
    }
    let after_system = systems::replace(ex, system, &SystemChange {
        hub_item_id: system.hub_item_id, claim_corporation_id: None, alliance_id: None,
        claim_time: None, ownership_epoch: epoch,
    }).await.map_err(db_err)?.ok_or_else(stale)?;
    let mut after_target = target.clone();
    after_target.ownership_epoch = epoch;
    after_target.revision = revision;
    after_target.controlling_team = None;
    after_target.scoring_team = None;
    after_target.defender_team = OWNER_UNOWNED;
    after_target.score = 0.0;
    after_target.rate = 0.0;
    after_target.blocked = false;
    after_target.checkpoint_wall_time = now;
    if !capture::insert(ex, &after_target).await.map_err(db_err)? { return Err(stale()); }
    let mut after = before.clone();
    after.system = after_system.clone();
    after.owner_id = neutral_owner;
    after.structure.self_destruct_due_at = None;
    after.structure.self_destruct_actor = None;
    Ok(Neutralized { publication: HubPublication { system: after_system, hub: Some(after), capture: Some(after_target) }, history })
}

/// A rejected observation returns the committed anchor for actor reconciliation.
pub(crate) enum CaptureOutcome {
    Claimed,
    Rejected { current: HubPublication, changes: Vec<crate::dao::sovereignty::sources::SourceChange> },
}

/// Own the operation independently of the caller that delivered the frozen endpoint.
pub(crate) async fn claim(resources: super::Resources, observation: super::CaptureObservation) -> Result<CaptureOutcome, CallError> {
    tokio::spawn(async move {
        let now = crate::client::filetime_now();
        loop {
            match claim_owned(resources.clone(), observation.clone(), now).await {
                Err(error) if super::is_stale(&error) => continue,
                result => return result,
            }
        }
    }).await
        .map_err(|error| db_err(anyhow::anyhow!("sovereignty settlement task failed: {error}")))?
}

pub(super) fn claimant_location_matches(location: &crate::dao::characters::ship::Location, contributor: &super::CaptureContributor, system: i64) -> bool {
    location.corporation == contributor.corporation_id && location.alliance == Some(contributor.alliance_id)
        && location.ship == Some(contributor.ship_id) && location.system == Some(system) && location.station.is_none()
}

pub(super) fn endpoint_contributor(observation: &super::CaptureObservation, winner: Option<i64>) -> Option<&super::CaptureContributor> {
    observation.claimant.as_ref().filter(|c| Some(c.alliance_id) == winner
        && observation.contributors.iter().any(|p| p.contributor == **c && p.through == observation.target.score_sim_time))
}


async fn claim_owned(resources: super::Resources, observation: super::CaptureObservation, now: i64) -> Result<CaptureOutcome, CallError> {
    use crate::{dao::{characters::ship, corporations::registry, sovereignty::{policy as policy_dao, structures}}, db::outbox::{dispatch_world, Outbox}, lua, services::market};
    use super::{notifications::{self, HubTransition}, policy, CapitalPublication, CommittedPublication, TransitionKind};

    let target = &observation.target;
    let winner = target.scoring_team.filter(|team| Some(*team) == target.controlling_team);
    let claimant = endpoint_contributor(&observation, winner);
    let valid_endpoint = target.score == 1.0 && target.rate.is_finite() && target.rate > 0.0 && !target.blocked
        && winner.is_some() && claimant.is_some() && !observation.contributors.is_empty();
    let expected_system = systems::read(&resources.db, target.system_id).await.map_err(db_err)?
        .ok_or_else(|| db_err(anyhow::anyhow!("capture system missing")))?;
    let prepared_hub = structures::snapshots(&resources.db, Some(target.system_id), None).await.map_err(db_err)?.into_iter().next();
    if let Some(hub) = &prepared_hub {
        let _ = retained_for_hub(&resources.db, hub).await?;
    }
    super::validate_retained(&[(&target.content_namespace, &target.content_version)]).await?;
    let alliance = winner.unwrap_or(OWNER_UNOWNED);
    let mut owned_systems = systems::for_alliance(&resources.db, alliance).await.map_err(db_err)?;
    let expected_policy = match policy_dao::read(&resources.db, alliance).await.map_err(db_err)? {
        Some(policy) => policy,
        None if owned_systems.is_empty() => policy_dao::PolicyRow {
            alliance_id: alliance, current_capital_system: None, last_capital_changed_at: None,
            capital_lost_at: None, capital_change_not_before: None, revision: 0,
        },
        None => return Err(db_err(anyhow::anyhow!("claimed Hub policy missing"))),
    };
    let original_owned = owned_systems.clone();
    let pin = lua::content_policy("sovereignty")?;
    let plan = if valid_endpoint {
        let mut proposed = expected_system.clone();
        proposed.alliance_id = winner;
        proposed.claim_corporation_id = claimant.map(|c| c.corporation_id);
        proposed.claim_time = Some(now);
        owned_systems.push(proposed);
        policy::prepare_initial(&pin, &expected_policy, &owned_systems, target.system_id, now)?
    } else { crate::lua::content::PolicyPlan::Unchanged };
    let _market = market::lock().await;
    if !crate::space::sovereignty::is_ready(&resources.world) { return Err(db_err(anyhow::anyhow!("sovereignty unavailable"))); }
    let admission = lua::admit_content(&pin)?;
    let neutral_owner = resources.sde.sovereignty.hub().deploy.default_owner;
    let world = resources.world.clone();
    let frozen = Arc::new((observation, expected_policy, expected_system, original_owned, plan, prepared_hub, admission));
    resources.db.transaction(move |tx, outbox: &mut Outbox| {
        let frozen = Arc::clone(&frozen);
        Box::pin(async move {
            let (observation, expected_policy, expected_system, original_owned, plan, prepared_hub, admission) = &*frozen;
            let claimant = endpoint_contributor(observation, winner);
            let mut alliances: Vec<_> = observation.contributors.iter().map(|p| p.contributor.alliance_id).collect();
            let mut corps: Vec<_> = observation.contributors.iter().map(|p| p.contributor.corporation_id).collect();
            let mut characters: Vec<_> = observation.contributors.iter().map(|p| p.contributor.character_id).collect();
            let current_orgs: std::collections::BTreeMap<_, _> = crate::dao::characters::roles::authorities(tx, &characters).await.map_err(db_err)?
                .into_iter().map(|a| (a.character, a.corporation)).collect();
            corps.extend(current_orgs.values().copied());
            alliances.sort_unstable(); alliances.dedup();
            corps.sort_unstable(); corps.dedup();
            characters.sort_unstable(); characters.dedup();
            let admitted = registry::lock_admission(tx, &alliances, &corps, &characters).await.map_err(db_err)?;
            let locked_orgs: std::collections::BTreeMap<_, _> = crate::dao::characters::roles::authorities(tx, &characters).await.map_err(db_err)?
                .into_iter().map(|a| (a.character, a.corporation)).collect();
            let current_policy = if valid_endpoint { Some(policy_dao::lock(tx, alliance).await.map_err(db_err)?) } else { None };
            let system = systems::lock(tx, observation.target.system_id).await.map_err(db_err)?;
            let hub = structures::snapshots(tx, Some(system.system_id), None).await.map_err(db_err)?.into_iter().next();
            let current_target = capture::lock_for_system(tx, system.system_id).await.map_err(db_err)?;
            if hub.as_ref().map(|h| (&h.structure.content_namespace, &h.structure.content_version))
                != prepared_hub.as_ref().map(|h| (&h.structure.content_namespace, &h.structure.content_version)) {
                return Err(super::stale_error());
            }
            let current = HubPublication { system: system.clone(), hub: hub.clone(), capture: current_target.clone() };
            let validation = super::authority::validate_contributors(tx, &observation.contributors, observation.target.score_sim_time).await?;
            let changes = validation.changes;
            let claimant_location = match claimant {
                Some(c) => ship::current(tx, c.character_id).await.map_err(db_err)?,
                None => None,
            };
            let claimant_current = match claimant {
                Some(c) => {
                    let proof = super::CaptureContribution { contributor: c.clone(), through: observation.target.score_sim_time };
                    let valid = super::authority::validate_contributors(tx, &[proof], observation.target.score_sim_time).await?.valid;
                    let versions = crate::dao::sovereignty::sources::versions(tx, &[c.character_id]).await.map_err(db_err)?;
                    valid && versions.first().is_some_and(|v| v.revision == c.source_revision)
                }
                None => false,
            };
            let claimant_valid = claimant_current && claimant.zip(claimant_location.as_ref())
                .is_some_and(|(c, location)| claimant_location_matches(location, c, system.system_id));
            let target = &observation.target;
            let anchor = current_target.as_ref();
            if !admitted || current_orgs != locked_orgs || !valid_endpoint || !validation.valid || !claimant_valid || system != *expected_system
                || system.revision != observation.system_revision || system.alliance_id.is_some()
                || system.claim_corporation_id.is_some() || system.ownership_epoch != target.ownership_epoch
                || system.hub_item_id != Some(target.target_item_id)
                || current_policy.as_ref() != Some(expected_policy)
                || systems::for_alliance(tx, alliance).await.map_err(db_err)? != *original_owned
                || !anchor.is_some_and(|a| a.revision == target.revision && a.ownership_epoch == target.ownership_epoch
                    && a.content_namespace == target.content_namespace && a.content_version == target.content_version
                    && a.capture_time_seconds == target.capture_time_seconds && a.defensive_regen_rate == target.defensive_regen_rate
                    && a.checkpoint_interval_ms == target.checkpoint_interval_ms)
                || hub.is_none() {
                return Ok(CaptureOutcome::Rejected { current, changes });
            }
            let hub = hub.ok_or_else(|| db_err(anyhow::anyhow!("missing capture Hub")))?;
            let claimant = claimant.ok_or_else(|| db_err(anyhow::anyhow!("missing winning contributor")))?;
            let claimant_location = claimant_location.as_ref().ok_or_else(|| db_err(anyhow::anyhow!("missing claimant location")))?;
            let claimant_corporation = claimant_location.corporation;
            let claimant_alliance = claimant_location.alliance;
            if hub.owner_id != neutral_owner {
                return Ok(CaptureOutcome::Rejected { current, changes });
            }
            let epoch = system.ownership_epoch.checked_add(1).ok_or_else(|| db_err(anyhow::anyhow!("ownership epoch overflow")))?;
            let mut history = HistoryRow { history_id: 0, system_id: system.system_id, hub_item_id: hub.structure.item_id,
                old_corporation_id: None, new_corporation_id: Some(claimant_corporation), old_alliance_id: None,
                new_alliance_id: claimant_alliance, old_epoch: system.ownership_epoch, new_epoch: epoch,
                reason: TransitionKind::Claimed.as_str().to_owned(), at: now, actor_id: Some(claimant.character_id),
                operation_key: format!("claim:{}:{}:{}", hub.structure.item_id, system.ownership_epoch, target.revision),
                content_namespace: admission.policy().namespace().to_owned(), content_version: admission.policy().version().to_owned() };
            history.history_id = history::insert(tx, &history).await.map_err(db_err)?
                .ok_or_else(|| db_err(anyhow::anyhow!("duplicate capture settlement")))?;
            let old_anchor = anchor.ok_or_else(|| db_err(anyhow::anyhow!("capture anchor missing")))?;
            if !capture::checkpoint(tx, old_anchor.revision, old_anchor).await.map_err(db_err)? { return Err(super::stale_error()); }
            if !capture::remove(tx, old_anchor.target_item_id).await.map_err(db_err)?
                || !items::transfer_owner(tx, hub.structure.item_id, system.system_id, hub.type_id, hub.owner_id, claimant_corporation).await.map_err(db_err)? {
                return Err(db_err(anyhow::anyhow!("capture settlement guard changed")));
            }
            let updated = systems::replace(tx, &system, &SystemChange { hub_item_id: system.hub_item_id,
                claim_corporation_id: Some(claimant_corporation), alliance_id: claimant_alliance, claim_time: Some(now), ownership_epoch: epoch,
            }).await.map_err(db_err)?.ok_or_else(|| db_err(anyhow::anyhow!("capture system changed")))?;
            let mut next = old_anchor.clone();
            next.ownership_epoch = epoch;
            next.revision = next.revision.checked_add(1).ok_or_else(|| db_err(anyhow::anyhow!("capture revision overflow")))?;
            next.controlling_team = None; next.scoring_team = None; next.defender_team = alliance;
            next.score = 0.0; next.rate = 0.0; next.blocked = false; next.score_sim_time = target.score_sim_time; next.checkpoint_wall_time = now;
            if !capture::insert(tx, &next).await.map_err(db_err)? { return Err(db_err(anyhow::anyhow!("capture target changed"))); }
            let capital = policy::apply(tx, expected_policy, plan, now).await?;
            let mut after = hub.clone(); after.system = updated.clone(); after.owner_id = claimant_corporation;
            notifications::transition(tx, outbox, &HubTransition { history, before: Some(hub), after: Some(after.clone()),
                capital: (capital != *expected_policy).then(|| (expected_policy.clone(), capital.clone())), destruct_deadline: None }).await?;
            outbox.sovereignty(CommittedPublication { hubs: vec![HubPublication { system: updated.clone(), hub: Some(after), capture: Some(next) }],
                capitals: vec![CapitalPublication::new(capital, expected_policy.current_capital_system)], ..Default::default() });
            Ok(CaptureOutcome::Claimed)
        })
    }, |value, outbox| async move { dispatch_world(value, outbox, &world) }).await
}

/// Transfer a claimed Hub between corporations in the same live alliance.
pub(crate) async fn transfer(resources: super::Resources, identity: crate::services::identity::Identity, item: i64, old_owner: i64, new_owner: i64) -> Result<(), CallError> {
    tokio::spawn(async move {
        let now = crate::client::filetime_now();
        loop {
            match transfer_owned(resources.clone(), identity, item, old_owner, new_owner, now).await {
                Err(error) if super::is_stale(&error) => continue,
                result => return result,
            }
        }
    }).await
        .map_err(|error| db_err(anyhow::anyhow!("Hub transfer task failed: {error}")))?
}

async fn transfer_owned(resources: super::Resources, identity: crate::services::identity::Identity, item: i64, old_owner: i64, new_owner: i64, now: i64) -> Result<(), CallError> {
    use infinity_core::consts::DRONE_SCOOP_RANGE;
    use crate::{dao::{characters::ship, corporations::registry, sovereignty::{policy as policy_dao, structures}}, db::outbox::{dispatch_world, Outbox}, lua, services::{market, notify_error}};
    use super::{authority, notifications::{self, HubTransition}, CapitalPublication, CommittedPublication, TransitionKind};
    let refused = || notify_error(infinity_core::messages::INSUFFICIENT_ROLE_FOR_OPERATION);
    let structure = structures::read(&resources.db, item).await.map_err(db_err)?.ok_or_else(refused)?;
    let hub = structures::snapshots(&resources.db, Some(structure.system_id), None).await.map_err(db_err)?.into_iter().next().ok_or_else(refused)?;
    let retained = retained_for_hub(&resources.db, &hub).await?;
    let alliance = hub.system.alliance_id.ok_or_else(refused)?;
    if hub.owner_id != old_owner || hub.system.claim_corporation_id != Some(old_owner) || old_owner == new_owner
        || ![hub.x, hub.y, hub.z].into_iter().all(|v| v.is_some_and(f64::is_finite)) { return Err(refused()); }
    let character = identity.char_id;
    let system_id = hub.system.system_id;
    let ship_id = resources.world.with(system_id, move |system| {
        let pilot = system.pilot_ball(character)?;
        let ship = system.sampled_ball(pilot.id)?;
        let hub = system.sampled_ball(item)?;
        let gap = crate::space::dist(ship.pos, hub.pos) - f64::from(ship.radius) - f64::from(hub.radius);
        (gap.is_finite() && gap <= DRONE_SCOOP_RANGE).then_some(ship.id)
    }).await.flatten().ok_or_else(refused)?;
    let pin = lua::content_policy("sovereignty")?;
    let _market = market::lock().await;
    if !crate::space::sovereignty::is_ready(&resources.world) { return Err(refused()); }
    let admission = lua::admit_content(&pin)?;
    let world = resources.world.clone();
    let frozen = Arc::new((hub, retained, admission));
    resources.db.transaction(move |tx, outbox: &mut Outbox| {
        let frozen = Arc::clone(&frozen);
        Box::pin(async move {
            let (hub, retained, admission) = &*frozen;
            if !registry::lock_admission(tx, &[alliance], &[old_owner, new_owner], &[character]).await.map_err(db_err)? { return Err(refused()); }
            authority::director(tx, character, old_owner).await?;
            let location = ship::current(tx, character).await.map_err(db_err)?.ok_or_else(refused)?;
            if location.corporation != old_owner || location.alliance != Some(alliance) || location.station.is_some() || location.system != Some(system_id) || location.ship != Some(ship_id) || identity.ship_id != Some(ship_id) {
                return Err(refused());
            }
            let target_corp = registry::corporation(tx, new_owner).await.map_err(db_err)?.ok_or_else(refused)?;
            if target_corp.deleted || target_corp.alliance_id != Some(alliance) { return Err(refused()); }
            let system = systems::lock(tx, system_id).await.map_err(db_err)?;
            let actual = structures::snapshots(tx, Some(system_id), None).await.map_err(db_err)?;
            if system != hub.system || actual.as_slice() != std::slice::from_ref(hub) { return Err(refused()); }
            let capital = policy_dao::read(tx, alliance).await.map_err(db_err)?
                .ok_or_else(|| db_err(anyhow::anyhow!("claimed Hub policy missing")))?;
            let mut target = capture::for_system(tx, system_id).await.map_err(db_err)?.ok_or_else(refused)?;
            if (&target.content_namespace, &target.content_version) != (&retained.0, &retained.1) { return Err(super::stale_error()); }
            fence_target(tx, &mut target).await?;
            let mut history = HistoryRow { history_id: 0, system_id, hub_item_id: item,
                old_corporation_id: Some(old_owner), new_corporation_id: Some(new_owner), old_alliance_id: Some(alliance), new_alliance_id: Some(alliance),
                old_epoch: system.ownership_epoch, new_epoch: system.ownership_epoch, reason: TransitionKind::Transferred.as_str().to_owned(),
                at: now, actor_id: Some(character), operation_key: format!("transfer:{item}:{}", system.revision),
                content_namespace: admission.policy().namespace().to_owned(), content_version: admission.policy().version().to_owned() };
            history.history_id = history::insert(tx, &history).await.map_err(db_err)?.ok_or_else(refused)?;
            let mut after = hub.clone();
            if let Some(due) = hub.structure.self_destruct_due_at {
                if hub.structure.self_destruct_actor.is_none() || !structures::set_destruct(tx, item, Some(due), None, None).await.map_err(db_err)? { return Err(refused()); }
                crate::scheduler::cancel(tx, &super::jobs::destruct_id(item)).await.map_err(db_err)?;
                after.structure.self_destruct_due_at = None;
                after.structure.self_destruct_actor = None;
            }
            if !items::transfer_owner(tx, item, system_id, hub.type_id, old_owner, new_owner).await.map_err(db_err)? { return Err(refused()); }
            let updated = systems::replace(tx, &system, &SystemChange { hub_item_id: system.hub_item_id, claim_corporation_id: Some(new_owner),
                alliance_id: system.alliance_id, claim_time: system.claim_time, ownership_epoch: system.ownership_epoch,
            }).await.map_err(db_err)?.ok_or_else(refused)?;
            after.system = updated.clone(); after.owner_id = new_owner;
            notifications::transition(tx, outbox, &HubTransition { history, before: Some(hub.clone()), after: Some(after.clone()), capital: None, destruct_deadline: None }).await?;
            let previous = capital.current_capital_system;
            outbox.sovereignty(CommittedPublication { hubs: vec![HubPublication { system: updated, hub: Some(after), capture: Some(target) }], capitals: vec![CapitalPublication::new(capital, previous)], ..Default::default() });
            Ok(())
        })
    }, |value, outbox| async move { dispatch_world(value, outbox, &world) }).await
}

/// Start a durable self-destruct deadline or cancel the current request.
pub(crate) async fn destruct(resources: super::Resources, identity: crate::services::identity::Identity, item: i64, request: bool) -> Result<(), CallError> {
    tokio::spawn(async move {
        let now = crate::client::filetime_now();
        loop {
            match destruct_owned(resources.clone(), identity, item, request, now).await {
                Err(error) if super::is_stale(&error) => continue,
                result => return result,
            }
        }
    }).await
        .map_err(|error| db_err(anyhow::anyhow!("Hub destruct task failed: {error}")))?
}

async fn destruct_owned(resources: super::Resources, identity: crate::services::identity::Identity, item: i64, request: bool, now: i64) -> Result<(), CallError> {
    use infinity_core::consts::DRONE_SCOOP_RANGE;
    use crate::{dao::{characters::ship, corporations::registry, sovereignty::{policy as policy_dao, structures}}, db::outbox::{dispatch_world, Outbox}, lua::{self, content::{PolicyInput, PolicyPlan, Rule}}, scheduler, services::{market, notify_error}};
    use super::{authority, jobs, notifications::{self, HubTransition}, CapitalPublication, CommittedPublication, TransitionKind};
    let refused = || notify_error(infinity_core::messages::INSUFFICIENT_ROLE_FOR_OPERATION);
    let structure = structures::read(&resources.db, item).await.map_err(db_err)?.ok_or_else(refused)?;
    let hub = structures::snapshots(&resources.db, Some(structure.system_id), None).await.map_err(db_err)?.into_iter().next().ok_or_else(refused)?;
    let retained = retained_for_hub(&resources.db, &hub).await?;
    let corporation = hub.system.claim_corporation_id.ok_or_else(refused)?;
    let alliance = hub.system.alliance_id.ok_or_else(refused)?;
    if hub.owner_id != corporation || ![hub.x, hub.y, hub.z].into_iter().all(|v| v.is_some_and(f64::is_finite)) { return Err(refused()); }
    let character = identity.char_id;
    let system_id = hub.system.system_id;
    let ship_id = resources.world.with(system_id, move |system| {
        let pilot = system.pilot_ball(character)?;
        let ship = system.sampled_ball(pilot.id)?;
        let hub = system.sampled_ball(item)?;
        let gap = crate::space::dist(ship.pos, hub.pos) - f64::from(ship.radius) - f64::from(hub.radius);
        (gap.is_finite() && gap <= DRONE_SCOOP_RANGE).then_some(ship.id)
    }).await.flatten().ok_or_else(refused)?;
    let pin = lua::content_policy("sovereignty")?;
    let due = if request {
        match pin.evaluate(Rule::SelfDestruct, &PolicyInput { now, system: system_id, current_system: None, first_appointment: None, last_loss: None, not_before: None, owned_count: 0 })? {
            PolicyPlan::SelfDestruct { due } => Some(due),
            _ => return Err(db_err(anyhow::anyhow!("invalid destruct policy result"))),
        }
    } else { None };
    let _market = market::lock().await;
    if !crate::space::sovereignty::is_ready(&resources.world) { return Err(refused()); }
    let admission = lua::admit_content(&pin)?;
    let world = resources.world.clone();
    let frozen = Arc::new((hub, retained, admission));
    resources.db.transaction(move |tx, outbox: &mut Outbox| {
        let frozen = Arc::clone(&frozen);
        Box::pin(async move {
            let (hub, retained, admission) = &*frozen;
            if !registry::lock_admission(tx, &[alliance], &[corporation], &[character]).await.map_err(db_err)? { return Err(refused()); }
            authority::director(tx, character, corporation).await?;
            let location = ship::current(tx, character).await.map_err(db_err)?.ok_or_else(refused)?;
            if location.corporation != corporation || location.alliance != Some(alliance) || location.station.is_some()
                || location.system != Some(system_id) || location.ship != Some(ship_id) || identity.ship_id != Some(ship_id) { return Err(refused()); }
            let system = systems::lock(tx, system_id).await.map_err(db_err)?;
            let actual = structures::snapshots(tx, Some(system_id), None).await.map_err(db_err)?;
            if system != hub.system || actual.as_slice() != std::slice::from_ref(hub) { return Err(refused()); }
            let previous = hub.structure.self_destruct_due_at;
            if request && previous.is_some() && hub.structure.self_destruct_actor == Some(character) { return Ok(()); }
            if request == previous.is_some() { return Err(refused()); }
            let capital = policy_dao::read(tx, alliance).await.map_err(db_err)?
                .ok_or_else(|| db_err(anyhow::anyhow!("claimed Hub policy missing")))?;
            let mut target = capture::for_system(tx, system_id).await.map_err(db_err)?.ok_or_else(refused)?;
            if (&target.content_namespace, &target.content_version) != (&retained.0, &retained.1) { return Err(super::stale_error()); }
            fence_target(tx, &mut target).await?;
            let updated = systems::replace(tx, &system, &SystemChange { hub_item_id: system.hub_item_id,
                claim_corporation_id: system.claim_corporation_id, alliance_id: system.alliance_id,
                claim_time: system.claim_time, ownership_epoch: system.ownership_epoch,
            }).await.map_err(db_err)?.ok_or_else(refused)?;
            if !structures::set_destruct(tx, item, previous, due.map(|_| character), due).await.map_err(db_err)? { return Err(refused()); }
            if let Some(deadline) = due {
                let payload = jobs::DestructPayload { item, system: system_id, ownership_epoch: updated.ownership_epoch, system_revision: updated.revision, due: deadline };
                let payload = serde_json::to_string(&payload).map_err(db_err)?;
                scheduler::schedule(tx, &scheduler::Job { id: jobs::destruct_id(item), kind: jobs::DESTRUCT_KIND.to_owned(), due_at: deadline, payload }).await.map_err(db_err)?;
            } else {
                scheduler::cancel(tx, &jobs::destruct_id(item)).await.map_err(db_err)?;
            }
            let mut after = hub.clone(); after.system = updated.clone(); after.structure.self_destruct_due_at = due; after.structure.self_destruct_actor = due.map(|_| character);
            let mut history = HistoryRow { history_id: 0, system_id, hub_item_id: item, old_corporation_id: Some(corporation), new_corporation_id: Some(corporation),
                old_alliance_id: Some(alliance), new_alliance_id: Some(alliance), old_epoch: system.ownership_epoch, new_epoch: system.ownership_epoch,
                reason: if request { TransitionKind::DestructRequested } else { TransitionKind::DestructCancelled }.as_str().to_owned(),
                at: now, actor_id: Some(character), operation_key: format!("destruct:{item}:{}", system.revision),
                content_namespace: admission.policy().namespace().to_owned(), content_version: admission.policy().version().to_owned() };
            history.history_id = history::insert(tx, &history).await.map_err(db_err)?.ok_or_else(refused)?;
            notifications::transition(tx, outbox, &HubTransition { history, before: Some(hub.clone()), after: Some(after.clone()), capital: None, destruct_deadline: due }).await?;
            let previous = capital.current_capital_system;
            outbox.sovereignty(CommittedPublication { hubs: vec![HubPublication { system: updated, hub: Some(after), capture: Some(target) }], capitals: vec![CapitalPublication::new(capital, previous)], ..Default::default() });
            Ok(())
        })
    }, |value, outbox| async move { let result = dispatch_world(value, outbox, &world); scheduler::wake(); result }).await
}

/// Finish only the exact still-live durable request; stale leased jobs are harmless.
pub(crate) async fn finish_destruct(resources: super::Resources, job: super::jobs::DestructPayload) -> Result<(), CallError> {
    let now = crate::client::filetime_now();
    loop {
        match finish_destruct_owned(resources.clone(), job.clone(), now).await {
            Err(error) if super::is_stale(&error) => continue,
            result => return result,
        }
    }
}

async fn finish_destruct_owned(resources: super::Resources, job: super::jobs::DestructPayload, now: i64) -> Result<(), CallError> {
    use crate::{dao::{corporations::registry, items::{fitting, loading::Item}, sovereignty::{policy as policy_dao, structures}}, db::outbox::{dispatch_world, Outbox}, lua, services::market};
    use super::{notifications::{self, HubTransition}, policy, CapitalPublication, CommittedPublication, TransitionKind};
    if now < job.due { return Err(db_err(anyhow::anyhow!("destruct deadline not reached"))); }
    let Some(hub) = structures::snapshots(&resources.db, Some(job.system), None).await.map_err(db_err)?.into_iter().next() else { return Ok(()); };
    if hub.structure.item_id != job.item || hub.system.ownership_epoch != job.ownership_epoch
        || hub.system.revision != job.system_revision || hub.structure.self_destruct_due_at != Some(job.due) { return Ok(()); }
    let retained = retained_for_hub(&resources.db, &hub).await?;
    let Some(alliance) = hub.system.alliance_id else { return Ok(()); };
    let Some(corporation) = hub.system.claim_corporation_id else { return Ok(()); };
    if hub.owner_id != corporation || ![hub.x, hub.y, hub.z].into_iter().all(|v| v.is_some_and(f64::is_finite)) {
        return Err(db_err(anyhow::anyhow!("invalid destruct item custody")));
    }
    let Some(actor) = hub.structure.self_destruct_actor else { return Err(db_err(anyhow::anyhow!("destruct actor missing"))); };
    let expected_policy = policy_dao::read(&resources.db, alliance).await.map_err(db_err)?
        .ok_or_else(|| db_err(anyhow::anyhow!("claimed Hub policy missing")))?;
    let owned = systems::for_alliance(&resources.db, alliance).await.map_err(db_err)?;
    let pin = lua::content_policy("sovereignty")?;
    let plan = policy::prepare_loss(&pin, &expected_policy, &owned, job.system, now)?;
    let _market = market::lock().await;
    if !crate::space::sovereignty::is_ready(&resources.world) { return Ok(()); }
    let admission = lua::admit_content(&pin)?;
    let frozen = Arc::new((hub, retained, expected_policy, owned, plan, admission, job));
    let world = resources.world.clone();
    resources.db.transaction(move |tx, outbox: &mut Outbox| {
        let frozen = Arc::clone(&frozen);
        Box::pin(async move {
            let (hub, retained, expected_policy, owned, plan, admission, job) = &*frozen;
            if !registry::lock_admission(tx, &[alliance], &[corporation], &[actor]).await.map_err(db_err)? { return Err(super::stale_error()); }
            let locked_policy = policy_dao::lock(tx, alliance).await.map_err(db_err)?;
            if locked_policy != *expected_policy || systems::for_alliance(tx, alliance).await.map_err(db_err)? != *owned {
                return Err(super::stale_error());
            }
            let system = systems::lock(tx, job.system).await.map_err(db_err)?;
            let actual = structures::snapshots(tx, Some(job.system), None).await.map_err(db_err)?;
            if system != hub.system || actual.as_slice() != std::slice::from_ref(hub)
                || system.revision != job.system_revision || system.ownership_epoch != job.ownership_epoch
                || hub.structure.self_destruct_due_at != Some(job.due) { return Ok(()); }
            let target = capture::for_system(tx, job.system).await.map_err(db_err)?
                .ok_or_else(|| db_err(anyhow::anyhow!("destruct capture target missing")))?;
            if (&target.content_namespace, &target.content_version) != (&retained.0, &retained.1) { return Err(super::stale_error()); }
            let epoch = system.ownership_epoch.checked_add(1).ok_or_else(|| db_err(anyhow::anyhow!("ownership epoch overflow")))?;
            let mut history = HistoryRow { history_id: 0, system_id: job.system, hub_item_id: job.item,
                old_corporation_id: Some(corporation), new_corporation_id: None, old_alliance_id: Some(alliance), new_alliance_id: None,
                old_epoch: system.ownership_epoch, new_epoch: epoch, reason: TransitionKind::DestructFinished.as_str().to_owned(),
                at: now, actor_id: Some(actor), operation_key: format!("destruct-finish:{}:{}", job.item, job.system_revision),
                content_namespace: admission.policy().namespace().to_owned(), content_version: admission.policy().version().to_owned() };
            history.history_id = history::insert(tx, &history).await.map_err(db_err)?
                .ok_or_else(|| db_err(anyhow::anyhow!("duplicate destruct history")))?;
            if !capture::checkpoint(tx, target.revision, &target).await.map_err(db_err)? { return Err(super::stale_error()); }
            if !capture::remove(tx, job.item).await.map_err(db_err)?
                || !structures::remove(tx, job.item).await.map_err(db_err)? {
                return Err(db_err(anyhow::anyhow!("destruct target changed")));
            }
            let item = Item { item_id: job.item, type_id: hub.type_id, owner_id: hub.owner_id, location_id: job.system,
                flag_id: 0, quantity: 1, singleton: true, group_id: 0, category_id: 0, custom_info: None };
            if !fitting::destroy_upgrade(tx, &item).await.map_err(db_err)? { return Err(db_err(anyhow::anyhow!("destruct item changed"))); }
            let updated = systems::replace(tx, &system, &SystemChange { hub_item_id: None, claim_corporation_id: None,
                alliance_id: None, claim_time: None, ownership_epoch: epoch,
            }).await.map_err(db_err)?.ok_or_else(|| db_err(anyhow::anyhow!("destruct system changed")))?;
            let capital = policy::apply(tx, expected_policy, plan, now).await?;
            notifications::transition(tx, outbox, &HubTransition { history, before: Some(hub.clone()), after: None,
                capital: (capital != *expected_policy).then(|| (expected_policy.clone(), capital.clone())), destruct_deadline: None }).await?;
            outbox.sovereignty(CommittedPublication { hubs: vec![HubPublication { system: updated, hub: None, capture: None }],
                capitals: vec![CapitalPublication::new(capital, expected_policy.current_capital_system)], ..Default::default() });
            Ok(())
        })
    }, |value, outbox| async move { dispatch_world(value, outbox, &world) }).await
}
