//! Guards durable victim state and locks item rows for combat loss settlement.
//! Uses game.characters, game.items, game.item_attributes, game.ship_configuration,
//! game.ship_dirt, game.ship_skin_applications, game.dynamic_items,
//! game.dynamic_item_modifiers, game.module_charges, game.weapon_banks,
//! game.pending_reloads and game.insurance.
//! There is no direct client wire owner; combat services own the loss policy,
//! and live damage contributions are not persisted by this DAO.
use crate::db::{self, executor::Executor};

/// Lock loss attachments for the caller-revalidated, write-locked item closure.
/// `contents` is the root-inclusive active closure; the root's own attachments
/// are matched by the same predicates.
/// Locking follows the fixed family order below so every loss takes the same path.
pub(crate) async fn lock_related(ex: &(impl Executor + ?Sized), ship: i64, contents: &[i64], locks: &crate::dao::items::parent_locks::LockedItems<'_>) -> Result<(), db::Error> {
    if !locks.is_write_locked(ship) || contents.iter().any(|id| !locks.is_write_locked(*id)) {
        return Err(db::Error::InvalidItemLockRequest("combat item set is not write locked"));
    }
    ex.query("SELECT item_id, attribute_id FROM game.item_attributes WHERE item_id = ANY($1) ORDER BY item_id, attribute_id FOR UPDATE", &[&contents]).await?;
    ex.query("SELECT ship_item_id FROM game.ship_configuration WHERE ship_item_id = ANY($1) ORDER BY ship_item_id FOR UPDATE", &[&contents]).await?;
    ex.query("SELECT ship_item_id FROM game.ship_dirt WHERE ship_item_id = ANY($1) ORDER BY ship_item_id FOR UPDATE", &[&contents]).await?;
    ex.query("SELECT ship_item_id FROM game.ship_skin_applications WHERE ship_item_id = ANY($1) ORDER BY ship_item_id FOR UPDATE", &[&contents]).await?;
    ex.query("SELECT item_id FROM game.dynamic_items WHERE item_id = ANY($1) ORDER BY item_id FOR UPDATE", &[&contents]).await?;
    ex.query("SELECT item_id, attribute_id FROM game.dynamic_item_modifiers WHERE item_id = ANY($1) ORDER BY item_id, attribute_id FOR UPDATE", &[&contents]).await?;
    ex.query("SELECT ship_id, flag_id FROM game.module_charges WHERE ship_id = ANY($1) ORDER BY ship_id, flag_id FOR UPDATE", &[&contents]).await?;
    ex.query("SELECT slave_id FROM game.weapon_banks WHERE ship_id = ANY($1) OR master_id = ANY($1) OR slave_id = ANY($1) ORDER BY slave_id FOR UPDATE", &[&contents]).await?;
    ex.query("SELECT reload_id FROM game.pending_reloads WHERE ship_id = ANY($1) OR source_location_id = ANY($1) ORDER BY reload_id FOR UPDATE", &[&contents]).await?;
    ex.query("SELECT ship_id FROM game.insurance WHERE ship_id = ANY($1) ORDER BY ship_id FOR UPDATE", &[&contents]).await?;
    ex.query("SELECT item_id FROM game.blueprints WHERE item_id = ANY($1) ORDER BY item_id FOR UPDATE", &[&contents]).await?;
    Ok(())
}


pub(crate) async fn victim_current(ex: &(impl Executor + ?Sized), character: i64, ship: i64, system: i64) -> Result<bool, db::Error> {
    Ok(ex.query_opt("SELECT 1 FROM game.characters c JOIN game.items i ON i.item_id=c.ship_id WHERE c.character_id=$1 AND c.ship_id=$2 AND c.solar_system_id=$3 AND c.station_id IS NULL AND i.owner_id=$1 AND i.location_id=$3 AND i.singleton AND i.jump_clone_id IS NULL", &[&character, &ship, &system]).await?.is_some())
}
