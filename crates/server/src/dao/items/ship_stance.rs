//! Tactical stance switch timing in `game.ship_stances`.
//! The current stance is the real hidden mode item; this table only records the
//! previous switch and its cooldown deadline, both in FILETIME.
//! This DAO has no direct client mirror or wire contract; owning services supply those.
//! Mode item existence, custody, transactions, refusals, notifications and World are caller concerns.
use crate::db::{executor::Executor, Error};
use tokio_postgres::Row;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Timing {
    /// Stance key before the last accepted switch; `None` means never switched.
    pub(crate) previous_stance_id: Option<i64>,
    /// FILETIME when the last switch was accepted; `0` on initialization.
    pub(crate) switched_at: i64,
    /// FILETIME when the next switch is admitted; `0` means no cooldown.
    pub(crate) ready_at: i64,
}
impl Timing {
    fn from_row(r: &Row) -> Self {
        Self { previous_stance_id: r.get("previous_stance_id"), switched_at: r.get("switched_at"), ready_at: r.get("ready_at") }
    }
}

pub(crate) async fn timing(ex: &(impl Executor + ?Sized), ship: i64) -> Result<Option<Timing>, Error> {
    Ok(ex.query_opt("SELECT previous_stance_id, switched_at, ready_at FROM game.ship_stances WHERE ship_id = $1", &[&ship]).await?.as_ref().map(Timing::from_row))
}

/// Insert the never-switched row; false when the hull already has one, so a
/// valid existing deadline is never reset by a second initialization.
pub(crate) async fn insert_timing(ex: &(impl Executor + ?Sized), ship: i64) -> Result<bool, Error> {
    Ok(ex.execute("INSERT INTO game.ship_stances (ship_id, previous_stance_id, switched_at, ready_at) VALUES ($1, NULL, 0, 0) ON CONFLICT (ship_id) DO NOTHING", &[&ship]).await? == 1)
}

/// Guarded timing replacement: false when the stored row no longer matches the
/// one the caller switched from.
pub(crate) async fn replace_timing(ex: &(impl Executor + ?Sized), ship: i64, expected: &Timing, next: &Timing) -> Result<bool, Error> {
    Ok(ex.execute("UPDATE game.ship_stances SET previous_stance_id = $2, switched_at = $3, ready_at = $4
                   WHERE ship_id = $1 AND previous_stance_id IS NOT DISTINCT FROM $5 AND switched_at = $6 AND ready_at = $7",
        &[&ship, &next.previous_stance_id, &next.switched_at, &next.ready_at, &expected.previous_stance_id, &expected.switched_at, &expected.ready_at]).await? == 1)
}

/// Drop the timing rows of a locked hull closure (repackaging, loss).
pub(crate) async fn clear(ex: &(impl Executor + ?Sized), ships: &[i64]) -> Result<(), Error> {
    if ships.is_empty() { return Ok(()); }
    ex.execute("DELETE FROM game.ship_stances WHERE ship_id = ANY($1)", &[&ships]).await?;
    Ok(())
}

#[cfg(all(test, feature = "tests-storage"))]
mod tests;
