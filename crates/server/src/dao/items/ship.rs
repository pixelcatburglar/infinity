//! Ship item persistence in game.items and game.module_charges.
//! Ship services own movement, transactions and notifications; no client wire contract lives here.
//! Shared item type lookup and positioned creation live in `items::inventory` and `items::creation`.
//! Reload start, completion, return, recovery and ship loss lock the ship item row
//! as their first transaction statement, before acquiring child rows in their usual order.
use crate::db::{self, executor::Executor};
use tokio_postgres::Row;
use super::{charges::Charge, combat, inventory};
struct Hull { type_id: i32 }
impl Hull { fn from_row(row: &Row) -> Self { Self { type_id: row.get("type_id") } } }
pub(crate) async fn lock_owned_at(ex: &(impl Executor + ?Sized), ship: i64, owner: i64, location: i64, flag: i32) -> Result<Option<i32>, db::Error> {
    Ok(ex.query_opt("SELECT type_id FROM game.items WHERE item_id=$1 AND owner_id=$2 AND location_id=$3 AND flag_id=$4 AND singleton AND quantity=1 AND jump_clone_id IS NULL FOR UPDATE", &[&ship, &owner, &location, &flag]).await?.as_ref().map(|row| Hull::from_row(row).type_id))
}
pub(crate) async fn lock_reload_ship(ex:&(impl Executor+?Sized), ship:i64)->Result<bool,db::Error> {
    Ok(ex.query_opt("SELECT item_id FROM game.items WHERE item_id = $1 FOR UPDATE", &[&ship]).await?.is_some())
}
pub(crate) async fn undock(ex:&(impl Executor+?Sized), ship:i64, system:i64, pos:[f64;3])->Result<u64,db::Error> { ex.execute("UPDATE game.items SET location_id = $2, flag_id = 0, x = $3, y = $4, z = $5 WHERE item_id = $1", &[&ship,&system,&pos[0],&pos[1],&pos[2]]).await }
pub(crate) async fn dock(ex:&(impl Executor+?Sized), ship:i64, station:i64, flag:i32)->Result<u64,db::Error> { ex.execute("UPDATE game.items SET location_id = $2, flag_id = $3, x = NULL, y = NULL, z = NULL WHERE item_id = $1", &[&ship,&station,&flag]).await }
pub(crate) async fn position(ex:&(impl Executor+?Sized), ship:i64, system:i64, pos:[f64;3])->Result<u64,db::Error> { ex.execute("UPDATE game.items SET location_id = $2, x = $3, y = $4, z = $5 WHERE item_id = $1", &[&ship,&system,&pos[0],&pos[1],&pos[2]]).await }
pub(crate) struct LostItem { pub type_id:i32, pub flag_id:i32, pub singleton:bool, pub quantity:i64 }
impl LostItem { fn from_row(r:&Row)->Self { Self { type_id:r.get("type_id"), flag_id:r.get("flag_id"), singleton:r.get("singleton"), quantity:r.get("quantity") } } }
pub(crate) async fn lost_items(ex:&(impl Executor+?Sized), ship:i64)->Result<Vec<LostItem>,db::Error> { Ok(ex.query("SELECT type_id, flag_id, singleton, quantity FROM game.items WHERE location_id = $1", &[&ship]).await?.iter().map(LostItem::from_row).collect()) }
pub(crate) async fn lost_charges(ex:&(impl Executor+?Sized), ship:i64)->Result<Vec<Charge>,db::Error> { Ok(ex.query("SELECT type_id, flag_id, quantity FROM game.module_charges WHERE ship_id = $1", &[&ship]).await?.iter().map(Charge::from_row).collect()) }
pub(crate) async fn delete_lost(ex:&(impl Executor+?Sized), ship:i64)->Result<(),db::Error> {
    ex.execute("DELETE FROM game.items WHERE location_id = $1", &[&ship]).await?;
    ex.execute("DELETE FROM game.module_charges WHERE ship_id = $1", &[&ship]).await?;
    combat::clear_banks(ex, ship).await?;
    inventory::delete(ex, ship).await?;
    Ok(())
}
