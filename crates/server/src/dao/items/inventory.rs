//! Inventory slots and item mutations using `game.items`.
//! This DAO has no direct client mirror or wire contract; owning services supply those.
//! Transactions, refusals, notifications and World are caller concerns.
use crate::db::{executor::Executor, Error};
use super::loading::Item;
use tokio_postgres::Row;

pub(crate) struct ItemId { pub item_id: i64 }
impl ItemId { pub(super) fn from_row(r: &Row) -> Self { Self { item_id: r.get("item_id") } } }
struct TypeId { type_id: i32 }
impl TypeId { fn from_row(r: &Row) -> Self { Self { type_id: r.get("type_id") } } }

/// Look up an item's type for ship and fit services.
pub(crate) async fn type_id(ex: &(impl Executor + ?Sized), item: i64) -> Result<Option<i32>, Error> {
    Ok(ex.query_opt("SELECT type_id FROM game.items WHERE item_id = $1", &[&item]).await?.as_ref().map(|r| TypeId::from_row(r).type_id))
}

pub(crate) async fn split(ex: &(impl Executor + ?Sized), item: &Item, quantity: i32, owner: i64, location: i64, flag: i32, singleton: bool) -> Result<Option<i64>, Error> {
    let affected = ex.execute("UPDATE game.items SET quantity = quantity - $2 WHERE item_id = $1 AND quantity = $3 AND owner_id = $4 AND location_id = $5 AND flag_id = $6 AND singleton = $7 AND NOT singleton AND quantity > $2 AND $2 > 0", &[&item.item_id, &quantity, &item.quantity, &item.owner_id, &item.location_id, &item.flag_id, &item.singleton]).await?;
    if affected == 0 { return Ok(None); }
    let row = ex.query_one("INSERT INTO game.items (type_id, owner_id, location_id, flag_id, quantity, singleton) VALUES ($1, $2, $3, $4, $5, $6) RETURNING item_id", &[&item.type_id, &owner, &location, &flag, &quantity, &singleton]).await?;
    Ok(Some(ItemId::from_row(&row).item_id))
}
pub(crate) async fn delete(ex: &(impl Executor + ?Sized), id: i64) -> Result<(), Error> { ex.execute("DELETE FROM game.items WHERE item_id = $1", &[&id]).await?; Ok(()) }
pub(crate) async fn move_item(ex: &(impl Executor + ?Sized), item: &Item, location: i64, flag: i32, owner: i64, singleton: bool) -> Result<bool, Error> {
    Ok(ex.execute("UPDATE game.items SET location_id = $2, flag_id = $3, owner_id = $4, singleton = $5,
                   x = NULL, y = NULL, z = NULL
                   WHERE item_id = $1 AND owner_id = $6 AND location_id = $7 AND flag_id = $8 AND quantity = $9 AND singleton = $10",
        &[&item.item_id, &location, &flag, &owner, &singleton, &item.owner_id, &item.location_id, &item.flag_id, &item.quantity, &item.singleton]).await? != 0)
}
pub(crate) async fn rename(ex: &(impl Executor + ?Sized), id: i64, name: &str, owner: i64) -> Result<(), Error> { ex.execute("UPDATE game.items SET name = $2 WHERE item_id = $1 AND owner_id = $3", &[&id, &name, &owner]).await?; Ok(()) }
