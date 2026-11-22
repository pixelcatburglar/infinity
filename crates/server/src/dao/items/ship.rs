//! Ship item persistence in game.items, game.module_charges, game.weapon_banks,
//! game.pending_reloads, game.ship_stances, game.insurance and game.blueprints.
//! Shared item type lookup and positioned creation live in `items::inventory` and `items::creation`.
//! Reload start, completion, return and recovery lock the ship item row as their
//! first transaction statement, before acquiring child rows in their usual order.
//! Ship loss instead fences its entire contents closure through the shared item
//! lock order, hulls before other items; `delete_lost` cleans combat attachments
//! for the whole closure and deletes only non-retained physical and blueprint rows.
//! A validated frigate escape-bay survivor is relocated out of the lost hull
//! under the same retained token before deletion, so the recomputed closure
//! leaves it; guarded relocation failures roll back the whole loss.
use infinity_core::consts::FLAG_FRIGATE_ESCAPE_BAY;
use crate::db::{self, executor::Executor};
use tokio_postgres::Row;
use super::{charges::Charge, inventory};
struct Hull { type_id: i32 }
impl Hull { fn from_row(row: &Row) -> Self { Self { type_id: row.get("type_id") } } }
pub(crate) async fn lock_owned_at(ex: &(impl Executor + ?Sized), ship: i64, owner: i64, location: i64, flag: i32) -> Result<Option<i32>, db::Error> {
    Ok(ex.query_opt("SELECT type_id FROM game.items WHERE item_id=$1 AND owner_id=$2 AND location_id=$3 AND flag_id=$4 AND singleton AND quantity=1 AND jump_clone_id IS NULL FOR UPDATE", &[&ship, &owner, &location, &flag]).await?.as_ref().map(|row| Hull::from_row(row).type_id))
}
pub(crate) async fn lock_reload_ship(ex:&(impl Executor+?Sized), ship:i64)->Result<bool,db::Error> {
    Ok(ex.query_opt("SELECT item_id FROM game.items WHERE item_id = $1 FOR UPDATE", &[&ship]).await?.is_some())
}
pub(crate) async fn undock(ex:&(impl Executor+?Sized), ship:i64, system:i64, pos:[f64;3])->Result<u64,db::Error> { ex.execute("UPDATE game.items SET location_id = $2, flag_id = 0, x = $3, y = $4, z = $5 WHERE item_id = $1", &[&ship,&system,&pos[0],&pos[1],&pos[2]]).await }
pub(crate) async fn position(ex:&(impl Executor+?Sized), ship:i64, system:i64, pos:[f64;3])->Result<u64,db::Error> { ex.execute("UPDATE game.items SET location_id = $2, x = $3, y = $4, z = $5 WHERE item_id = $1", &[&ship,&system,&pos[0],&pos[1],&pos[2]]).await }
/// Relocate a validated frigate escape-bay survivor out of its lost host to
/// the first-lethal position. The caller holds UPDATE locks on the host and
/// candidate rows through `locks`; the write stays guarded by the candidate's
/// original custody rather than an unconditional undock.
pub(crate) async fn extract_escape_ship(
    ex: &(impl Executor + ?Sized), locks: &super::parent_locks::LockedItems<'_>,
    host: i64, candidate: i64, owner: i64, type_id: i32, system: i64, position: [f64; 3],
) -> Result<bool, db::Error> {
    if !locks.is_write_locked(host) || !locks.is_write_locked(candidate) {
        return Err(db::Error::InvalidItemLockRequest("escape extraction requires update-locked host and candidate"));
    }
    if !position.iter().all(|v| v.is_finite()) { return Ok(false); }
    Ok(ex.execute("UPDATE game.items SET location_id = $3, flag_id = 0, x = $4, y = $5, z = $6 WHERE item_id = $1 AND location_id = $2 AND flag_id = $7 AND type_id = $8 AND owner_id = $9 AND singleton AND quantity = 1 AND jump_clone_id IS NULL", &[&candidate, &host, &system, &position[0], &position[1], &position[2], &FLAG_FRIGATE_ESCAPE_BAY, &type_id, &owner]).await? == 1)
}
pub(crate) struct LostItem { pub type_id:i32, pub flag_id:i32, pub singleton:bool, pub quantity:i64 }
impl LostItem { fn from_row(r:&Row)->Self { Self { type_id:r.get("type_id"), flag_id:r.get("flag_id"), singleton:r.get("singleton"), quantity:r.get("quantity") } } }
pub(crate) async fn lost_items(ex:&(impl Executor+?Sized), ship:i64)->Result<Vec<LostItem>,db::Error> { Ok(ex.query("SELECT type_id, flag_id, singleton, quantity FROM game.items WHERE location_id = $1", &[&ship]).await?.iter().map(LostItem::from_row).collect()) }
pub(crate) async fn lost_charges(ex:&(impl Executor+?Sized), ship:i64)->Result<Vec<Charge>,db::Error> { Ok(ex.query("SELECT type_id, flag_id, quantity FROM game.module_charges WHERE ship_id = $1", &[&ship]).await?.iter().map(Charge::from_row).collect()) }
/// Delete the current loss closure except the strictly ordered retained subset.
/// Combat attachments are cleaned for the whole closure before custody moves.
pub(crate) async fn delete_lost(ex:&(impl Executor+?Sized), ship:i64, retained:&[i64])->Result<(),db::Error> {
    let closure = inventory::descendants(ex, &[ship]).await?;
    if retained.windows(2).any(|pair| pair[0] >= pair[1])
        || retained.iter().any(|id| !closure.contains(id)) {
        return Err(db::Error::StaleSnapshot("invalid retained loss items"));
    }
    if closure.is_empty() { return Ok(()); }
    let reservations = super::pending_reloads::intersecting(ex, &closure).await?;
    let ids: Vec<i64> = reservations.iter().map(|row| row.reload_id).collect();
    if super::pending_reloads::lock_ids(ex, &ids).await? != reservations {
        return Err(db::Error::StaleSnapshot("loss snapshot changed"));
    }
    for row in &reservations { super::pending_reloads::take(ex, row.reload_id).await?; }
    ex.execute("DELETE FROM game.module_charges WHERE ship_id = ANY($1)", &[&closure]).await?;
    ex.execute("DELETE FROM game.weapon_banks WHERE ship_id = ANY($1) OR master_id = ANY($1) OR slave_id = ANY($1)", &[&closure]).await?;
    // A retained repairable hull loses its mode item with the closure; drop
    // its stance timing too; the items FK only cascades for deleted hulls.
    super::ship_stance::clear(ex, &closure).await?;
    for ship_id in &closure { crate::dao::insurance::take_payout(ex, *ship_id).await?; }
    let destroyed: Vec<_> = closure.iter().copied().filter(|id| retained.binary_search(id).is_err()).collect();
    crate::dao::industry::delete_blueprint_state(ex, &destroyed).await?;
    ex.execute("DELETE FROM game.items WHERE item_id = ANY($1)", &[&destroyed]).await?;
    Ok(())
}

#[cfg(all(test, feature = "tests-storage"))] mod tests;
