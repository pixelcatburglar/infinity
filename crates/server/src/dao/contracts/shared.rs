//! Contract item, bid and status persistence for contractProxy.
//! Uses game contracts, contract items, bids and items; status writes guard the previous status.
use crate::db::{executor::Executor, Error};
use tokio_postgres::Row;

pub(crate) struct ContractItem { pub contract_id: i64, pub item_id: Option<i64>, pub type_id: i64, pub quantity: i64, pub in_crate: bool }
impl ContractItem { fn from_row(r: &Row) -> Self { Self { contract_id: r.get("contract_id"), item_id: r.get("item_id"), type_id: r.get("type_id"), quantity: r.get("quantity"), in_crate: r.get("in_crate") } } }
pub(crate) async fn items(ex: &(impl Executor + ?Sized), ids: &[i64]) -> Result<Vec<ContractItem>, Error> {
    Ok(ex.query("SELECT contract_id, item_id, type_id::bigint, quantity::bigint, in_crate FROM game.contract_items WHERE contract_id = ANY($1)", &[&ids]).await?.iter().map(ContractItem::from_row).collect())
}
pub(crate) struct Bid { pub bid_id: i64, pub contract_id: i64, pub bidder_id: i64, pub amount: f64, pub date: i64 }
impl Bid { fn from_row(r: &Row) -> Self { Self { bid_id: r.get("bid_id"), contract_id: r.get("contract_id"), bidder_id: r.get("bidder_id"), amount: r.get("amount"), date: r.get("date") } } }
pub(crate) async fn bids(ex: &(impl Executor + ?Sized), ids: &[i64]) -> Result<Vec<Bid>, Error> {
    Ok(ex.query("SELECT bid_id, contract_id, bidder_id, amount, date FROM game.contract_bids WHERE contract_id = ANY($1) ORDER BY amount DESC", &[&ids]).await?.iter().map(Bid::from_row).collect())
}
pub(crate) async fn set_status(ex: &(impl Executor + ?Sized), id: i64, old_status: i32, status: i32, completed: i64, acceptor: i64, acceptor_wallet_key: i32) -> Result<bool, Error> {
    Ok(ex.execute("UPDATE game.contracts SET status = $2, date_completed = $3, acceptor_id = $4, acceptor_wallet_key = $5 WHERE contract_id = $1 AND status = $6", &[&id, &status, &completed, &acceptor, &acceptor_wallet_key, &old_status]).await? == 1)
}
struct ItemId { item_id: i64 }
impl ItemId { fn from_row(r: &Row) -> Self { Self { item_id: r.get("item_id") } } }
pub(crate) async fn insert_item(ex: &(impl Executor + ?Sized), contract: i64, item: i64, type_id: i32, quantity: i64) -> Result<u64, Error> {
    ex.execute("INSERT INTO game.contract_items (contract_id, item_id, type_id, quantity, in_crate) VALUES ($1, $2, $3, $4, true)", &[&contract, &item, &type_id, &quantity]).await
}
pub(crate) async fn escrowed_ids(ex: &(impl Executor + ?Sized), id: i64) -> Result<Vec<i64>, Error> {
    Ok(ex.query("SELECT item_id FROM game.contract_items WHERE contract_id = $1 AND in_crate AND item_id IS NOT NULL", &[&id]).await?.iter().map(|r| ItemId::from_row(r).item_id).collect())
}
/// Capture actual escrow descendants, including clone rows, without acquiring locks.
/// A missing root is still a traversal seed.
pub(crate) async fn escrow_closure(ex: &(impl Executor + ?Sized), roots: &[i64]) -> Result<Vec<i64>, Error> {
    Ok(ex.query(
        "WITH RECURSIVE tree(id) AS (
             SELECT unnest($1::bigint[])
             UNION
             SELECT i.item_id FROM game.items i JOIN tree t ON i.location_id = t.id
         ) SELECT i.item_id FROM game.items i JOIN tree t ON t.id = i.item_id ORDER BY i.item_id",
        &[&roots],
    ).await?.iter().map(|row| ItemId::from_row(row).item_id).collect())
}
/// Delete the caller-revalidated existing closure without acquiring item locks.
/// Missing roots remain traversal seeds and need not appear in the closure.
pub(crate) async fn destroy_escrow(ex: &(impl Executor + ?Sized), roots: &[i64], closure: &[i64], locks: &crate::dao::items::parent_locks::LockedItems<'_>) -> Result<(), Error> {
    if closure.iter().any(|id| !locks.is_write_locked(*id)) {
        return Err(Error::InvalidItemLockRequest("escrow closure is not write locked"));
    }
    ex.execute(
        "WITH RECURSIVE tree(id) AS (
             SELECT unnest($1::bigint[])
             UNION
             SELECT i.item_id FROM game.items i JOIN tree t ON i.location_id = t.id
             WHERE i.item_id = ANY($2)
         ) DELETE FROM game.items WHERE item_id IN (SELECT id FROM tree) AND item_id = ANY($2)",
        &[&roots, &closure],
    ).await?;
    Ok(())
}
pub(crate) struct TopBid { pub bidder_id: i64, pub wallet_key: i32, pub amount: f64 }
impl TopBid { fn from_row(r: &Row) -> Self { Self { bidder_id: r.get("bidder_id"), wallet_key: r.get("wallet_key"), amount: r.get("amount") } } }
pub(crate) async fn top_bid(ex: &(impl Executor + ?Sized), id: i64) -> Result<Option<TopBid>, Error> {
    Ok(ex.query_opt("SELECT bidder_id, wallet_key, amount FROM game.contract_bids WHERE contract_id = $1 ORDER BY amount DESC, bid_id LIMIT 1", &[&id]).await?.map(|r| TopBid::from_row(&r)))
}

pub(crate) struct OutstandingCounts {
    pub my_char_total: i64,
    pub non_corp_for_my_char: i64,
    pub my_corp_total: i64,
    pub non_corp_for_my_corp: i64,
}

pub(crate) async fn outstanding_counts(ex: &(impl Executor + ?Sized), char_id: i64, corp_id: i64, status: i32) -> Result<OutstandingCounts, Error> {
    let row = ex.query_one(
        "SELECT count(*) FILTER (WHERE NOT c.for_corp AND c.issuer_id = $1) AS my_char_total,
                count(*) FILTER (WHERE NOT c.for_corp AND c.issuer_id = $1 AND NOT (c.assignee_id = $2 OR EXISTS (SELECT 1 FROM game.characters a WHERE a.character_id = c.assignee_id AND a.corporation_id = $2 AND NOT a.deleted))) AS non_corp_for_my_char,
                count(*) FILTER (WHERE c.for_corp AND c.issuer_corp_id = $2) AS my_corp_total,
                count(*) FILTER (WHERE c.for_corp AND c.issuer_corp_id = $2 AND NOT (c.assignee_id = $2 OR EXISTS (SELECT 1 FROM game.characters a WHERE a.character_id = c.assignee_id AND a.corporation_id = $2 AND NOT a.deleted))) AS non_corp_for_my_corp
         FROM game.contracts c WHERE c.status = $3",
        &[&char_id, &corp_id, &status],
    ).await?;
    Ok(OutstandingCounts { my_char_total: row.get("my_char_total"), non_corp_for_my_char: row.get("non_corp_for_my_char"), my_corp_total: row.get("my_corp_total"), non_corp_for_my_corp: row.get("non_corp_for_my_corp") })
}
