//! Bloodline identifiers and free skill points from `game.characters`, and
//! chosen bases and entitlements from `game.character_respec`.
//! No direct client mirror; policy, transactions and notifications are caller concerns.
use crate::db::{executor::Executor, Error};
use tokio_postgres::Row;

struct Bloodline { bloodline_id: i32 }
impl Bloodline {
    fn from_row(r: &Row) -> Self { Self { bloodline_id: r.get("bloodline_id") } }
}
pub(crate) async fn bloodline(ex: &(impl Executor + ?Sized), char_id: i64) -> Result<i32, Error> {
    Ok(Bloodline::from_row(&ex.query_one("SELECT bloodline_id FROM game.characters WHERE character_id = $1", &[&char_id]).await?).bloodline_id)
}
struct FreePoints { points: i64 }
impl FreePoints { fn from_row(r: &Row) -> Self { Self { points: r.get("free_skill_points") } } }
pub(crate) async fn free_points(ex: &(impl Executor + ?Sized), char_id: i64) -> Result<i64, Error> {
    Ok(FreePoints::from_row(&ex.query_one("SELECT free_skill_points FROM game.characters WHERE character_id = $1", &[&char_id]).await?).points)
}
/// Grant positive free points unless the character is deleted or the balance would overflow.
pub(crate) async fn grant_free_points(ex: &(impl Executor + ?Sized), character: i64, points: i64) -> Result<Option<i64>, Error> {
    if points <= 0 {
        return Ok(None);
    }
    Ok(ex.query_opt("UPDATE game.characters SET free_skill_points = free_skill_points + $2 WHERE character_id = $1 AND NOT deleted AND free_skill_points <= $3::bigint - $2 RETURNING free_skill_points", &[&character, &points, &i64::MAX]).await?.as_ref().map(FreePoints::from_row).map(|row| row.points))
}
/// Replace an observed free-point balance, refusing stale or negative updates.
pub(crate) async fn replace_free_points(ex: &(impl Executor + ?Sized), char_id: i64, expected: i64, points: i64) -> Result<bool, Error> {
    Ok(ex.execute("UPDATE game.characters SET free_skill_points = $3 WHERE character_id = $1 AND free_skill_points = $2 AND $3::bigint >= 0", &[&char_id, &expected, &points]).await? == 1)
}


const SELECT_RESPEC: &str = "SELECT bases, free_respecs, last_respec_date, next_timed_respec FROM game.character_respec WHERE character_id = $1";

/// Durable chosen bases and remap entitlements; absent before initialization.
pub(crate) struct Respec { pub bases: Option<Vec<i32>>, pub free_respecs: i64, pub last_respec_date: Option<i64>, pub next_timed_respec: Option<i64> }
impl Respec {
    fn from_row(r: &Row) -> Self { Self { bases: r.get("bases"), free_respecs: r.get("free_respecs"), last_respec_date: r.get("last_respec_date"), next_timed_respec: r.get("next_timed_respec") } }
}
pub(crate) async fn respec(ex: &(impl Executor + ?Sized), char_id: i64) -> Result<Option<Respec>, Error> {
    Ok(ex.query_opt(SELECT_RESPEC, &[&char_id]).await?.as_ref().map(Respec::from_row))
}
pub(crate) async fn initialize_respec(ex: &(impl Executor + ?Sized), char_id: i64, free: i64) -> Result<Respec, Error> {
    ex.execute("INSERT INTO game.character_respec (character_id, free_respecs) VALUES ($1, $2) ON CONFLICT (character_id) DO NOTHING", &[&char_id, &free]).await?;
    Ok(Respec::from_row(&ex.query_one(SELECT_RESPEC, &[&char_id]).await?))
}
pub(crate) async fn save_respec(ex: &(impl Executor + ?Sized), char_id: i64, bases: &[i32], state: &Respec, free: i64, date: i64, next: i64) -> Result<bool, Error> {
    Ok(ex.execute("UPDATE game.character_respec SET bases = $2, free_respecs = $3, last_respec_date = $4, next_timed_respec = $5 WHERE character_id = $1 AND free_respecs = $6 AND last_respec_date IS NOT DISTINCT FROM $7 AND next_timed_respec IS NOT DISTINCT FROM $8 AND bases IS NOT DISTINCT FROM $9", &[&char_id, &bases, &free, &date, &next, &state.free_respecs, &state.last_respec_date, &state.next_timed_respec, &state.bases]).await? == 1)
}

#[cfg(test)] mod tests;
