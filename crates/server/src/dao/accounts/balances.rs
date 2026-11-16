//! PLEX vault balance lookup in `game.accounts` for store and gateway callers.
//! This DAO has no direct client mirror or wire contract; owning services supply those.
//! Unit conversion, transactions, refusals, notifications and World are caller concerns.
//! PLEX is stored as exact numeric and projected to clamped whole units for the client.
//! Writes and the ledger belong to the PLEX vault authority.
use crate::db::{self, executor::Executor};
use infinity_core::consts::CLIENT_MAX_SHORT_AMOUNT;

pub(crate) async fn plex_balance(ex: &(impl Executor + ?Sized), user_id: i64) -> Result<i64, db::Error> {
    Ok(ex.query_one("SELECT LEAST(plex_balance, $2::bigint)::bigint AS plex_balance FROM game.accounts WHERE user_id = $1", &[&user_id, &CLIENT_MAX_SHORT_AMOUNT]).await?.get("plex_balance"))
}
