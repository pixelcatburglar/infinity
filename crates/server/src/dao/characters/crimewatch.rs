//! Crimewatch security, location, safety level and attacker corporation
//! persistence in `game.characters`.
//! This DAO has no direct client mirror or wire contract; the owning service handles those.
//! Policy, transactions and notifications are not handled here.

use crate::db::{self, executor::Executor};
use tokio_postgres::Row;

struct Security {
    security_status: f64,
}

impl Security {
    fn from_row(r: &Row) -> Self {
        Self { security_status: r.get("security_status") }
    }
}

struct Corporation {
    corporation_id: i64,
}

impl Corporation {
    fn from_row(r: &Row) -> Self {
        Self { corporation_id: r.get("corporation_id") }
    }
}

struct Location {
    solar_system_id: Option<i64>,
}

impl Location {
    fn from_row(r: &Row) -> Self {
        Self { solar_system_id: r.get("solar_system_id") }
    }
}

struct Safety {
    safety_level: i64,
}

impl Safety {
    fn from_row(r: &Row) -> Self {
        Self { safety_level: r.get("safety_level") }
    }
}

pub(crate) async fn security(ex: &(impl Executor + ?Sized), char_id: i64) -> Result<Option<f64>, db::Error> {
    let row = ex.query_opt("SELECT security_status FROM game.characters WHERE character_id = $1", &[&char_id]).await?;
    Ok(row.as_ref().map(|r| Security::from_row(r).security_status))
}

pub(crate) async fn shift_security(ex: &(impl Executor + ?Sized), char_id: i64, fraction: f64, toward: f64) -> Result<f64, db::Error> {
    let row = ex.query_one(
        "UPDATE game.characters SET security_status = GREATEST(-10, LEAST(10, security_status + ($3 - security_status) * $2))
             WHERE character_id = $1 RETURNING security_status",
        &[&char_id, &fraction, &toward],
    ).await?;
    Ok(Security::from_row(&row).security_status)
}

/// Replace a character's security status exactly.
pub(crate) async fn set_security(ex: &(impl Executor + ?Sized), char_id: i64, value: f64) -> Result<f64, db::Error> {
    let row = ex.query_one(
        "UPDATE game.characters SET security_status = $2 WHERE character_id = $1 RETURNING security_status",
        &[&char_id, &value],
    ).await?;
    Ok(Security::from_row(&row).security_status)
}

pub(crate) async fn corporation(ex: &(impl Executor + ?Sized), char_id: i64) -> Result<Option<i64>, db::Error> {
    let row = ex.query_opt("SELECT corporation_id FROM game.characters WHERE character_id = $1", &[&char_id]).await?;
    Ok(row.as_ref().map(|r| Corporation::from_row(r).corporation_id))
}

pub(crate) async fn solar_system(ex: &(impl Executor + ?Sized), char_id: i64) -> Result<Option<i64>, db::Error> {
    let row = ex.query_opt("SELECT solar_system_id FROM game.characters WHERE character_id = $1", &[&char_id]).await?;
    Ok(row.as_ref().and_then(|r| Location::from_row(r).solar_system_id))
}

/// Persisted safety level of one live character; `None` when the row is
/// missing or deleted.
pub(crate) async fn safety(ex: &(impl Executor + ?Sized), character: i64) -> Result<Option<i64>, db::Error> {
    let row = ex.query_opt("SELECT safety_level FROM game.characters WHERE character_id = $1 AND NOT deleted", &[&character]).await?;
    Ok(row.as_ref().map(|r| Safety::from_row(r).safety_level))
}

/// Replace the safety level in one guarded write, returning the stored
/// value; `None` when no live row matches. The 0..2 enum domain is the
/// column's CHECK constraint.
pub(crate) async fn set_safety(ex: &(impl Executor + ?Sized), character: i64, level: i64) -> Result<Option<i64>, db::Error> {
    let row = ex.query_opt(
        "UPDATE game.characters SET safety_level = $2 WHERE character_id = $1 AND NOT deleted RETURNING safety_level",
        &[&character, &level],
    ).await?;
    Ok(row.as_ref().map(|r| Safety::from_row(r).safety_level))
}
