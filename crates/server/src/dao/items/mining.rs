//! Mining and reprocessing game item and price reads; static data is owned by SDE.
use crate::db::{self, executor::Executor};
use tokio_postgres::Row;

pub(crate) struct Price { pub type_id: i32, pub average: f64 }
impl Price { fn from_row(r: &Row) -> Self { Self { type_id: r.get("type_id"), average: r.get("average") } } }
/// Lock the exact online fitted module until the caller's transaction commits.
pub(crate) async fn lock_module(ex: &(impl Executor + ?Sized), owner: i64, ship: i64, module: i64, module_type: i32, module_flag: i32) -> Result<bool, db::Error> {
    Ok(ex.query_opt("SELECT item_id FROM game.items WHERE item_id = $1 AND type_id = $2 AND owner_id = $3 AND location_id = $4 AND flag_id = $5 AND singleton AND online FOR UPDATE", &[&module, &module_type, &owner, &ship, &module_flag]).await?.is_some())
}
pub(crate) async fn prices(ex: &(impl Executor + ?Sized), ids: &[i32]) -> Result<Vec<Price>, db::Error> {
    Ok(ex.query("SELECT type_id, COALESCE(average, 0)::float8 AS average FROM game.retail_prices WHERE type_id = ANY($1)", &[&ids]).await?.iter().map(Price::from_row).collect())
}
