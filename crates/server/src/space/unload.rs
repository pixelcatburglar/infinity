//! Explicit and automatic idle-system unloading.
//! Uses the existing actor, worker and snapshot protocols; no client protocol
//! or durable timer state is introduced. Busy systems remain loaded. The
//! automatic entry point only carries weak identities and an idle epoch; it
//! never holds a system handle, sender or worker slot.
use super::*;
use actor::lifecycle::StopDecision;

#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct UnloadReport {
    pub(crate) total: i64,
    pub(crate) unloaded: i64,
    pub(crate) busy: i64,
    pub(crate) changed: i64,
    pub(crate) failed: i64,
    pub(crate) unprocessed: i64,
}

enum Outcome { Unloaded, Busy(&'static str), Changed }
impl Pilots {
    pub(super) fn unload_blocked(&self, id: i64) -> Option<&'static str> {
        let queues = crate::poison::lock_or_exit(self.queues.lock());
        let now = crate::client::filetime_now();
        queues.get(&id).filter(|q| !q.waiting.is_empty()
            || q.keys.values().any(|issued| now - *issued < JUMP_KEY_LIFE * FILETIME_SECOND))
            .map(|_| "jump_queue")
    }
}


impl World {
    pub(super) async fn send_to_loaded(&self, system: i64, job: actor::Job) -> Result<()> {
        let admission = self.admission(system).map_err(|error| anyhow::anyhow!(error))?;
        debug_assert_eq!(admission.system(), system);
        let job = match self.running(system) {
            Some(handle) => match handle.send(job) { Ok(()) => return Ok(()), Err(error) => error.0 },
            None => job,
        };
        admission.check().map_err(|error| anyhow::anyhow!(error))?;
        self.system(system).await?;
        self.running(system).ok_or_else(|| anyhow::anyhow!("system actor stopped"))?
            .send(job).map_err(|_| anyhow::anyhow!("system actor stopped"))
    }

    pub(crate) async fn unload_idle_systems(&self) -> Result<UnloadReport> {
        if !crate::config::game().gm.performance_testing { anyhow::bail!("system unloading disabled"); }
        let world = self.shared().ok_or_else(|| anyhow::anyhow!("world unavailable"))?;
        let mut targets: Vec<_> = crate::poison::lock_or_exit(self.systems.lock()).iter()
            .filter(|(_, handle)| !handle.is_closed())
            .map(|(&id, handle)| (id, Arc::downgrade(&handle.gate))).collect();
        targets.sort_unstable_by_key(|(id, _)| *id);
        tokio::spawn(async move {
            let mut report = UnloadReport { total: targets.len() as i64, unprocessed: targets.len() as i64, ..Default::default() };
            for (id, identity) in targets {
                match world.unload_one(id, &identity, None).await {
                    Ok(Outcome::Unloaded) => report.unloaded += 1,
                    Ok(Outcome::Changed) => report.changed += 1,
                    Ok(Outcome::Busy(reason)) => { report.busy += 1; tracing::debug!(system = id, reason, "system unload skipped"); }
                    Err(error) => { report.failed += 1; tracing::warn!(system = id, %error, "system unload failed"); }
                }
                report.unprocessed -= 1;
            }
            report
        }).await.map_err(|error| anyhow::anyhow!("system unload task failed: {error}"))
    }

    async fn unload_one(&self, id: i64, identity: &std::sync::Weak<tokio::sync::Mutex<()>>, idle_epoch: Option<u64>) -> Result<Outcome> {
        let _market = crate::services::market::lock().await;
        let current = || crate::poison::lock_or_exit(self.systems.lock()).get(&id)
            .filter(|h| !h.is_closed() && std::sync::Weak::ptr_eq(identity, &Arc::downgrade(&h.gate))).cloned();
        let Some(handle) = current() else { return Ok(Outcome::Changed) };
        if let Some(reason) = self.pilots.unload_blocked(id) { return Ok(Outcome::Busy(reason)); }
        if let Some(reason) = handle.with(|s| s.unload_blocked()).await.ok_or_else(|| anyhow::anyhow!("system actor stopped"))? {
            return Ok(Outcome::Busy(reason));
        }
        snapshot::flush(handle.clone(), self.db.clone()).await?;
        let _gate = handle.gate.lock().await;
        {
            let systems = crate::poison::lock_or_exit(self.systems.lock());
            if !systems.get(&id).is_some_and(|h| std::sync::Weak::ptr_eq(identity, &Arc::downgrade(&h.gate))) {
                return Ok(Outcome::Changed);
            }
        }
        match handle.request_idle_stop(idle_epoch).await? {
            StopDecision::Busy(reason) => Ok(Outcome::Busy(reason)),
            StopDecision::Stopping => { handle.wait_exited().await?; Ok(Outcome::Unloaded) }
        }
    }
}

/// Spawn the single in-flight automatic attempt for one idle system. The task
/// owns only weak identities and scalars, upgrades the world inside, runs the
/// ordinary `unload_one` path and returns its own completion instant so the
/// actor's retry clock does not depend on when the join is observed.
pub(in crate::space) fn request_auto_unload(
    world: std::sync::Weak<World>, id: i64, identity: std::sync::Weak<tokio::sync::Mutex<()>>, idle_epoch: u64,
) -> tokio::task::JoinHandle<tokio::time::Instant> {
    tokio::spawn(async move {
        let future = async move {
            let Some(world) = world.upgrade() else { anyhow::bail!("world unavailable") };
            world.unload_one(id, &identity, Some(idle_epoch)).await
        };
        match crate::runtime_watchdog::watch("space_unload", std::any::type_name_of_val(&future), future).await {
            Ok(Outcome::Unloaded) => tracing::debug!(system = id, "idle system unloaded"),
            Ok(Outcome::Changed) => tracing::debug!(system = id, "idle system unload skipped: actor changed"),
            Ok(Outcome::Busy(reason)) => tracing::debug!(system = id, reason, "idle system unload skipped"),
            Err(error) => tracing::warn!(system = id, %error, "idle system unload failed"),
        }
        tokio::time::Instant::now()
    })
}

#[cfg(all(test, feature = "tests-space"))] mod tests;
