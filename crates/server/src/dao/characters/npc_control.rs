//! Persistent per-character NPC target-ignore switch in `game.characters`.
//! A `true` flag makes the character's piloted ship and owned space drones
//! ineligible NPC targets; eligibility and live-actor projection are caller concerns.
//! Authorization, transactions, notifications and World are caller concerns;
//! this DAO has no client wire contract.

use crate::db::{self, executor::Executor};
use tokio_postgres::Row;

struct Ignored {
    npc_ignored: bool,
}
impl Ignored {
    fn from_row(row: &Row) -> Self {
        Self { npc_ignored: row.get("npc_ignored") }
    }
}

/// Flag of one live character; `None` when the row is missing or deleted.
pub(crate) async fn get(ex: &(impl Executor + ?Sized), character: i64) -> Result<Option<bool>, db::Error> {
    Ok(ex.query_opt("SELECT gm_npc_ignored AS npc_ignored FROM game.characters WHERE character_id = $1 AND NOT deleted", &[&character]).await?.as_ref().map(Ignored::from_row).map(|row| row.npc_ignored))
}

/// Set or atomically toggle (`None` desired) the flag of a live character owned
/// by `user`, returning the persisted value; `None` when no such owned row exists.
pub(crate) async fn set(ex: &(impl Executor + ?Sized), character: i64, user: i64, desired: Option<bool>) -> Result<Option<bool>, db::Error> {
    Ok(ex.query_opt("UPDATE game.characters SET gm_npc_ignored = COALESCE($3::boolean, NOT gm_npc_ignored) WHERE character_id = $1 AND user_id = $2 AND NOT deleted RETURNING gm_npc_ignored AS npc_ignored", &[&character, &user, &desired]).await?.as_ref().map(Ignored::from_row).map(|row| row.npc_ignored))
}
