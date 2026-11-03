//! Station delivery asset locations and shared item ancestry from `game.items`.
//! corpmgr consumes location and count rows with station metadata from the SDE;
//! custody and inventory movement consume ancestry. These queries do not authorize
//! access or include structure delivery locations.

use crate::db::{executor::Executor, Error};
use crate::consts::FLAG_CORP_DELIVERIES;

pub(crate) struct DeliveryLocation {
    pub location_id: i64,
    pub item_count: i64,
}

impl DeliveryLocation {
    fn from_row(row: &tokio_postgres::Row) -> Self {
        Self { location_id: row.get("location_id"), item_count: row.get("item_count") }
    }
}

pub(crate) async fn locations(ex: &(impl Executor + ?Sized), corp_id: i64) -> Result<Vec<DeliveryLocation>, Error> {
    Ok(ex.query("SELECT location_id, count(*) AS item_count FROM game.items WHERE owner_id = $1 AND flag_id = $2 GROUP BY location_id ORDER BY location_id", &[&corp_id, &FLAG_CORP_DELIVERIES]).await?.iter().map(DeliveryLocation::from_row).collect())
}

/// Item ancestry, with UNION ensuring corrupt cycles terminate.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Ancestor {
    pub item_id: i64,
    pub type_id: i32,
    pub owner_id: i64,
    pub location_id: i64,
    pub flag_id: i32,
    pub singleton: bool,
}

impl Ancestor {
    fn from_row(row: &tokio_postgres::Row) -> Self {
        Self { item_id: row.get("item_id"), type_id: row.get("type_id"), owner_id: row.get("owner_id"), location_id: row.get("location_id"), flag_id: row.get("flag_id"), singleton: row.get("singleton") }
    }
}

pub(crate) async fn ancestry(ex: &(impl Executor + ?Sized), id: i64) -> Result<Vec<Ancestor>, Error> {
    Ok(ex.query("WITH RECURSIVE ancestors AS (SELECT item_id, type_id, owner_id, location_id, flag_id, singleton FROM game.items WHERE item_id = $1 AND jump_clone_id IS NULL UNION SELECT i.item_id, i.type_id, i.owner_id, i.location_id, i.flag_id, i.singleton FROM game.items i JOIN ancestors a ON i.item_id = a.location_id WHERE i.jump_clone_id IS NULL) SELECT item_id, type_id, owner_id, location_id, flag_id, singleton FROM ancestors ORDER BY item_id", &[&id]).await?.iter().map(Ancestor::from_row).collect())
}
