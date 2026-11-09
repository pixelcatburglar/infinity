//! Shared item transfer and stack operations using `game.items` and `game.characters`.
//! This DAO has no direct client mirror or wire contract; owning services supply those.
//! Transfer policy, transactions, refusals, notifications and World are caller concerns.
//! Market delivery uses returned item metadata for the actual inserted or merged stack;
//! market custody authorization and structure destinations are not implemented here.
use crate::db::{executor::Executor, Error};
use super::parent_locks::{LockedItems, Parent};
use tokio_postgres::Row;
struct ItemId { item_id: i64 }
impl ItemId { fn from_row(r: &Row) -> Self { Self { item_id: r.get("item_id") } } }
struct Station { station_id: Option<i64> }
impl Station { fn from_row(r: &Row) -> Self { Self { station_id: r.get("station_id") } } }
pub(crate) async fn insert(ex: &(impl Executor + ?Sized), type_id: i32, owner: i64, location: i64, flag: i32, quantity: i64) -> Result<i64, Error> { let r = ex.query_one("INSERT INTO game.items (type_id, owner_id, location_id, flag_id, quantity) VALUES ($1, $2, $3, $4, $5) RETURNING item_id", &[&type_id, &owner, &location, &flag, &quantity]).await?; Ok(ItemId::from_row(&r).item_id) }
pub(crate) async fn docked(ex: &(impl Executor + ?Sized), owner: i64) -> Result<Option<i64>, Error> { Ok(ex.query_opt("SELECT station_id FROM game.characters WHERE character_id = $1", &[&owner]).await?.as_ref().and_then(|r| Station::from_row(r).station_id)) }

/// Select the first matching stack before the transaction acquires item locks.
pub(crate) async fn deposit_candidate(ex: &(impl Executor + ?Sized), location: i64, flag: i32, type_id: i32, owner: i64) -> Result<Option<Parent>, Error> {
    Ok(ex.query_opt(
        "SELECT item_id, type_id, owner_id, location_id, flag_id, quantity, singleton, jump_clone_id FROM game.items WHERE location_id = $1 AND flag_id = $2 AND type_id = $3 AND owner_id = $4 AND NOT singleton AND quantity >= 0 AND quantity < $5 ORDER BY item_id LIMIT 1",
        &[&location, &flag, &type_id, &owner, &infinity_core::consts::MAX_STACK_QUANTITY],
    ).await?.as_ref().map(Parent::from_row))
}

