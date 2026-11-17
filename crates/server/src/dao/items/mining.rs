//! Mining and reprocessing game item reads; static data is owned by SDE.
use crate::db::{self, executor::Executor};

/// Lock the exact online fitted module until the caller's transaction commits.
pub(crate) async fn lock_module(ex: &(impl Executor + ?Sized), owner: i64, ship: i64, module: i64, module_type: i32, module_flag: i32) -> Result<bool, db::Error> {
    Ok(ex.query_opt("SELECT item_id FROM game.items WHERE item_id = $1 AND type_id = $2 AND owner_id = $3 AND location_id = $4 AND flag_id = $5 AND singleton AND online FOR UPDATE", &[&module, &module_type, &owner, &ship, &module_flag]).await?.is_some())
}
