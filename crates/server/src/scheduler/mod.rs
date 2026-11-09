//! Durable, at-least-once deadlines backed by game.jobs, not space actor timers.
//! Handlers must be idempotent and finish within the lease; leases are not renewed.
//! Maintenance scans are unbounded; operators must size the lease for their data.
//! Local active claims are excluded from reclaim, but multiple processes require
//! lease renewal or persistent ownership to prevent simultaneous execution.
//! Register handlers at startup. In a transaction, call `schedule(tx, &job).await?`
//! and `outbox.operation(MyEffect::WakeScheduler)` on an `Outbox<MyEffect>`.
//! In the transaction's post-commit dispatcher, match `Effect::Operation` and
//! call `wake()` for that variant. For autocommit, call `wake()` only after
//! `schedule` returns successfully.
//! Payloads are JSON text; this module does not mirror a client service.
use std::{future::Future, pin::Pin, sync::{Arc, LazyLock}};
use crate::{client::filetime_now, dao::jobs, db::{executor::Executor, Db, Error}, space::World};
use tokio::sync::Notify;

pub(crate) mod runner;
#[cfg(test)] mod tests;

#[derive(Clone, Debug)]
pub(crate) struct Job { pub id: String, pub kind: String, pub due_at: i64, pub payload: String }
pub(crate) type Handler = fn(Db, Arc<World>, Job) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send>>;
pub(crate) struct Registration { pub kind: &'static str, pub handler: Handler, pub interval_secs: Option<u64> }
pub(crate) const MARKET: &str = "periodic:market.expire";
pub(crate) const CONTRACTS: &str = "periodic:contracts.settle_auctions";
pub(crate) const BILLS: &str = "periodic:bills.run";
pub(crate) const MINING_LEDGER: &str = "periodic:mining.ledger_retention";
pub(crate) const MINING_PROGRESS: &str = "periodic:mining.progress_aggregation";
pub(crate) const MAP_HISTORY: &str = "periodic:map.history_retention";
pub(crate) const PERIODIC_KINDS: [&str; 6] = [MARKET, CONTRACTS, BILLS, MINING_LEDGER, MINING_PROGRESS, MAP_HISTORY];
pub(crate) async fn seed(db: &Db) -> Result<(), Error> {
    let now = filetime_now();
    for kind in PERIODIC_KINDS {
        let job = Job { id: kind.into(), kind: kind.into(), due_at: now, payload: "{}".into() };
        jobs::seed(db, &job, now).await?;
    }
    wake();
    Ok(())
}
pub(crate) fn register(kind: &'static str, handler: Handler) -> Registration { Registration { kind, handler, interval_secs: None } }
pub(crate) fn periodic(kind: &'static str, handler: Handler, interval_secs: u64) -> Registration { Registration { interval_secs: Some(interval_secs), ..register(kind, handler) } }
pub(crate) async fn schedule(ex: &(impl Executor + ?Sized), job: &Job) -> Result<u64, Error> { jobs::schedule(ex, job, filetime_now()).await }
pub(crate) async fn cancel(ex: &(impl Executor + ?Sized), id: &str) -> Result<u64, Error> { jobs::cancel(ex, id).await }
static NOTIFY: LazyLock<Notify> = LazyLock::new(Notify::new);
pub(crate) fn wake() { NOTIFY.notify_one(); }