/// A physical row changed by a deposit.
pub(crate) struct DepositedStack {
    pub item: super::loading::GameItem,
    pub old_quantity: Option<i64>,
    pub added_quantity: i64,
    pub visible: bool,
}
pub(crate) struct DepositStack {
    pub merged: Option<DepositedStack>,
    pub created: Option<DepositedStack>,
}
pub(crate) enum DepositCandidate<'a> {
    Prepared(Option<(&'a Parent, i64)>),
    Created(&'a Parent),
    InStatement,
}
/// A stale chosen candidate creates a new stack rather than selecting another row.
pub(crate) async fn deposit_stack(ex: &(impl Executor + ?Sized), location: i64, flag: i32, type_id: i32, owner: i64, quantity: i64, source: DepositCandidate<'_>, locks: Option<&LockedItems<'_>>) -> Result<DepositStack, Error> {
    let (candidate, in_statement, allow_insert) = match source {
        DepositCandidate::Prepared(candidate) => {
            let locks = locks.ok_or(Error::InvalidItemLockRequest("deposit candidate requires locks"))?;
            if candidate.is_some_and(|(item, _)| !locks.is_write_locked(item.item_id)) {
                return Err(Error::InvalidItemLockRequest("deposit candidate is not write locked"));
            }
            (candidate, false, true)
        }
        DepositCandidate::Created(item) => (Some((item, item.quantity)), false, false),
        DepositCandidate::InStatement => {
            if locks.is_some_and(|locks| !locks.is_write_locked(location)) {
                return Err(Error::InvalidItemLockRequest("deposit hull is not write locked"));
            }
            (None, true, true)
        }
    };
    let eligible = candidate.filter(|(item, _)| item.location_id == location && item.flag_id == flag && item.type_id == type_id && item.owner_id == owner && !item.singleton);
    let candidate_id = eligible.map(|(item, _)| item.item_id);
    let candidate_quantity = eligible.map(|(_, quantity)| quantity);
    let candidate_clone = eligible.and_then(|(item, _)| item.jump_clone_id);
    let rows = ex.query(
        "WITH selected AS MATERIALIZED (
             SELECT item_id, quantity FROM game.items
             WHERE $5::bigint > 0 AND location_id = $1 AND flag_id = $2 AND type_id = $3 AND owner_id = $4
               AND NOT singleton AND quantity >= 0
               AND (($9 AND quantity < $11) OR
                    (NOT $9 AND item_id = $6 AND quantity = $7 AND jump_clone_id IS NOT DISTINCT FROM $8))
             ORDER BY item_id LIMIT 1 FOR UPDATE
         ), updated AS (
             UPDATE game.items i SET quantity = i.quantity + LEAST($5, $11 - s.quantity)
             FROM selected s WHERE i.item_id = s.item_id AND s.quantity < $11
             RETURNING i.item_id, i.type_id, i.owner_id, i.location_id, i.flag_id, i.quantity, i.singleton, i.custom_info,
                       s.quantity AS old_quantity, LEAST($5, $11 - s.quantity) AS added_quantity, i.jump_clone_id IS NULL AS visible
         ), inserted AS (
             INSERT INTO game.items (type_id, owner_id, location_id, flag_id, quantity)
             SELECT $3, $4, $1, $2, $5 - COALESCE((SELECT added_quantity FROM updated), 0::bigint)
             WHERE $5 > 0 AND ($10 OR EXISTS (SELECT 1 FROM selected))
               AND $5 - COALESCE((SELECT added_quantity FROM updated), 0::bigint) > 0
             RETURNING item_id, type_id, owner_id, location_id, flag_id, quantity, singleton, custom_info,
                       NULL::bigint AS old_quantity, quantity AS added_quantity, jump_clone_id IS NULL AS visible
         )
         SELECT * FROM updated UNION ALL SELECT * FROM inserted",
        &[&location, &flag, &type_id, &owner, &quantity, &candidate_id, &candidate_quantity, &candidate_clone, &in_statement, &allow_insert, &infinity_core::consts::MAX_STACK_QUANTITY],
    ).await?;
    let mut result = DepositStack { merged: None, created: None };
    for row in &rows {
        let stack = DepositedStack { item: super::loading::GameItem::from_row(row), old_quantity: row.get("old_quantity"), added_quantity: row.get("added_quantity"), visible: row.get("visible") };
        if stack.old_quantity.is_some() { result.merged = Some(stack); } else { result.created = Some(stack); }
    }
    Ok(result)
}
/// One root and the owner and location it receives; descendants keep their parent.
pub(crate) struct Transfer {
    pub(crate) item_id: i64,
    pub(crate) owner: i64,
    pub(crate) location: i64,
    pub(crate) flag: i32,
}

/// Hand over the caller-revalidated closure, without locking additional rows.
/// Created IDs must come from INSERT RETURNING in this transaction; all other
/// closure members must be existing rows held UPDATE by the token.
pub(crate) async fn hand_over(ex: &(impl Executor + ?Sized), transfers: &[Transfer], closure: &[i64], locks: &LockedItems<'_>, created: &[i64]) -> Result<(), Error> {
    let ids: Vec<_> = transfers.iter().map(|item| item.item_id).collect();
    if ids.iter().any(|id| !closure.contains(id))
        || created.iter().any(|id| !closure.contains(id))
        || closure.iter().any(|id| !locks.is_write_locked(*id) && !created.contains(id))
    {
        return Err(Error::InvalidItemLockRequest("item transfer closure is incomplete or not write locked or created"));
    }
    let owners: Vec<_> = transfers.iter().map(|item| item.owner).collect();
    let locations: Vec<_> = transfers.iter().map(|item| item.location).collect();
    let flags: Vec<_> = transfers.iter().map(|item| item.flag).collect();
    let tree = "WITH RECURSIVE roots(root, owner, location, flag) AS (SELECT * FROM unnest($1::int8[], $2::int8[], $3::int8[], $4::int4[])), tree(root, id, owner, location, flag) AS (SELECT r.root, r.root, r.owner, r.location, r.flag FROM roots r JOIN game.items i ON i.item_id = r.root AND i.jump_clone_id IS NULL AND i.item_id = ANY($5) UNION SELECT t.root, i.item_id, t.owner, t.location, t.flag FROM tree t JOIN game.items i ON i.location_id = t.id AND i.jump_clone_id IS NULL AND i.item_id = ANY($5))";
    ex.execute(&format!("{tree}, changed AS (UPDATE game.items i SET owner_id = t.owner, location_id = CASE WHEN t.id = t.root THEN t.location ELSE i.location_id END, flag_id = CASE WHEN t.id = t.root THEN t.flag ELSE i.flag_id END FROM tree t WHERE i.item_id = t.id AND i.jump_clone_id IS NULL RETURNING i.item_id, i.owner_id, i.singleton) DELETE FROM game.ship_skin_applications a USING changed c WHERE a.ship_item_id = c.item_id AND (a.character_id <> c.owner_id OR NOT c.singleton)"), &[&ids, &owners, &locations, &flags, &closure]).await?;
    Ok(())
}

#[cfg(test)] mod tests;
