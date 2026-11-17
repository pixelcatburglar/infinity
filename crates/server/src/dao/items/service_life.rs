//! Per-physical-item service-life damage in `game.item_service_life`; an
//! absent row is a pristine item, never damage zero.
//! The client reads damage only through `repairableShipMgr` queries.
//! This DAO has no direct client mirror; ownership, access authority and the
//! damage increments of `repairableShip/common/methods.py:CalDamageLife` are
//! caller concerns.

use std::collections::BTreeMap;
use crate::{dao::repairable_ship::ItemId, db::{self, executor::Executor}};
struct ServiceLifeRow {
    item: ItemId,
    damage: f64,
}
impl ServiceLifeRow {
    fn from_row(row: &tokio_postgres::Row) -> Result<Self, db::Error> {
        let raw: i64 = row.get("item_id");
        Ok(Self {
            item: ItemId::new(raw)
                .ok_or_else(|| db::Error::Progress(anyhow::anyhow!("service-life row {raw} is not a physical item")))?,
            damage: row.get("damage"),
        })
    }
}

/// Present damage rows for the requested items; missing rows mean pristine.
pub(crate) async fn get_many(ex: &(impl Executor + ?Sized), ids: &[ItemId]) -> Result<BTreeMap<ItemId, f64>, db::Error> {
    let raw: Vec<i64> = ids.iter().map(|id| id.get()).collect();
    ex.query("SELECT item_id, damage FROM game.item_service_life WHERE item_id = ANY($1)", &[&raw]).await?
        .iter()
        .map(|row| ServiceLifeRow::from_row(row).map(|row| (row.item, row.damage)))
        .collect()
}

/// Guarded service-life write: `expected` `None` requires the row to be absent
/// (pristine), `Some` requires the stored damage to match exactly. Returns
/// whether the write landed; the caller turns a miss into a retry or refusal.
pub(crate) async fn set_if(ex: &(impl Executor + ?Sized), id: ItemId, expected: Option<f64>, damage: f64) -> Result<bool, db::Error> {
    match expected {
        Some(current) => Ok(ex.execute(
            "UPDATE game.item_service_life SET damage = $3 WHERE item_id = $1 AND damage = $2",
            &[&id.get(), &current, &damage],
        ).await? == 1),
        None => Ok(ex.execute(
            "INSERT INTO game.item_service_life (item_id, damage) VALUES ($1, $2) ON CONFLICT (item_id) DO NOTHING",
            &[&id.get(), &damage],
        ).await? == 1),
    }
}

#[cfg(all(test, feature = "tests-storage"))] mod tests;
