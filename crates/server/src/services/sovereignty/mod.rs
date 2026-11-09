//! Sovereignty Hub ownership and committed actor publications.
//! The Hub uses `game.sov_*` rows and server-authored sovereignty policy.
//! Command-node campaigns and ordinary capital changes are not supported.
//! Organization mutations and capture checkpoints are ordered by the canonical
//! `registry::lock_admission` guards. Business and pure-plan `now` values stay
//! frozen across retries; source invalidation instead samples a per-attempt
//! linearization stamp after the mutation under those guards, rolled back if
//! the attempt aborts.
mod model;
pub(crate) mod authority;
pub(crate) mod policy;
pub(crate) mod deploy;
pub(crate) mod lifecycle;
pub(crate) mod capture;
pub(crate) mod recovery;
pub(crate) mod jobs;
pub(crate) mod governance;
pub(crate) mod notifications;
pub(crate) mod wire;
pub(crate) mod queries;
pub(crate) use model::{CapitalPublication, CaptureContribution, CaptureContributor, CaptureObservation, CommittedPublication, HubPublication, PlacementObstacle, PlacementProof, Resources, TransitionKind};

use crate::{dao::sovereignty::sources, db::{executor::Executor, outbox::Outbox}, services::{db_err, CallError}};

pub(crate) fn stale_error() -> CallError { CallError::Internal(anyhow::Error::new(model::StalePlan)) }
pub(crate) fn is_stale(error: &CallError) -> bool {
    matches!(error, CallError::Internal(error) if error.downcast_ref::<model::StalePlan>().is_some())
}

/// Validate each distinct persisted authored identity before acquiring any guard.
pub(crate) async fn validate_retained(identities: &[(&str, &str)]) -> Result<(), CallError> {
    let unique: std::collections::BTreeSet<_> = identities.iter().copied().collect();
    for (namespace, version) in unique {
        crate::lua::retained_content(namespace, version).await?;
    }
    Ok(())
}

/// Invalidate only activated link sources as part of the organization transaction.
pub(crate) async fn invalidate_sources<X: Send>(
    ex: &(impl Executor + ?Sized), outbox: &mut Outbox<X>, characters: &[i64],
) -> Result<(), CallError> {
    if characters.is_empty() { return Ok(()); }
    let changes = sources::invalidate(ex, characters, crate::client::filetime_now()).await.map_err(db_err)?;
    if !changes.is_empty() {
        outbox.sovereignty(CommittedPublication { sources: changes, ..Default::default() });
    }
    Ok(())
}

#[cfg(test)]
mod tests;
