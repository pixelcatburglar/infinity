//! Locked, unaggregated mining delivery snapshots in game.character_mining_ledger.
//! The caller keeps the row locks and marker updates in the aggregation transaction.

use crate::{db::{self, executor::Executor}, lua::events::resources::MiningResourceKind};

#[derive(Debug)]
pub(crate) struct ProgressLog {
    pub log_id: i64,
    pub character_id: i64,
    pub event_time: i64,
    pub solar_system_id: i64,
    pub type_id: i32,
    pub quantity: i64,
    pub unit_volume_m3: f64,
    pub resource_kind: MiningResourceKind,
    pub corporation_id: Option<i64>,
    pub alliance_id: Option<i64>,
    pub faction_id: Option<i64>,
    pub source_ship_id: i64,
    pub source_ship_type_id: i32,
}

pub(crate) async fn lock_pending(ex: &(impl Executor + ?Sized), limit: i64) -> Result<Vec<ProgressLog>, db::Error> {
    if limit <= 0 { return Err(db::Error::Progress(anyhow::anyhow!("mining batch limit must be positive"))); }
    let rows = ex.query("SELECT log_id, character_id, event_time, solar_system_id, type_id, quantity, unit_volume_m3, resource_kind, corporation_id, alliance_id, faction_id, source_ship_id, source_ship_type_id FROM game.character_mining_ledger WHERE NOT progress_aggregated ORDER BY log_id LIMIT $1 FOR UPDATE SKIP LOCKED", &[&limit]).await?;
    rows.into_iter().map(|row| {
        let log_id: i64 = row.get("log_id");
        let invalid = || db::Error::Progress(anyhow::anyhow!("invalid pending mining snapshot {log_id}"));
        let unit_volume_m3: f64 = row.get::<_, Option<f64>>("unit_volume_m3").ok_or_else(invalid)?;
        let quantity: i64 = row.get("quantity");
        if quantity <= 0 || !unit_volume_m3.is_finite() || unit_volume_m3 <= 0.0 || !(unit_volume_m3 * quantity as f64).is_finite() { return Err(invalid()); }
        let resource_kind = match row.get::<_, Option<String>>("resource_kind").as_deref() {
            Some("ore") => MiningResourceKind::Ore,
            Some("gas") => MiningResourceKind::Gas,
            _ => return Err(invalid()),
        };
        Ok(ProgressLog {
            log_id, character_id: row.get("character_id"), event_time: row.get("event_time"),
            solar_system_id: row.get("solar_system_id"), type_id: row.get("type_id"), quantity,
            unit_volume_m3, resource_kind, corporation_id: row.get("corporation_id"),
            alliance_id: row.get("alliance_id"), faction_id: row.get("faction_id"),
            source_ship_id: row.get::<_, Option<i64>>("source_ship_id").ok_or_else(invalid)?,
            source_ship_type_id: row.get::<_, Option<i32>>("source_ship_type_id").ok_or_else(invalid)?,
        })
    }).collect()
}

pub(crate) async fn mark_aggregated(ex: &(impl Executor + ?Sized), ids: &[i64]) -> Result<u64, db::Error> {
    ex.execute("UPDATE game.character_mining_ledger SET progress_aggregated = true WHERE NOT progress_aggregated AND log_id = ANY($1)", &[&ids]).await
}
