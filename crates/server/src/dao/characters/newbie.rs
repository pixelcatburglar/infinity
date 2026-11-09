//! Locked character authorization and one-time spare kit claim in `game.characters`.
//! The service owns policy, transactions and notifications; this DAO has no client wire contract.

use crate::db::{self, executor::Executor};
use tokio_postgres::Row;

pub(crate) struct GrantCharacter {
    pub user_id: i64,
    pub station_id: Option<i64>,
    pub race_id: i32,
    pub ship_id: Option<i64>,
    pub claimed_at: Option<i64>,
}

impl GrantCharacter {
    fn from_row(row: &Row) -> Self {
        Self { user_id: row.get("user_id"), station_id: row.get("station_id"),
            race_id: row.get("race_id"), ship_id: row.get("ship_id"),
            claimed_at: row.get("newbie_bonus_claimed_at") }
    }
}

pub(crate) async fn lock(ex: &(impl Executor + ?Sized), character: i64) -> Result<Option<GrantCharacter>, db::Error> {
    Ok(ex.query_opt("SELECT user_id, station_id, race_id, ship_id, newbie_bonus_claimed_at FROM game.characters WHERE character_id = $1 AND NOT deleted FOR UPDATE", &[&character]).await?.as_ref().map(GrantCharacter::from_row))
}

pub(crate) async fn claim_bonus_if_unclaimed(ex: &(impl Executor + ?Sized), character: i64, now: i64) -> Result<bool, db::Error> {
    Ok(ex.execute("UPDATE game.characters SET newbie_bonus_claimed_at = $2 WHERE character_id = $1 AND newbie_bonus_claimed_at IS NULL", &[&character, &now]).await? == 1)
}
