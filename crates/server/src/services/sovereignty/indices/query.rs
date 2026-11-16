//! Batch read views and the startup capability snapshot for index queries.
//! No client module is mirrored; rows come from `dao::sovereignty` and policy
//! history from retained packages. Reads are read-only except the one approved
//! refresh: a newly published policy segment that starts before a stored anchor
//! is reprojected over already-applied facts before answering, without touching
//! delivery receipts or notifying the actor (the maintenance job republishes).

use std::sync::Arc;
use crate::{dao::sovereignty::development, db::Db, lua::RetainedPolicies, progress::{ApplyError, ContentProgressResources}, sde::Sde, services::CallError};
use super::{activation, model::{DevelopmentView, IndexCapability, IndexLoadError}, replay, rules};

/// One batch of development views for explicit systems or all active candidates.
/// A stored row that disagrees with the retained history is refreshed first;
/// malformed rows stay errors, never silent zeroes. The returned history has
/// passed the same activation identity check as the views.
pub(crate) async fn read_views(db: &Db, sde: &Sde, policies: &RetainedPolicies, systems: Option<&[i64]>, now: i64) -> Result<(Arc<activation::PolicyHistory>, Vec<DevelopmentView>), CallError> {
    for attempt in 0..=3 {
        let history = activation::load(policies)?;
        let candidates;
        let systems = match systems {
            Some(systems) => systems,
            None => {
                candidates = development::active_systems(db, 0, i64::MAX).await?;
                &candidates
            }
        };
        let observations = development::observations(db, systems).await?;
        let mut refreshed = false;
        for observation in &observations {
            let backdated = observation.development.as_ref().is_some_and(|row| {
                history.segments.iter().any(|segment| segment.ordinal > row.activation_ordinal && segment.starts_at <= row.evaluated_at)
            });
            if !backdated { continue; }
            if attempt == 3 {
                return Err(crate::services::db_err(anyhow::anyhow!("development-index refresh stayed stale")));
            }
            refreshed = true;
            let plan = Arc::new(replay::plan(db, sde, policies, history.clone(), observation.system_id, None, &[], now).await?);
            match db.transaction(|tx, _outbox: &mut ()| {
                let plan = plan.clone();
                Box::pin(async move {
                    plan.prelock(tx).await?;
                    plan.apply_state(tx).await
                })
            }, |(), _| async move { Ok(()) }).await {
                Ok(()) | Err(ApplyError::Reprepare) => {},
                Err(error) => return Err(error.into_call()),
            }
        }
        if !refreshed {
            if !policies.is_current(crate::lua::content::indices::NAMESPACE, history.identities.last().map(|row| row.ordinal))? {
                continue;
            }
            let views = observations.iter()
                .map(|row| rules::project(row.development.as_ref(), row.system.as_ref(), row.capital.as_ref(), &history, row.system_id, now))
                .collect::<Result<Vec<_>, _>>()?;
            return Ok((history, views));
        }
    }
    Err(crate::services::db_err(anyhow::anyhow!("development-index refresh stayed stale")))
}

/// Compile the retained policy history into the capability snapshot installed
/// by startup wiring. Runs outside every market, database and actor lock; the
/// result is a cache, never the policy authority.
pub(crate) async fn initialize(resources: &ContentProgressResources) -> IndexCapability {
    match activation::load(&resources.policies) {
        Ok(history) => IndexCapability::Ready(history),
        Err(IndexLoadError::Unavailable(unavailable)) => IndexCapability::Unavailable(unavailable),
        Err(IndexLoadError::Invalid(error)) => {
            let diagnostic: Arc<str> = format!("{error:?}").into();
            tracing::warn!("development-index capability rejected available state: {error:?}");
            IndexCapability::Invalid(diagnostic)
        }
    }
}
