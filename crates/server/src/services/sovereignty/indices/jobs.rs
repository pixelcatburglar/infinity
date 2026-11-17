//! The single self-rescheduling development-index maintenance job.
//! One ordinary `game.jobs` row walks every active system in bounded pages,
//! analytically advances offline decay and refreshes state the same way a
//! receipt does, then reschedules itself at the next real display boundary,
//! the next aligned decay interval or the next strategic-age threshold, only
//! when the stored job still carries the identity this run claimed.
//! No periodic registration, timer thread or sleep is involved.

use std::{future::Future, pin::Pin, sync::Arc};
use crate::{client::filetime_now, dao::{jobs, lua_content::activations, sovereignty::development}, db::Db, scheduler::{self, Job}, services::{db_err, CallError}, space::World};
use super::{activation::{self, PolicyHistory}, model::{IndexCapability, IndexLoadError}, replay};

/// One ordinary self-rescheduling job; the runner acknowledges the claimed
/// token and `schedule` clears the lease, which is the existing replacement
/// semantic for a stale acknowledgement affecting zero rows.
const KIND: &str = "sovereignty.indices";
const JOB_ID: &str = "sovereignty:indices";
/// Server maintenance policy: page bound for the active-system scan.
const PAGE: i64 = 64;

/// The policy head this job last observed; kept inside the payload so a newer
/// publication reschedule is never overwritten by a stale run.
#[derive(serde::Serialize, serde::Deserialize, PartialEq, Eq)]
struct Payload { policy_ordinal: i64 }

impl Payload {
    fn text(&self) -> String { serde_json::to_string(self).expect("job payload is always serializable") }
}

fn job(due_at: i64, ordinal: i64) -> Job {
    Job { id: JOB_ID.to_owned(), kind: KIND.to_owned(), due_at, payload: Payload { policy_ordinal: ordinal }.text() }
}

pub(crate) fn registration() -> scheduler::Registration {
    scheduler::register(KIND, run)
}

/// Seed the single job after Lua startup; an existing scheduled or running row
/// is never overridden, only a dead one is resurrected by `jobs::seed`.
pub(crate) async fn seed(db: &Db, policies: &crate::lua::RetainedPolicies) -> Result<(), CallError> {
    let Some(ordinal) = policies.activations(crate::lua::content::indices::NAMESPACE)?.last().map(|row| row.ordinal) else {
        return Ok(());
    };
    let now = filetime_now();
    jobs::seed(db, &job(now, ordinal), now).await?;
    scheduler::wake();
    Ok(())
}

/// Reschedule this run's job under a row lock only when the stored row still
/// carries the full identity (id, kind, due, payload) this run claimed; a newer
/// publication or reload reschedule wins and is left alone.
async fn reschedule(db: &Db, claimed: Job, next: Job, now: i64) -> Result<(), CallError> {
    let pair = Arc::new((claimed, next));
    db.transaction(|tx, _outbox: &mut ()| {
        let pair = pair.clone();
        Box::pin(async move {
            let (claimed, next) = &*pair;
            let unchanged = jobs::lock_job(tx, &claimed.id).await?.is_some_and(|stored| {
                stored.id == claimed.id && stored.kind == claimed.kind && stored.due_at == claimed.due_at && stored.payload == claimed.payload
            });
            if unchanged {
                jobs::schedule(tx, next, now).await?;
            }
            Ok::<_, crate::db::Error>(())
        })
    }, |(), _| async move { Ok(()) }).await?;
    Ok(())
}

/// Compute the next active-policy interval, activation, or claim boundary.
fn next_due(history: &PolicyHistory, now: i64) -> i64 {
    let Some(segment) = history.segment_at(now) else { return history.segments[0].starts_at };
    let interval = segment.definitions.decay_interval_ticks;
    let mut due = now.saturating_add(interval - now.rem_euclid(interval));
    if let Some(next) = history.segments.iter().find(|segment| segment.starts_at > now) {
        due = due.min(next.starts_at);
    }
    due
}

