//! Physical hull player-kill counts in `game.ship_kill_counts`.
//! The ship service supplies the client `shipKillCounter` protocol and destiny
//! slim items. Counts follow the item, not its owner; repackaging clears the
//! row inside `items::fitting::repackage`, real item deletion cascades, and
//! transactions and authorization are caller concerns.
//! This DAO does not emit notifications or apply bay configuration policy.
use crate::db::{self, executor::Executor};
use tokio_postgres::Row;

struct KillCount { player_kills: i64 }
impl KillCount { fn from_row(row: &Row) -> Self { Self { player_kills: row.get("player_kills") } } }

/// Write the player-kill count of an already locked and authorized hull;
/// the caller holds the hull row lock until commit and checked `count >= 0`.
/// There is no getter: reads join this table inside `items::ship_appearance`.
pub(crate) async fn set_player_kills(ex: &(impl Executor + ?Sized), ship: i64, count: i64) -> Result<i64, db::Error> {
    Ok(KillCount::from_row(&ex.query_one("INSERT INTO game.ship_kill_counts (ship_item_id,player_kills) VALUES ($1,$2) ON CONFLICT (ship_item_id) DO UPDATE SET player_kills=EXCLUDED.player_kills RETURNING player_kills", &[&ship, &count]).await?).player_kills)
}
