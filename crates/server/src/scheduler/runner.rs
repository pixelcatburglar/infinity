//! Supervised `game.jobs` workers; no client service is mirrored.
//! Dropping the JoinSet aborts handlers; lease renewal and cross-process exclusion
//! are unsupported.
use std::{collections::HashMap, sync::Arc, time::Duration};
use anyhow::{ensure, Result};
use tokio::{sync::watch, task::{Id, JoinHandle, JoinSet}};
use tracing::{debug, warn};
use crate::{client::{filetime_now, ms_from_filetime_ticks}, config::scheduler::SchedulerConfig, dao::jobs, db::Db, space::World};
use super::{Registration, NOTIFY};

/// Server retry policy: cap exponential backoff at 2^8.
const BACKOFF_EXPONENT_CAP: i32 = 8;

struct ActiveClaim {
    id: String,
    token: i64,
    attempt: i32,
    interval_secs: Option<u64>,
}

pub(super) fn backoff(base: u64, attempt: i32) -> u64 { base.saturating_mul(1_u64 << attempt.saturating_sub(1).clamp(0, BACKOFF_EXPONENT_CAP)) }

pub(super) fn registry(registrations: Vec<Registration>) -> Result<HashMap<&'static str, Registration>> {
    let mut handlers = HashMap::new();
    for registration in registrations {
        ensure!(registration.interval_secs != Some(0), "periodic interval must be positive");
        ensure!(!handlers.contains_key(registration.kind), "duplicate scheduler kind: {}", registration.kind);
        handlers.insert(registration.kind, registration);
    }
    Ok(handlers)
}

pub(crate) fn start(db: Db, world: Arc<World>, registrations: Vec<Registration>, settings: SchedulerConfig, mut shutdown: watch::Receiver<bool>) -> Result<JoinHandle<()>> {
    settings.validate()?;
    let handlers = registry(registrations)?;
    Ok(tokio::spawn(async move {
        let mut tasks = JoinSet::new();
        let mut active: HashMap<Id, ActiveClaim> = HashMap::new();
        loop {
            if *shutdown.borrow() { break; }
            let free = settings.max_concurrent.saturating_sub(tasks.len());
            let mut delay = Duration::from_secs(settings.poll_secs);
            if free > 0 {
                let active_ids: Vec<&str> = active.values().map(|claim| claim.id.as_str()).collect();
                let now = filetime_now();
                let claims = jobs::claim(&db, now, now.saturating_add(jobs::duration_ticks(settings.lease_secs)), free.min(i64::MAX as usize) as i64, &active_ids).await;
                drop(active_ids);
                match claims {
                    Ok(claims) => for claim in claims {
                        if *shutdown.borrow() { break; }
                        if let Some(reg) = handlers.get(claim.job.kind.as_str()) {
                            let handler = reg.handler;
                            let metadata = ActiveClaim {
                                id: claim.job.id.clone(),
                                token: claim.token,
                                attempt: claim.attempt,
                                interval_secs: reg.interval_secs,
                            };
                            let (task_db, task_world, job) = (db.clone(), world.clone(), claim.job);
                            let handle = tasks.spawn(async move { handler(task_db, task_world, job).await });
                            active.insert(handle.id(), metadata);
                        } else {
                            let due = now.saturating_add(jobs::duration_ticks(settings.retry_base_secs));
                            let dead = !super::PERIODIC_KINDS.contains(&claim.job.kind.as_str()) && claim.attempt >= settings.dead_letter_attempts;
                            match jobs::fail(&db, &claim.job.id, claim.token, due, now, "no handler", dead).await {
                                Ok(0) => debug!(job = %claim.job.id, "stale missing-handler outcome"),
                                Ok(_) if dead => warn!(job = %claim.job.id, "scheduler dead-lettered: no handler"),
                                Err(e) => warn!(job = %claim.job.id, "scheduler failure update: {e}"),
                                _ => (),
                            }
                        }
                    },
                    Err(e) => warn!("scheduler claim failed: {e}"),
                }
            }
            if free > 0 {
                let active_ids: Vec<&str> = active.values().map(|claim| claim.id.as_str()).collect();
                if let Ok(Some(due)) = jobs::next_due(&db, &active_ids).await {
                    let remaining = due.saturating_sub(filetime_now()).max(1);
                    let millis = ms_from_filetime_ticks(remaining).max(1) as u64;
                    delay = delay.min(Duration::from_millis(millis));
                }
            }
            tokio::select! {
                biased;
                changed = shutdown.changed() => if changed.is_err() || *shutdown.borrow() { break; },
                Some(outcome) = tasks.join_next_with_id(), if !tasks.is_empty() => finish(&db, &settings, &mut active, outcome).await,
                _ = NOTIFY.notified() => (),
                _ = tokio::time::sleep(delay) => (),
            }
        }
        while let Some(outcome) = tasks.join_next_with_id().await { finish(&db, &settings, &mut active, outcome).await; }
    }))
}

async fn finish(db: &Db, settings: &SchedulerConfig, active: &mut HashMap<Id, ActiveClaim>, outcome: std::result::Result<(Id, Result<()>), tokio::task::JoinError>) {
    let id = match &outcome { Ok((id, _)) => *id, Err(error) => error.id() };
    let Some(claim) = active.remove(&id) else { return; };
    let now = filetime_now();
    let result = match outcome { Ok((_, result)) => result, Err(error) => Err(error.into()) };
    let updated = match result {
        Ok(()) => match claim.interval_secs {
            Some(secs) => jobs::complete_periodic(db, &claim.id, claim.token, now.saturating_add(jobs::duration_ticks(secs)), now).await,
            None => jobs::ack(db, &claim.id, claim.token).await,
        },
        Err(error) => {
            let dead = claim.interval_secs.is_none() && claim.attempt >= settings.dead_letter_attempts;
            let due = now.saturating_add(jobs::duration_ticks(backoff(settings.retry_base_secs, claim.attempt)));
            let updated = jobs::fail(db, &claim.id, claim.token, due, now, &format!("{error:#}"), dead).await;
            if dead && matches!(updated, Ok(n) if n > 0) {
                warn!(job = %claim.id, "scheduler dead-lettered: {error:#}");
            }
            updated
        }
    };
    match updated { Ok(0) => debug!(job = %claim.id, "stale scheduler outcome"), Err(error) => warn!(job = %claim.id, "scheduler outcome write failed: {error}"), _ => () }
}

#[cfg(test)]
mod tests;