/// Called by the Lua host after a real `sovereignty_indices` activation change:
/// refresh the derived capability cache first, then wake the same job. The
/// activation is already committed; an error here is a pending condition, not
/// a silent success.
/// The sole production caller holds the host reload guard; additional callers
/// must preserve that serialization so capability installation stays ordered.
pub(crate) async fn policy_published(db: &Db, world: &Arc<World>, policies: &crate::lua::RetainedPolicies) -> Result<(), CallError> {
    let now = filetime_now();
    let Some(ordinal) = policies.activations(crate::lua::content::indices::NAMESPACE)?.last().map(|row| row.ordinal) else {
        return Err(db_err(anyhow::anyhow!("published development-index activation missing")));
    };
    db.transaction(|tx, _outbox: &mut ()| Box::pin(async move {
        activations::lock_namespace(tx, crate::lua::content::indices::NAMESPACE, false).await?;
        jobs::schedule(tx, &job(now, ordinal), now).await?;
        Ok::<_, CallError>(())
    }), |(), _| async move { Ok(()) }).await?;
    scheduler::wake();
    match activation::load(policies) {
        Ok(history) => {
            crate::space::sovereignty::indices::install_policy(world.as_ref(), IndexCapability::Ready(history));
            Ok(())
        }
        Err(IndexLoadError::Unavailable(unavailable)) => {
            crate::space::sovereignty::indices::install_policy(world.as_ref(), IndexCapability::Unavailable(unavailable.clone()));
            Err(db_err(anyhow::anyhow!("development-index policy unavailable after publication: {unavailable}")))
        }
        Err(IndexLoadError::Invalid(error)) => {
            let diagnostic: Arc<str> = format!("{error:?}").into();
            crate::space::sovereignty::indices::install_policy(world.as_ref(), IndexCapability::Invalid(diagnostic));
            Err(error)
        }
    }
}

/// One complete maintenance pass: bounded pages over every system with points,
/// trend baselines or a live claim; per-system refresh uses the same prepared
/// fold and short transaction as a receipt.
fn run(db: Db, world: Arc<World>, claimed: Job) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send>> {
    Box::pin(async move {
        anyhow::ensure!(claimed.id == JOB_ID && claimed.kind == KIND, "development-index job identity mismatch");
        serde_json::from_str::<Payload>(&claimed.payload).map_err(|error| anyhow::anyhow!("development-index job payload: {error}"))?;
        let policies = crate::lua::retained_policies().map_err(|error| anyhow::anyhow!("{error:?}"))?;
        let now = filetime_now();
        let history = activation::load(&policies)
            .map_err(|error| anyhow::anyhow!("development-index policy load: {error:?}"))?;
        let mut due = next_due(&history, now);
        if let Some(active) = history.segment_at(now) {
            let mut after = 0_i64;
            loop {
                let page = development::active_systems(&db, after, PAGE).await?;
                if page.is_empty() { break; }
                after = *page.last().expect("non-empty page");
                let observations = development::observations(&db, &page).await?;
                for observation in observations {
                    if let Some(claim) = observation.system.as_ref().and_then(|row| row.claim_time) {
                        for &threshold in &active.definitions.strategic_seconds {
                            let crossing = crate::client::filetime_deadline_from_seconds_saturating(claim, threshold);
                            if crossing > now { due = due.min(crossing); break; }
                        }
                    }
                    replay::refresh_committed(&db, world.as_ref(), &policies, &history, observation, now).await
                        .map_err(|error| anyhow::anyhow!("{error:?}"))?;
                }
            }
        }
        reschedule(&db, claimed, job(due, history.head().ordinal), now).await
            .map_err(|error| anyhow::anyhow!("{error:?}"))?;
        Ok(())
    })
}

#[cfg(all(test, feature = "tests-content"))]
mod tests;
