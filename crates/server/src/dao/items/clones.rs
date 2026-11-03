//! Real implant custody in `game.items` for station jump clones.
//! Client behavior belongs to jumpCloneSvc; callers own transactions and locks.
use crate::consts::FLAG_IMPLANT;
use crate::db::{self, executor::Executor};
use super::{brain::BrainItem, dogma::AttributeOverride};

/// Dormant implants belonging to one owner's clone, ordered by item ID.
pub(crate) async fn list(ex: &(impl Executor + ?Sized), owner: i64, clone: i64) -> Result<Vec<BrainItem>, db::Error> {
    Ok(ex.query("SELECT item_id, type_id, flag_id, brain_slot, brain_activated_at, brain_expires_at, brain_effect_ids FROM game.items WHERE owner_id=$1 AND jump_clone_id=$2 ORDER BY item_id", &[&owner, &clone]).await?.iter().map(BrainItem::from_row).collect())
}
/// Lock all active and dormant implants for an owner in item ID order.
pub(crate) async fn lock(ex: &(impl Executor + ?Sized), owner: i64) -> Result<Vec<BrainItem>, db::Error> {
    Ok(ex.query("SELECT item_id, type_id, flag_id, brain_slot, brain_activated_at, brain_expires_at, brain_effect_ids FROM game.items WHERE owner_id=$1 AND location_id=$1 AND flag_id=$2 ORDER BY item_id FOR UPDATE", &[&owner, &FLAG_IMPLANT]).await?.iter().map(BrainItem::from_row).collect())
}
pub(crate) async fn park(ex: &(impl Executor + ?Sized), owner: i64, clone: i64, item: i64) -> Result<bool, db::Error> {
    Ok(ex.execute("UPDATE game.items SET jump_clone_id=$2 WHERE owner_id=$1 AND location_id=$1 AND item_id=$3 AND flag_id=$4 AND jump_clone_id IS NULL AND quantity=1 AND singleton AND brain_slot IS NOT NULL", &[&owner, &clone, &item, &FLAG_IMPLANT]).await? == 1)
}
pub(crate) async fn activate(ex: &(impl Executor + ?Sized), owner: i64, clone: i64, item: i64) -> Result<bool, db::Error> {
    Ok(ex.execute("UPDATE game.items SET jump_clone_id=NULL WHERE owner_id=$1 AND jump_clone_id=$2 AND item_id=$3", &[&owner, &clone, &item]).await? == 1)
}
pub(crate) async fn destroy(ex: &(impl Executor + ?Sized), owner: i64, clone: i64) -> Result<u64, db::Error> {
    ex.execute("DELETE FROM game.items WHERE owner_id=$1 AND jump_clone_id=$2", &[&owner, &clone]).await
}
/// Implant types across the active body and every dormant body for extraction checks.
pub(crate) async fn implant_types(ex: &(impl Executor + ?Sized), owner: i64) -> Result<Vec<i32>, db::Error> {
    Ok(ex.query("SELECT DISTINCT type_id FROM game.items WHERE owner_id=$1 AND location_id=$1 AND flag_id=$2 ORDER BY type_id", &[&owner, &FLAG_IMPLANT]).await?.iter().map(|r| r.get("type_id")).collect())
}
pub(crate) async fn overrides(ex: &(impl Executor + ?Sized), owner: i64, clone: i64, ids: &[i64]) -> Result<Vec<AttributeOverride>, db::Error> {
    Ok(ex.query("SELECT a.item_id, a.attribute_id, a.value, a.updated_at FROM game.item_attributes a JOIN game.items i ON i.item_id=a.item_id WHERE i.owner_id=$1 AND i.jump_clone_id=$2 AND i.item_id=ANY($3) ORDER BY a.item_id, a.attribute_id", &[&owner, &clone, &ids]).await?.iter().map(AttributeOverride::from_row).collect())
}
/// Delete only the owner's currently boarded capsule.
pub(crate) async fn delete_capsule(ex: &(impl Executor + ?Sized), owner: i64, ship: i64, capsule_type: i32) -> Result<u64, db::Error> {
    ex.execute("DELETE FROM game.items i WHERE i.item_id=$2 AND i.owner_id=$1 AND i.type_id=$3 AND i.jump_clone_id IS NULL AND EXISTS (SELECT 1 FROM game.characters c WHERE c.character_id=$1 AND c.ship_id=i.item_id)", &[&owner, &ship, &capsule_type]).await
}

#[cfg(test)] mod tests;
