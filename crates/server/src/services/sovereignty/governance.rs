//! Corporation alliance changes and atomic Hub neutralization.
//! The complete corporation-owned set is prepared before locks and rechecked in
//! the membership transaction. The market guard fences physical owner writes.
use crate::{dao::sovereignty::{capture, policy as policy_dao, structures, systems}, db::{executor::Executor, outbox::Outbox}, lua::{self, content::{OwnedAdmission, PinnedPolicy, PolicyPlan}}, services::{db_err, CallError}, space::World};
use super::{lifecycle, notifications, policy, stale_error as stale, CapitalPublication, CommittedPublication};

pub(crate) enum PreparedAllianceChange {
    NoOwnershipChange,
    OwnershipChange { hubs: Vec<structures::HubRow>, targets: Vec<capture::CaptureRow>, owned: Vec<systems::SystemRow>, expected: policy_dao::PolicyRow, plan: PolicyPlan, pin: PinnedPolicy },
}

pub(crate) enum AdmittedAllianceChange {
    NoOwnershipChange,
    OwnershipChange { hubs: Vec<structures::HubRow>, targets: Vec<capture::CaptureRow>, owned: Vec<systems::SystemRow>, expected: policy_dao::PolicyRow, plan: PolicyPlan, admission: OwnedAdmission, actor: Option<i64>, neutral_owner: i64 },
}


/// This performs all Lua evaluation and dependency reads before any locks.
pub(crate) async fn prepare(ex: &(impl Executor + ?Sized), corporation: i64, now: i64) -> Result<PreparedAllianceChange, CallError> {
    let systems = systems::for_corporation(ex, corporation).await.map_err(db_err)?;
    if systems.is_empty() { return Ok(PreparedAllianceChange::NoOwnershipChange); }
    let alliance = systems[0].alliance_id.ok_or_else(|| db_err(anyhow::anyhow!("claimed Hub has no alliance")))?;
    if systems.iter().any(|row| row.alliance_id != Some(alliance)) {
        return Err(db_err(anyhow::anyhow!("corporation Hub claims disagree on alliance")));
    }
    let mut hubs = Vec::with_capacity(systems.len());
    let mut targets = Vec::with_capacity(systems.len());
    for row in &systems {
        let mut snapshots = structures::snapshots(ex, Some(row.system_id), None).await.map_err(db_err)?;
        if snapshots.len() != 1 || snapshots[0].system != *row {
            if systems::read(ex, row.system_id).await.map_err(db_err)?.as_ref() != Some(row) { return Err(stale()); }
            return Err(db_err(anyhow::anyhow!("claimed Hub snapshot is missing")));
        }
        let hub = snapshots.remove(0);
        let target = match capture::read(ex, hub.structure.item_id).await.map_err(db_err)? {
            Some(target) => target,
            None => {
                if systems::read(ex, row.system_id).await.map_err(db_err)?.as_ref() != Some(row) { return Err(stale()); }
                return Err(db_err(anyhow::anyhow!("claimed Hub capture target is missing")));
            }
        };
        targets.push(target);
        hubs.push(hub);
    }
    let identities: Vec<_> = hubs.iter().map(|hub| (hub.structure.content_namespace.as_str(), hub.structure.content_version.as_str()))
        .chain(targets.iter().map(|target| (target.content_namespace.as_str(), target.content_version.as_str()))).collect();
    super::validate_retained(&identities).await?;
    let owned = systems::for_alliance(ex, alliance).await.map_err(db_err)?;
    let expected = policy_dao::read(ex, alliance).await.map_err(db_err)?
        .ok_or_else(|| db_err(anyhow::anyhow!("claimed Hub policy missing")))?;
    let pin = lua::content_policy("sovereignty")?;
    let plan = match expected.current_capital_system.filter(|capital| systems.iter().any(|row| row.system_id == *capital)) {
        Some(system) => policy::prepare_loss(&pin, &expected, &owned, system, now)?,
        None => PolicyPlan::Unchanged,
    };
    Ok(PreparedAllianceChange::OwnershipChange { hubs, targets, owned, expected, plan, pin })
}

pub(crate) fn admit(world: &World, prepared: PreparedAllianceChange, actor: Option<i64>) -> Result<AdmittedAllianceChange, CallError> {
    match prepared {
        PreparedAllianceChange::NoOwnershipChange => Ok(AdmittedAllianceChange::NoOwnershipChange),
        PreparedAllianceChange::OwnershipChange { hubs, targets, owned, expected, plan, pin } => {
            if !crate::space::sovereignty::is_ready(world) { return Err(stale()); }
            let admission = lua::admit_content(&pin)?;
            Ok(AdmittedAllianceChange::OwnershipChange { hubs, targets, owned, expected, plan, admission, actor, neutral_owner: world.sde().sovereignty.hub().deploy.default_owner })
        }
    }
}

/// Called inside the existing organization transaction after canonical admission.
pub(crate) async fn apply<X: Send>(ex: &(impl Executor + ?Sized), outbox: &mut Outbox<X>, corporation: i64, now: i64, admitted: &AdmittedAllianceChange) -> Result<(), CallError> {
    let current = systems::for_corporation(ex, corporation).await.map_err(db_err)?;
    match admitted {
        AdmittedAllianceChange::NoOwnershipChange => if !current.is_empty() { return Err(stale()); },
        AdmittedAllianceChange::OwnershipChange { hubs, targets, owned, expected, plan, admission, actor, neutral_owner } => {
            if current != hubs.iter().map(|hub| hub.system.clone()).collect::<Vec<_>>() { return Err(stale()); }
            let locked_policy = policy_dao::lock(ex, expected.alliance_id).await.map_err(db_err)?;
            if locked_policy != *expected || systems::for_alliance(ex, expected.alliance_id).await.map_err(db_err)? != *owned { return Err(stale()); }
            for hub in hubs {
                if systems::lock(ex, hub.system.system_id).await.map_err(db_err)? != hub.system { return Err(stale()); }
            }
            let updated_policy = policy::apply(ex, expected, plan, now).await?;
            let mut publications = Vec::with_capacity(hubs.len());
            for (hub, prepared_target) in hubs.iter().zip(targets) {
                let actual = structures::snapshots(ex, Some(hub.system.system_id), None).await.map_err(db_err)?;
                if actual.as_slice() != std::slice::from_ref(hub) { return Err(stale()); }
                let target = capture::read(ex, hub.structure.item_id).await.map_err(db_err)?.ok_or_else(stale)?;
                if target.content_namespace != prepared_target.content_namespace || target.content_version != prepared_target.content_version { return Err(stale()); }
                let key = format!("alliance-change:{}:{}", hub.structure.item_id, hub.system.revision);
                let result = lifecycle::neutralize_locked(ex, hub, &target, *neutral_owner, admission.policy(), now, *actor, &key).await?;
                let after = result.publication.hub.clone();
                notifications::transition(ex, outbox, &notifications::HubTransition {
                    history: result.history, before: Some(hub.clone()), after,
                    capital: (updated_policy != *expected && expected.current_capital_system == Some(hub.system.system_id)).then(|| (expected.clone(), updated_policy.clone())), destruct_deadline: None,
                }).await?;
                publications.push(result.publication);
            }
            outbox.sovereignty(CommittedPublication { hubs: publications, capitals: vec![CapitalPublication::new(updated_policy, expected.current_capital_system)], ..Default::default() });
        }
    }
    Ok(())
}
