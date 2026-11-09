//! Immutable personal mining delivery facts in `game.character_mining_ledger`.
//! The personal ledger client is in `eve/client/script/ui/shared/ledger/`.
//! Rows are append-only except for retention deletion and the progress marker;
//! corporate observers are not supported by this DAO.

pub(crate) mod progress;

use crate::lua::events::resources::MiningResourceKind;
use crate::db::{self, executor::Executor};
use tokio_postgres::Row;

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Log {
    pub log_id: i64,
    pub character_id: i64,
    pub event_time: i64,
    pub solar_system_id: i64,
    pub type_id: i32,
    pub quantity: i64,
    pub quantity_wasted: i64,
}

impl Log {
    fn from_row(row: &Row) -> Self {
        Self {
            log_id: row.get("log_id"),
            character_id: row.get("character_id"),
            event_time: row.get("event_time"),
            solar_system_id: row.get("solar_system_id"),
            type_id: row.get("type_id"),
            quantity: row.get("quantity"),
            quantity_wasted: row.get("quantity_wasted"),
        }
    }
}

pub(crate) struct DeliveryFact {
    pub character_id: i64,
    pub ship_id: i64,
    pub solar_system_id: i64,
    pub type_id: i32,
    pub quantity: i64,
    pub quantity_wasted: i64,
    pub event_time: i64,
    pub unit_volume_m3: f64,
    pub resource_kind: MiningResourceKind,
}

pub(crate) async fn append(ex: &(impl Executor + ?Sized), fact: &DeliveryFact) -> Result<(), db::Error> {
    let kind = match fact.resource_kind { MiningResourceKind::Ore => "ore", MiningResourceKind::Gas => "gas" };
    if !fact.unit_volume_m3.is_finite() || fact.unit_volume_m3 <= 0.0 || !(fact.unit_volume_m3 * fact.quantity as f64).is_finite() {
        return Err(db::Error::Progress(anyhow::anyhow!("invalid mining delivery volume")));
    }
    let count = ex.execute(
        "INSERT INTO game.character_mining_ledger (character_id, solar_system_id, type_id, quantity, quantity_wasted, event_time, source_ship_id, source_ship_type_id, corporation_id, alliance_id, faction_id, resource_kind, unit_volume_m3, progress_aggregated) SELECT c.character_id, $3, $4, $5, $6, $7, s.item_id, s.type_id, c.corporation_id, corp.alliance_id, NULL, $8, $9, false FROM game.characters c JOIN game.items s ON s.item_id = $2 LEFT JOIN game.corporations corp ON corp.corporation_id = c.corporation_id WHERE c.character_id = $1",
        &[&fact.character_id, &fact.ship_id, &fact.solar_system_id, &fact.type_id, &fact.quantity, &fact.quantity_wasted, &fact.event_time, &kind, &fact.unit_volume_m3],
    ).await?;
    if count != 1 { return Err(db::Error::Progress(anyhow::anyhow!("mining delivery source character or ship missing"))); }
    Ok(())
}

pub(crate) async fn list(
    ex: &(impl Executor + ?Sized),
    character_id: i64,
    since: i64,
    through: i64,
) -> Result<Vec<Log>, db::Error> {
    Ok(ex.query(
        "SELECT log_id, character_id, event_time, solar_system_id, type_id, quantity, quantity_wasted FROM game.character_mining_ledger WHERE character_id = $1 AND event_time >= $2 AND event_time <= $3 ORDER BY event_time DESC, log_id DESC",
        &[&character_id, &since, &through],
    ).await?.iter().map(Log::from_row).collect())
}

pub(crate) async fn delete_before(ex: &(impl Executor + ?Sized), cutoff: i64) -> Result<u64, db::Error> {
    ex.execute("DELETE FROM game.character_mining_ledger WHERE progress_aggregated AND event_time < $1", &[&cutoff]).await
}
