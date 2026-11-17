//! Account creation date lookup in `game.accounts` for character selection.
//! This DAO has no direct client mirror or wire contract; owning services supply those.
//! Subscription policy, transactions, refusals, notifications and World are caller concerns.
use crate::db::{self, executor::Executor};
pub(crate) async fn creation_date(ex: &(impl Executor + ?Sized), user_id: i64, second: i64, epoch: i64) -> Result<i64, db::Error> {
    Ok(ex.query_one("SELECT (extract(epoch FROM created_at) * $2::bigint)::bigint + $3::bigint AS creation_date FROM game.accounts WHERE user_id = $1", &[&user_id, &second, &epoch]).await?.get("creation_date"))
}
