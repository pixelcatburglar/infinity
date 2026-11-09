//! Guards durable victim state and locks item rows for combat loss settlement.
//! Uses game.characters, game.items, game.item_attributes and game.module_charges.
//! There is no direct client wire owner; combat services own the loss policy,
//! and live damage contributions are not persisted by this DAO.
use crate::db::{self, executor::Executor};

/// Read the shallow loss set before classifying and locking item rows.
pub(crate) async fn item_candidates(ex: &(impl Executor + ?Sized), ship: i64) -> Result<Vec<i64>, db::Error> {
    Ok(ex.query("SELECT item_id FROM game.items WHERE location_id=$1 ORDER BY item_id", &[&ship]).await?
        .iter().map(|row| row.get("item_id")).collect())
}

/// Lock non-item loss state for the caller-revalidated, write-locked item set.
pub(crate) async fn lock_related(ex: &(impl Executor + ?Sized), ship: i64, children: &[i64], locks: &crate::dao::items::parent_locks::LockedItems<'_>) -> Result<(), db::Error> {
    if !locks.is_write_locked(ship) || children.iter().any(|id| !locks.is_write_locked(*id)) {
        return Err(db::Error::InvalidItemLockRequest("combat item set is not write locked"));
    }
    ex.query("SELECT item_id, attribute_id FROM game.item_attributes WHERE item_id=$1 OR item_id = ANY($2) ORDER BY item_id, attribute_id FOR UPDATE", &[&ship, &children]).await?;
    ex.query("SELECT ship_id, flag_id FROM game.module_charges WHERE ship_id=$1 ORDER BY ship_id, flag_id FOR UPDATE", &[&ship]).await?;
    Ok(())
}


pub(crate) async fn victim_current(ex: &(impl Executor + ?Sized), character: i64, ship: i64, system: i64) -> Result<bool, db::Error> {
    Ok(ex.query_opt("SELECT 1 FROM game.characters c JOIN game.items i ON i.item_id=c.ship_id WHERE c.character_id=$1 AND c.ship_id=$2 AND c.solar_system_id=$3 AND c.station_id IS NULL AND i.owner_id=$1 AND i.location_id=$3 AND i.singleton AND i.jump_clone_id IS NULL", &[&character, &ship, &system]).await?.is_some())
}
