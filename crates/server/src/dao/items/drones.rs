//! Persistent drone inventory identities in `game.items` and `game.item_attributes`,
//! plus the real owner's NPC target-ignore flag from `game.characters`.
//! The client uses these identities for space drones; services own its wire contract.
//! Transactions, eligibility, notifications and world state are caller concerns.
//! Mining yield is not stored as child inventory.

use std::collections::BTreeMap;
use infinity_core::consts::{FLAG_DRONE_BAY, OWNER_UNOWNED};
use crate::db::{self, executor::Executor};
use super::{creation, loading::Item, quantities::{self, QuantityChange}};
use tokio_postgres::Row;

pub(crate) struct SpaceDrone {
    pub item_id: i64,
    pub type_id: i32,
    pub owner_id: i64,
    pub pos: [f64; 3],
    /// Real owner's flag; false for unowned or missing/deleted owner rows.
    pub npc_ignored: bool,
}

impl SpaceDrone {
    fn from_row(row: &Row) -> Self {
        Self { item_id: row.get("item_id"), type_id: row.get("type_id"),
            owner_id: row.get("owner_id"),
            pos: [row.get("x"), row.get("y"), row.get("z")],
            npc_ignored: row.get("npc_ignored") }
    }
}

pub(crate) async fn positioned(ex: &(impl Executor + ?Sized), system: i64, drone_types: &[i32]) -> Result<Vec<SpaceDrone>, db::Error> {
    let rows = ex.query("SELECT i.item_id, i.type_id, i.owner_id, i.x, i.y, i.z, COALESCE(c.gm_npc_ignored, false) AS npc_ignored FROM game.items i LEFT JOIN game.characters c ON c.character_id = i.owner_id AND NOT c.deleted WHERE i.location_id = $1 AND i.type_id = ANY($2) AND i.flag_id = 0 AND i.singleton AND i.quantity = 1 AND i.x IS NOT NULL AND i.y IS NOT NULL AND i.z IS NOT NULL ORDER BY i.item_id", &[&system, &drone_types]).await?;
    Ok(rows.iter().map(SpaceDrone::from_row).collect())
}

pub(crate) async fn launch(ex: &(impl Executor + ?Sized), item: &Item, count: i32, owner: i64, ship: i64, system: i64, pos: [f64; 3]) -> Result<Option<Vec<i64>>, db::Error> {
    if count <= 0 || i64::from(count) > item.quantity || item.owner_id != owner || item.location_id != ship || item.flag_id != FLAG_DRONE_BAY || (item.singleton && (item.quantity != 1 || count != 1)) {
        return Ok(None);
    }
    let locked = ex.query_opt("SELECT item_id FROM game.items WHERE item_id = $1 AND type_id = $2 AND owner_id = $3 AND location_id = $4 AND flag_id = $5 AND quantity = $6 AND singleton = $7 FOR UPDATE", &[&item.item_id, &item.type_id, &owner, &ship, &FLAG_DRONE_BAY, &item.quantity, &item.singleton]).await?;
    if locked.is_none() { return Ok(None); }

    let move_original = i64::from(count) == item.quantity;
    let remaining = if move_original { 1 } else { item.quantity - i64::from(count) };
    if remaining != item.quantity {
        let changes = BTreeMap::from([(item.item_id, QuantityChange::new(item, Some(remaining)))]);
        if !quantities::apply(ex, &changes).await? { return Ok(None); }
    }
    let mut ids = Vec::new();
    if move_original {
        let changed = ex.execute("UPDATE game.items SET location_id = $2, flag_id = 0, singleton = true, x = $3, y = $4, z = $5 WHERE item_id = $1 AND type_id = $6 AND owner_id = $7 AND location_id = $8 AND flag_id = $9 AND quantity = 1 AND singleton = $10", &[&item.item_id, &system, &pos[0], &pos[1], &pos[2], &item.type_id, &owner, &ship, &FLAG_DRONE_BAY, &item.singleton]).await?;
        if changed == 0 { return Ok(None); }
        ids.push(item.item_id);
    }
    for _ in 0..(count - i32::from(move_original)) {
        ids.push(creation::positioned(ex, item.type_id, owner, system, pos).await?);
    }
    Ok(Some(ids))
}

pub(crate) async fn return_to_bay(ex: &(impl Executor + ?Sized), id: i64, expected_owner: i64, system: i64, new_owner: i64, ship: i64) -> Result<bool, db::Error> {
    Ok(ex.execute("UPDATE game.items SET owner_id = $4, location_id = $5, flag_id = $6, x = NULL, y = NULL, z = NULL WHERE item_id = $1 AND owner_id = $2 AND location_id = $3 AND flag_id = 0 AND singleton AND quantity = 1 AND x IS NOT NULL AND y IS NOT NULL AND z IS NOT NULL", &[&id, &expected_owner, &system, &new_owner, &ship, &FLAG_DRONE_BAY]).await? != 0)
}

pub(crate) async fn abandon(ex: &(impl Executor + ?Sized), id: i64, owner: i64, system: i64) -> Result<bool, db::Error> {
    Ok(ex.execute("UPDATE game.items SET owner_id = $4 WHERE item_id = $1 AND owner_id = $2 AND location_id = $3 AND flag_id = 0 AND singleton AND quantity = 1 AND x IS NOT NULL AND y IS NOT NULL AND z IS NOT NULL", &[&id, &owner, &system, &OWNER_UNOWNED]).await? != 0)
}

pub(crate) async fn claim_lost(ex: &(impl Executor + ?Sized), id: i64, type_id: i32, owner: i64, system: i64) -> Result<bool, db::Error> {
    Ok(ex.query_opt("SELECT item_id FROM game.items WHERE item_id = $1 AND type_id = $2 AND owner_id = $3 AND location_id = $4 AND flag_id = 0 AND singleton AND quantity = 1 AND x > '-Infinity'::float8 AND x < 'Infinity'::float8 AND y > '-Infinity'::float8 AND y < 'Infinity'::float8 AND z > '-Infinity'::float8 AND z < 'Infinity'::float8 AND jump_clone_id IS NULL AND brain_slot IS NULL AND brain_activated_at IS NULL AND brain_expires_at IS NULL AND brain_effect_ids IS NULL FOR UPDATE", &[&id, &type_id, &owner, &system]).await?.is_some())
}

#[cfg(all(test, feature = "tests-storage"))] mod tests;
