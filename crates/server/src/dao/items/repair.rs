//! Station repair inventory locking. Uses game.items; repair policy is in repairSvc.
//! No client wire values or transaction management are implemented here.
use crate::db::{executor::Executor, Error};
use tokio_postgres::Row;

pub(crate) struct Item {
    pub item_id: i64,
    pub type_id: i32,
    pub owner_id: i64,
    pub location_id: i64,
    pub flag_id: i32,
    pub singleton: bool,
}
impl Item {
    fn from_row(row: &Row) -> Self {
        Self { item_id: row.get("item_id"), type_id: row.get("type_id"), owner_id: row.get("owner_id"), location_id: row.get("location_id"), flag_id: row.get("flag_id"), singleton: row.get("singleton") }
    }
}
/// Read requested items, their containers, and directly fitted children without locking.
pub(crate) async fn candidates(ex: &(impl Executor + ?Sized), ids: &[i64], owner: i64) -> Result<Vec<Item>, Error> {
    let rows = ex.query("SELECT item_id, type_id, owner_id, location_id, flag_id, singleton FROM game.items WHERE owner_id = $2 AND jump_clone_id IS NULL AND (item_id = ANY($1) OR location_id = ANY($1) OR item_id IN (SELECT location_id FROM game.items WHERE item_id = ANY($1) AND owner_id = $2 AND jump_clone_id IS NULL)) ORDER BY item_id", &[&ids, &owner]).await?;
    Ok(rows.iter().map(Item::from_row).collect())
}
