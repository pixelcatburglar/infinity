//! NPC effects FIFO and concurrent deferred work outside the system actor.
//! These server-owned workers use no client module or persistent data; they do
//! not provide durable execution across process restarts.
use super::{NpcFuture, System};
use tokio::sync::{mpsc, oneshot};
use tokio::task::{JoinHandle, JoinSet};

type Probe = oneshot::Sender<bool>;

pub(in crate::space) enum Workers {
    Production { tasks: Tasks },
    Controlled { probes: Probes, tasks: Tasks },
}

pub(in crate::space) struct Probes {
    effects: mpsc::UnboundedSender<Probe>,
    deferred: mpsc::UnboundedSender<Probe>,
}

pub(in crate::space) struct Tasks {
    effects: JoinHandle<()>,
    /// `false` when any owned deferred job failed; the supervisor retires on it.
    deferred: JoinHandle<bool>,
}

pub(in crate::space) fn start(system: &mut System, stop_enabled: bool) -> Workers {
    let (tx, mut rx) = mpsc::unbounded_channel::<NpcFuture>();
    let (deferred_tx, mut deferred_rx) = mpsc::unbounded_channel::<NpcFuture>();
    system.effect_jobs = Some(tx);
    system.deferred_jobs = Some(deferred_tx);
    if !stop_enabled {
        let effects = tokio::spawn(async move {
            while let Some(job) = rx.recv().await { job.await; }
        });
        let deferred = tokio::spawn(async move {
            let mut tasks = JoinSet::new();
            let mut clean = true;
            loop {
                tokio::select! {
                    job = deferred_rx.recv() => match job {
                        Some(job) => { tasks.spawn(job); }
                        None => break,
                    },
                    result = tasks.join_next(), if !tasks.is_empty() => {
                        if let Some(Err(error)) = result {
                            tracing::warn!(%error, "space deferred job failed");
                            clean = false;
                        }
                    }
                }
            }
            while let Some(result) = tasks.join_next().await {
                if let Err(error) = result {
                    tracing::warn!(%error, "space deferred job failed");
                    clean = false;
                }
            }
            clean
        });
        return Workers::Production { tasks: Tasks { effects, deferred } };
    }
    let (effects_probe, mut effects_probes) = mpsc::unbounded_channel::<Probe>();
    let (deferred_probe, mut deferred_probes) = mpsc::unbounded_channel::<Probe>();
    let effects = tokio::spawn(async move {
        loop {
            tokio::select! {
                job = rx.recv() => match job {
                    Some(mut job) => loop {
                        tokio::select! {
                            _ = &mut job => break,
                            Some(probe) = effects_probes.recv() => { let _ = probe.send(false); }
                        }
                    },
                    None => break,
                },
                Some(probe) = effects_probes.recv() => { let _ = probe.send(rx.is_empty()); }
            }
        }
    });
    let deferred = tokio::spawn(async move {
        let mut tasks = JoinSet::new();
        let mut clean = true;
        loop {
            tokio::select! {
                job = deferred_rx.recv() => match job {
                    Some(job) => { tasks.spawn(job); }
                    None => break,
                },
                result = tasks.join_next(), if !tasks.is_empty() => {
                    if let Some(Err(error)) = result {
                        tracing::warn!(%error, "space deferred job failed");
                        clean = false;
                    }
                },
                Some(probe) = deferred_probes.recv() => {
                    let mut failed = false;
                    while let Some(result) = tasks.try_join_next() {
                        if let Err(error) = result {
                            tracing::warn!(%error, "space deferred job failed");
                            failed = true;
                            clean = false;
                        }
                    }
                    if failed { deferred_probes.close(); }
                    if !deferred_probes.is_closed() {
                        let _ = probe.send(deferred_rx.is_empty() && tasks.is_empty());
                    }
                }
            }
        }
        while let Some(result) = tasks.join_next().await {
            if let Err(error) = result {
                tracing::warn!(%error, "space deferred job failed");
                clean = false;
            }
        }
        clean
    });
    Workers::Controlled {
        probes: Probes { effects: effects_probe, deferred: deferred_probe },
        tasks: Tasks { effects, deferred },
    }
}

impl Probes {
    pub(in crate::space) async fn busy_reason(&self) -> anyhow::Result<Option<&'static str>> {
        let (effects_tx, effects_rx) = oneshot::channel();
        let (deferred_tx, deferred_rx) = oneshot::channel();
        self.effects.send(effects_tx).map_err(|_| anyhow::anyhow!("space NPC worker probe closed"))?;
        self.deferred.send(deferred_tx).map_err(|_| anyhow::anyhow!("space deferred worker probe closed"))?;
        let (effects, deferred) = tokio::join!(effects_rx, deferred_rx);
        let effects = effects?;
        let deferred = deferred?;
        Ok(if !effects { Some("npc_effects_worker_busy") }
            else if !deferred { Some("deferred_worker_busy") } else { None })
    }
}

impl Tasks {
    /// EOF both workers; `false` when a worker or an owned deferred job failed.
    pub(in crate::space) async fn join(self) -> bool {
        let (effects, deferred) = tokio::join!(self.effects, self.deferred);
        let mut success = true;
        if let Err(error) = effects {
            tracing::warn!(%error, "space NPC worker failed");
            success = false;
        }
        match deferred {
            Err(error) => {
                tracing::warn!(%error, "space deferred worker failed");
                success = false;
            }
            Ok(clean) => success &= clean,
        }
        success
    }
}

#[cfg(all(test, feature = "tests-space"))] pub(in crate::space) mod tests;
