//! Durable periodic retention for game.character_mining_ledger and game.jobs.
//! This mechanic has no client service; the personal ledger client lives in
//! `eve/client/script/ui/shared/ledger/ledgerSvc.py`. The operator's rolling
//! retention window deletes only aggregated older facts; pending progress survives.

use std::{future::Future, pin::Pin, sync::Arc};
use infinity_core::consts::FILETIME_DAY;
use crate::{client::filetime_now, dao::mining_ledger, db::Db, scheduler::Job, space::World};

pub(super) fn cutoff(now: i64) -> i64 {
    now.saturating_sub(crate::config::game().mining.ledger_retention_days.saturating_mul(FILETIME_DAY))
}

pub(crate) fn handle(db: Db, _world: Arc<World>, _job: Job) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send>> {
    Box::pin(async move {
        mining_ledger::delete_before(&db, cutoff(filetime_now())).await?;
        Ok(())
    })
}
