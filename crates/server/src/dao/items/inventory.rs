//! Inventory slots and item mutations using `game.items`.
//! This DAO has no direct client mirror or wire contract; owning services supply those.
//! Transactions, refusals, notifications and World are caller concerns.
//! Split stacks retain their source custom information and name.
use crate::db::{executor::Executor, Error};
use super::loading::Item;
use tokio_postgres::Row;

pub(crate) struct ItemId { pub item_id: i64 }
impl ItemId { pub(super) fn from_row(r: &Row) -> Self { Self { item_id: r.get("item_id") } } }
pub(crate) struct Matched { pub(crate) matched: bool }
impl Matched { pub(crate) fn from_row(r: &Row) -> Self { Self { matched: r.get("matched") } } }
struct TypeId { type_id: i32 }
impl TypeId { fn from_row(r: &Row) -> Self { Self { type_id: r.get("type_id") } } }
struct LockedItemId { item_id: i64 }
impl LockedItemId { fn from_row(r: &Row) -> Self { Self { item_id: r.get("item_id") } } }

async fn lock_item_ids(ex: &(impl Executor + ?Sized), ids: &[i64]) -> Result<Vec<LockedItemId>, Error> {
    Ok(ex.query("SELECT item_id FROM game.items WHERE item_id = ANY($1) AND jump_clone_id IS NULL ORDER BY item_id FOR UPDATE", &[&ids]).await?
        .iter().map(LockedItemId::from_row).collect())
}

/// Look up an item's type for ship and fit services.
pub(crate) async fn type_id(ex: &(impl Executor + ?Sized), item: i64) -> Result<Option<i32>, Error> {
    Ok(ex.query_opt("SELECT type_id FROM game.items WHERE item_id = $1 AND jump_clone_id IS NULL", &[&item]).await?.as_ref().map(|r| TypeId::from_row(r).type_id))
}

pub(crate) async fn split(ex: &(impl Executor + ?Sized), item: &Item, quantity: i32, owner: i64, location: i64, flag: i32, singleton: bool) -> Result<Option<i64>, Error> {
    let affected = ex.execute("UPDATE game.items SET quantity = quantity - $2 WHERE item_id = $1 AND quantity = $3 AND owner_id = $4 AND location_id = $5 AND flag_id = $6 AND singleton = $7 AND NOT singleton AND quantity > $2 AND $2 > 0 AND jump_clone_id IS NULL", &[&item.item_id, &quantity, &item.quantity, &item.owner_id, &item.location_id, &item.flag_id, &item.singleton]).await?;
    if affected == 0 { return Ok(None); }
    let row = ex.query_one("INSERT INTO game.items (type_id, owner_id, location_id, flag_id, quantity, singleton, custom_info, name) SELECT $1, $2, $3, $4, $5, $6, custom_info, name FROM game.items WHERE item_id = $7 RETURNING item_id", &[&item.type_id, &owner, &location, &flag, &quantity, &singleton, &item.item_id]).await?;
    Ok(Some(ItemId::from_row(&row).item_id))
}
pub(crate) async fn delete(ex: &(impl Executor + ?Sized), id: i64) -> Result<(), Error> { ex.execute("DELETE FROM game.items WHERE item_id = $1 AND jump_clone_id IS NULL", &[&id]).await?; Ok(()) }

pub(crate) async fn rename(ex: &(impl Executor + ?Sized), id: i64, name: &str, owner: i64, type_id: i32, location: i64, flag: i32, max_len: i32) -> Result<bool, Error> {
    Ok(ex.execute("UPDATE game.items SET name = $2 WHERE item_id = $1 AND owner_id = $3 AND type_id = $4 AND location_id = $5 AND flag_id = $6 AND jump_clone_id IS NULL AND char_length($2) <= $7", &[&id, &name, &owner, &type_id, &location, &flag, &max_len]).await? == 1)
}

pub(crate) async fn delete_guarded(ex: &(impl Executor + ?Sized), ids: &[i64]) -> Result<u64, Error> {
    let mut sorted = ids.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    let rows = lock_item_ids(ex, &sorted).await?;
    if rows.len() != sorted.len() { return Ok(0); }
    ex.execute("DELETE FROM game.items WHERE item_id = ANY($1) AND jump_clone_id IS NULL", &[&sorted]).await
}

/// Lock the complete descendant closure in primary-key order inside the caller's transaction.
pub(crate) async fn lock_descendants(ex: &(impl Executor + ?Sized), roots: &[i64]) -> Result<Vec<i64>, Error> {
    // Lock roots first, then re-resolve descendants and lock that snapshot in
    // primary-key order before the caller authorizes and deletes it.
    let root_rows = lock_item_ids(ex, roots).await?;
    if root_rows.len() != roots.iter().copied().collect::<std::collections::HashSet<_>>().len() {
        return Ok(Vec::new());
    }
    let closure: Vec<i64> = ex.query(
        "WITH RECURSIVE tree(id) AS (
           SELECT item_id FROM game.items WHERE item_id = ANY($1) AND jump_clone_id IS NULL
           UNION
           SELECT i.item_id FROM game.items i JOIN tree t ON i.location_id = t.id
           WHERE i.jump_clone_id IS NULL
         ) SELECT id FROM tree ORDER BY id",
        &[&roots],
    ).await?.iter().map(|row| row.get("id")).collect();
    Ok(lock_item_ids(ex, &closure).await?.into_iter().map(|row| row.item_id).collect())
}
pub(crate) async fn move_item(ex: &(impl Executor + ?Sized), item: &Item, location: i64, flag: i32, owner: i64, singleton: bool) -> Result<bool, Error> {
    let row = ex.query_one("WITH changed AS (
                   UPDATE game.items SET location_id = $2, flag_id = $3, owner_id = $4, singleton = $5,
                   x = NULL, y = NULL, z = NULL
                   WHERE item_id = $1 AND owner_id = $6 AND location_id = $7 AND flag_id = $8 AND quantity = $9 AND singleton = $10 AND jump_clone_id IS NULL
                   RETURNING item_id, owner_id, singleton
               ), removed AS (
                   DELETE FROM game.ship_skin_applications a USING changed c
                   WHERE a.ship_item_id = c.item_id AND (a.character_id <> c.owner_id OR NOT c.singleton)
               ) SELECT EXISTS (SELECT 1 FROM changed) AS matched",
        &[&item.item_id, &location, &flag, &owner, &singleton, &item.owner_id, &item.location_id, &item.flag_id, &item.quantity, &item.singleton]).await?;
    Ok(Matched::from_row(&row).matched)
}
