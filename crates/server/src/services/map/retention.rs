//! Scheduled retention of map history for the rolling windows consumed by
//! `eve/client/script/ui/shared/mapView/mapViewColorHandler.py`; there is no
//! client-side retention counterpart. The existing scheduler applies the
//! operator's map retention policy to `game.map_events` via the telemetry DAO.
//! Cumulative `game.map_visits` are not deleted.
use std::{future::Future, pin::Pin, sync::Arc};
use infinity_core::consts::FILETIME_HOUR;
use crate::{client::filetime_now, dao::map_telemetry, db::Db, scheduler::Job, space::World};

pub(super) fn cutoff(now: i64, hours: i64) -> i64 {
    now.saturating_sub(hours.saturating_mul(FILETIME_HOUR))
}

pub(crate) fn handle(db: Db, _world: Arc<World>, _job: Job) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send>> {
    Box::pin(async move {
        map_telemetry::delete_before(&db, cutoff(filetime_now(), crate::config::game().map.history_retention_hours)).await?;
        Ok(())
    })
}
