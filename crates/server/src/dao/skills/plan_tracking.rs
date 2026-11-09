//! Active personal or certified skill plan tracking in `game.skill_plan_tracking`.
//! The gateway validates ownership and static certified IDs before writing.
//! Cancellation is represented by no row; this module does not open transactions.
use crate::db::{self, executor::Executor};
use tokio_postgres::Row;

pub(crate) struct Tracking { pub personal_plan_id: Option<Vec<u8>>, pub certified_plan_id: Option<i64> }
impl Tracking {
    fn from_row(row: &Row) -> Self { Self { personal_plan_id: row.get("personal_plan_id"), certified_plan_id: row.get("certified_plan_id") } }
}
pub(crate) async fn get(ex: &(impl Executor + ?Sized), character_id: i64) -> Result<Option<Tracking>, db::Error> {
    Ok(ex.query_opt("SELECT personal_plan_id, certified_plan_id FROM game.skill_plan_tracking WHERE character_id = $1", &[&character_id]).await?.as_ref().map(Tracking::from_row))
}
pub(crate) async fn set(ex: &(impl Executor + ?Sized), character_id: i64, personal: Option<&[u8]>, certified: Option<i64>) -> Result<(), db::Error> {
    ex.execute("INSERT INTO game.skill_plan_tracking(character_id, personal_plan_id, certified_plan_id) VALUES ($1, $2, $3) ON CONFLICT (character_id) DO UPDATE SET personal_plan_id = EXCLUDED.personal_plan_id, certified_plan_id = EXCLUDED.certified_plan_id", &[&character_id, &personal, &certified]).await?;
    Ok(())
}
pub(crate) async fn clear(ex: &(impl Executor + ?Sized), character_id: i64) -> Result<(), db::Error> {
    ex.execute("DELETE FROM game.skill_plan_tracking WHERE character_id = $1", &[&character_id]).await?;
    Ok(())
}
