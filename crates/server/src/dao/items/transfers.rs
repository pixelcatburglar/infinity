//! Shared item transfer and stack operations using `game.items` and `game.characters`.
//! This DAO has no direct client mirror or wire contract; owning services supply those.
//! Transfer policy, transactions, refusals, notifications and World are caller concerns.
use crate::db::{executor::Executor, Error};
use tokio_postgres::Row;
pub(crate) struct Stack { pub item_id: i64, pub quantity: i32 }
impl Stack { fn from_row(r: &Row) -> Self { Self { item_id: r.get("item_id"), quantity: r.get("quantity") } } }
struct ItemId { item_id: i64 }
impl ItemId { fn from_row(r: &Row) -> Self { Self { item_id: r.get("item_id") } } }
struct Station { station_id: Option<i64> }
impl Station { fn from_row(r: &Row) -> Self { Self { station_id: r.get("station_id") } } }
pub(crate) async fn take(ex: &(impl Executor + ?Sized), id: i64, quantity: i32) -> Result<bool, Error> { Ok(ex.execute("UPDATE game.items SET quantity = quantity - $2 WHERE item_id = $1 AND NOT singleton", &[&id, &quantity]).await? == 1) }
pub(crate) async fn stack(ex: &(impl Executor + ?Sized), location: i64, flag: i32, type_id: i32, owner: i64) -> Result<Option<Stack>, Error> { Ok(ex.query_opt("SELECT item_id, quantity FROM game.items WHERE location_id = $1 AND flag_id = $2 AND type_id = $3 AND owner_id = $4 AND NOT singleton LIMIT 1", &[&location, &flag, &type_id, &owner]).await?.as_ref().map(Stack::from_row)) }
pub(crate) async fn add(ex: &(impl Executor + ?Sized), id: i64, quantity: i32) -> Result<bool, Error> { Ok(ex.execute("UPDATE game.items SET quantity = quantity + $2 WHERE item_id = $1 AND NOT singleton", &[&id, &quantity]).await? == 1) }
pub(crate) async fn insert(ex: &(impl Executor + ?Sized), type_id: i32, owner: i64, location: i64, flag: i32, quantity: i32) -> Result<i64, Error> { let r = ex.query_one("INSERT INTO game.items (type_id, owner_id, location_id, flag_id, quantity) VALUES ($1, $2, $3, $4, $5) RETURNING item_id", &[&type_id, &owner, &location, &flag, &quantity]).await?; Ok(ItemId::from_row(&r).item_id) }
pub(crate) async fn docked(ex: &(impl Executor + ?Sized), owner: i64) -> Result<Option<i64>, Error> { Ok(ex.query_opt("SELECT station_id FROM game.characters WHERE character_id = $1", &[&owner]).await?.as_ref().and_then(|r| Station::from_row(r).station_id)) }
/// One root and the owner and location it receives; descendants keep their parent.
pub(crate) struct Transfer {
    pub(crate) item_id: i64,
    pub(crate) owner: i64,
    pub(crate) location: i64,
    pub(crate) flag: i32,
}

/// Acquire item locks in ID order, then hand over all roots and descendants.
pub(crate) async fn hand_over(ex: &(impl Executor + ?Sized), transfers: &[Transfer]) -> Result<(), Error> {
    let ids: Vec<_> = transfers.iter().map(|item| item.item_id).collect();
    let owners: Vec<_> = transfers.iter().map(|item| item.owner).collect();
    let locations: Vec<_> = transfers.iter().map(|item| item.location).collect();
    let flags: Vec<_> = transfers.iter().map(|item| item.flag).collect();
    let tree = "WITH RECURSIVE roots(root, owner, location, flag) AS (SELECT * FROM unnest($1::int8[], $2::int8[], $3::int8[], $4::int4[])), tree(root, id, owner, location, flag) AS (SELECT r.root, r.root, r.owner, r.location, r.flag FROM roots r JOIN game.items i ON i.item_id = r.root AND i.jump_clone_id IS NULL UNION SELECT t.root, i.item_id, t.owner, t.location, t.flag FROM tree t JOIN game.items i ON i.location_id = t.id AND i.jump_clone_id IS NULL)";
    ex.query(&format!("{tree} SELECT i.item_id FROM game.items i JOIN tree t ON t.id = i.item_id WHERE i.jump_clone_id IS NULL ORDER BY i.item_id FOR UPDATE OF i"), &[&ids, &owners, &locations, &flags]).await?;
    ex.execute(&format!("{tree} UPDATE game.items i SET owner_id = t.owner, location_id = CASE WHEN t.id = t.root THEN t.location ELSE i.location_id END, flag_id = CASE WHEN t.id = t.root THEN t.flag ELSE i.flag_id END FROM tree t WHERE i.item_id = t.id AND i.jump_clone_id IS NULL"), &[&ids, &owners, &locations, &flags]).await?;
    Ok(())
}

#[cfg(test)] mod tests;
