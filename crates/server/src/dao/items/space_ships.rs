//! Positioned, unselected item roots in `game.items` and owner affiliations in game data.
//! No client wire contract lives here; callers filter ship types using the SDE.
//! Transactions and lifecycle publication are caller concerns.
//! Only roots with finite coordinates are eligible for loading or claiming.
use crate::db::{self, executor::Executor};
use tokio_postgres::Row;

pub(crate) struct FreeShip {
    pub item_id: i64,
    pub type_id: i32,
    pub owner_id: i64,
    pub corporation_id: i64,
    pub alliance_id: i64,
    pub pos: [f64; 3],
}

impl FreeShip {
    fn from_row(row: &Row) -> Self {
        Self { item_id: row.get("item_id"), type_id: row.get("type_id"), owner_id: row.get("owner_id"),
            corporation_id: row.get("corporation_id"), alliance_id: row.get("alliance_id"),
            pos: [row.get("x"), row.get("y"), row.get("z")] }
    }
}

pub(crate) async fn unpiloted(ex: &(impl Executor + ?Sized), system: i64) -> Result<Vec<FreeShip>, db::Error> {
    Ok(ex.query("SELECT i.item_id, i.type_id, i.owner_id, COALESCE(c.corporation_id, 0)::bigint AS corporation_id, COALESCE(co.alliance_id, -1)::bigint AS alliance_id, i.x, i.y, i.z FROM game.items i LEFT JOIN game.characters c ON c.character_id = i.owner_id LEFT JOIN game.corporations co ON co.corporation_id = c.corporation_id WHERE i.location_id = $1 AND i.flag_id = 0 AND i.singleton AND i.quantity = 1 AND i.x > '-Infinity'::float8 AND i.x < 'Infinity'::float8 AND i.y > '-Infinity'::float8 AND i.y < 'Infinity'::float8 AND i.z > '-Infinity'::float8 AND i.z < 'Infinity'::float8 AND NOT EXISTS (SELECT 1 FROM game.characters selected WHERE selected.ship_id = i.item_id) ORDER BY i.item_id", &[&system]).await?.iter().map(FreeShip::from_row).collect())
}

/// Lock an exact unselected positioned root for a transaction that will consume it.
pub(crate) async fn claim_unpiloted(ex: &(impl Executor + ?Sized), id: i64, type_id: i32, owner: i64, system: i64) -> Result<bool, db::Error> {
    Ok(ex.query_opt("SELECT i.item_id FROM game.items i WHERE i.item_id = $1 AND i.type_id = $2 AND i.owner_id = $3 AND i.location_id = $4 AND i.flag_id = 0 AND i.singleton AND i.quantity = 1 AND i.x > '-Infinity'::float8 AND i.x < 'Infinity'::float8 AND i.y > '-Infinity'::float8 AND i.y < 'Infinity'::float8 AND i.z > '-Infinity'::float8 AND i.z < 'Infinity'::float8 AND NOT EXISTS (SELECT 1 FROM game.characters c WHERE c.ship_id = i.item_id) FOR UPDATE OF i", &[&id, &type_id, &owner, &system]).await?.is_some())
}

#[cfg(all(test, feature = "tests-storage"))] mod tests;
