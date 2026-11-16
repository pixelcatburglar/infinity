//! Character NPC standings persistence, used by standingsvc and mission mechanics.
//! Reads and writes game.standings; `history` persists game.standing_history
//! journal rows. Corporation standings are not modelled here.

use crate::db::{executor::Executor, Error};

pub(crate) mod history;

pub(crate) struct StandingRow { pub from_id: i64, pub standing: f64 }
impl StandingRow {
    fn from_row(row: &tokio_postgres::Row) -> Self {
        Self { from_id: row.get("from_id"), standing: row.get("standing") }
    }
}
struct StandingValue { standing: f64 }
impl StandingValue {
    fn from_row(row: &tokio_postgres::Row) -> Self { Self { standing: row.get("standing") } }
}
pub(crate) async fn standing(ex: &(impl Executor + ?Sized), char_id: i64, from: i64) -> Result<Option<f64>, Error> {
    Ok(ex.query_opt("SELECT standing FROM game.standings WHERE char_id = $1 AND from_id = $2", &[&char_id, &from]).await?.map(|r| StandingValue::from_row(&r).standing))
}
pub(crate) async fn set(ex: &(impl Executor + ?Sized), char_id: i64, from: i64, standing: f64) -> Result<u64, Error> {
    ex.execute("INSERT INTO game.standings (char_id, from_id, standing) VALUES ($1, $2, $3)
             ON CONFLICT (char_id, from_id) DO UPDATE SET standing = $3", &[&char_id, &from, &standing]).await
}
pub(crate) async fn list(ex: &impl Executor, char_id: i64) -> Result<Vec<StandingRow>, Error> {
    Ok(ex.query("SELECT from_id, standing FROM game.standings WHERE char_id = $1", &[&char_id]).await?.iter().map(StandingRow::from_row).collect())
}
