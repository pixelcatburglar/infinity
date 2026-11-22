//! Item-ID block reservation, the take/salvage operation fence and the
//! taken-slice insert for transient NPC loot in game tables.
//! One reserve statement draws a block of real `game.items_item_id_seq`
//! values; ordinary inserts and trade reservations share the same sequence,
//! so the returned set is owned but not numerically contiguous. The advisory
//! fence serializes one operation's commit with its later proof inside the
//! same transaction; it is not a general lock service. The taken slice is the
//! only inventory row a successful pickup stages before `move_core` delivers
//! it to the real destination.
//! Policy, transactions, refusals, notifications and World are caller concerns.

use crate::db::{executor::Executor, Error};
use super::loading::GameItem;

/// Reserve `count` real sequence IDs in one statement, returned in the order
/// PostgreSQL issued them. Callers consume the committed list ascending and
/// must never synthesize a contiguous `[min, max]` range from it.
pub(crate) async fn reserve_ids(ex: &(impl Executor + ?Sized), count: i64) -> Result<Vec<i64>, Error> {
    let rows = ex.query("SELECT nextval('game.items_item_id_seq') AS item_id FROM generate_series(1, $1::bigint)", &[&count]).await?;
    Ok(rows.iter().map(|row| row.get("item_id")).collect())
}

/// Transaction-scoped advisory fence for one take/salvage attempt and its
/// commit proof. `key` is the caller's fixed business namespace plus the
/// frozen source key; hash collisions only add wait, never wrong ownership.
pub(crate) async fn fence(ex: &(impl Executor + ?Sized), key: &str) -> Result<(), Error> {
    ex.query("SELECT pg_advisory_xact_lock(hashtextextended($1::text, 0))", &[&key]).await?;
    Ok(())
}

/// Insert the already reserved taken slice with every column explicit and
/// read the stored row back. A conflicting pre-owned ID is a consistency
/// error, never an `ON CONFLICT` skip; idempotency is proven through the
/// operation's progress source before this statement runs.
pub(crate) async fn insert_taken_slice(ex: &(impl Executor + ?Sized), item: &GameItem) -> Result<GameItem, Error> {
    let row = ex.query_one(
        "INSERT INTO game.items (item_id, type_id, owner_id, location_id, flag_id, quantity, singleton, custom_info) \
         VALUES ($1, $2, $3, $4, $5, $6, false, $7) \
         RETURNING item_id, type_id, owner_id, location_id, flag_id, quantity, singleton, custom_info",
        &[&item.item_id, &item.type_id, &item.owner_id, &item.location_id, &item.flag_id, &item.quantity, &item.custom_info]).await?;
    Ok(GameItem::from_row(&row))
}

#[cfg(all(test, feature = "tests-storage"))]
mod tests;
