//! Durable idle abort of personal station trades; client `pvptradesvc.py` consumes `OnTradeCancel`.
//! `game.trades.updated_at` and `config.trade.idle_timeout_secs` determine the deadline;
//! `game.jobs` stores the durable job. Scheduling and cancellation share the trade
//! transaction, with scheduler wakeup after commit.
//! This module schedules no actor-space timers and has no in-memory fallback or extra runner.
use std::{future::Future, pin::Pin, sync::Arc};
use crate::{client, dao::trades::{self, Trade}, db::{executor::Executor, outbox::Outbox, Db}, scheduler::{self, Job, Registration}, services::{db_err, market, CallError}, space::World};
use super::{dispatch, settle, TradeEffect};

/// Server design's durable handler discriminator.
const KIND: &str = "trade.idle_timeout";

fn id(trade_id: i64) -> String { format!("trade-timeout:{trade_id}") }

fn deadline(updated_at: i64) -> anyhow::Result<i64> {
    updated_at.checked_add(client::filetime_ticks_from_seconds(crate::config::game().trade.idle_timeout_secs))
        .ok_or_else(|| anyhow::anyhow!("trade idle deadline overflow"))
}

pub(super) async fn refresh(ex: &(impl Executor + ?Sized), outbox: &mut Outbox<TradeEffect>, trade: &Trade) -> Result<(), CallError> {
    let job = Job { id: id(trade.id), kind: KIND.into(), due_at: deadline(trade.updated_at)?, payload: serde_json::to_string(&trade.id).map_err(db_err)? };
    scheduler::schedule(ex, &job).await.map_err(db_err)?;
    outbox.operation(TradeEffect::WakeScheduler);
    Ok(())
}

pub(super) async fn cancel(ex: &(impl Executor + ?Sized), trade_id: i64) -> Result<(), CallError> {
    scheduler::cancel(ex, &id(trade_id)).await.map_err(db_err)?;
    Ok(())
}

pub(super) fn registration() -> Registration { scheduler::register(KIND, handle) }

fn handle(db: Db, world: Arc<World>, job: Job) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send>> {
    Box::pin(async move {
        let trade_id: i64 = serde_json::from_str(&job.payload)?;
        let _lock = market::lock().await;
        let sde = world.sde().clone();
        db.transaction(|tx, outbox: &mut Outbox<TradeEffect>| {
            let sde = sde.clone();
            Box::pin(async move {
                let Some(mut trade) = trades::get(tx, trade_id).await.map_err(db_err)? else { return Ok(()); };
                if trade.state == "open" && deadline(trade.updated_at)? <= client::filetime_now() {
                    settle::abort(tx, &sde, outbox, &mut trade, true).await?;
                }
                Ok(())
            })
        }, |value, outbox| async move { dispatch(value, outbox, &world) }).await.map_err(|error| match error {
            CallError::Internal(error) => error,
            CallError::User { msg, .. } => anyhow::anyhow!("trade timeout failed: {msg}"),
        })
    })
}

#[cfg(all(test, feature = "tests-economy"))] mod tests;
