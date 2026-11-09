//! Observational map event ingestion from completed arrivals and gameplay deaths.
//! There is no direct client mirror: this projects gameplay completions for
//! `eve/client/script/ui/shared/mapView/mapViewColorHandler.py` and
//! `eve/client/script/ui/shared/mapView/filters/mapFilterVisited.py`.
//! The map telemetry DAO writes `game.map_events` and `game.map_visits`, using
//! gameplay occurrence IDs, killmail IDs and persistent ship IDs as sources.
//! World/actor-to-database crash gaps can lose NPC deaths; this projection is
//! not a second death lifecycle authority. Arrivals are recorded by ship movement.
use crate::{dao::map_telemetry::{self as dao, EventKind, EventSource}, db::{Db, executor::Executor}, services::CallError};
use tracing::warn;


pub(crate) async fn player_lost(ex: &(impl Executor + ?Sized), kill_id: i64, system: i64, is_pod: bool, now: i64) -> Result<(), CallError> {
    dao::append(ex, EventSource::Killmail, kill_id, system, now, if is_pod { EventKind::Pod } else { EventKind::Ship }).await?;
    Ok(())
}

pub(crate) async fn npc_lost(db: &Db, system: i64, now: i64) {
    let occurrence = match dao::next_occurrence(db).await {
        Ok(id) => id,
        Err(error) => { warn!(system, occurrence = "unavailable", %error, "map NPC sequence allocation failed"); return; }
    };
    let result: Result<(), crate::db::Error> = db.transaction(|tx, _: &mut ()| Box::pin(async move {
        dao::append(tx, EventSource::NpcOccurrence, occurrence, system, now, EventKind::Npc).await?;
        Ok(())
    }), |(), ()| async { Ok(()) }).await;
    if let Err(error) = result { warn!(system, occurrence, %error, "map NPC recording failed"); }
}

pub(crate) async fn unpiloted_lost(ex: &(impl Executor + ?Sized), ship: i64, system: i64, now: i64) -> Result<(), CallError> {
    dao::append(ex, EventSource::UnpilotedShip, ship, system, now, EventKind::Ship).await?;
    Ok(())
}
