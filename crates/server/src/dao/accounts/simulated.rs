//! Server-owned simulated-pilot account and clone authority persistence.
//! This server facility has no client counterpart and does not support client login.
//! New or missing authority is initialized to unlimited Omega; existing grades are preserved.
//! Startup rejects a conflicting player account without modifying it.

use infinity_core::consts::ROLE_SERVICE;
use crate::db::{self, executor::Executor};
use tokio_postgres::Row;

/// Server design: reserved owner account for simulated pilots.
pub(crate) const ACCOUNT_NAME: &str = "infinity-simulated-pilots";

struct Ownership { valid: bool }
impl Ownership {
    fn from_row(row: &Row) -> Self { Self { valid: row.get("valid") } }
}

struct AccountId { user_id: i64 }
impl AccountId {
    fn from_row(row: &Row) -> Self { Self { user_id: row.get("user_id") } }
}

pub(crate) async fn account_id(ex: &(impl Executor + ?Sized)) -> Result<i64, db::Error> {
    let row = ex.query_one("SELECT user_id FROM game.accounts WHERE lower(user_name) = lower($1) AND (role & $2) != 0", &[&ACCOUNT_NAME, &ROLE_SERVICE]).await?;
    Ok(AccountId::from_row(&row).user_id)
}

pub(crate) async fn ensure(ex: &(impl Executor + ?Sized)) -> Result<bool, db::Error> {
    let password: &[u8] = &[];
    let row = ex.query_one(initial_authority_sql!("WITH account AS (INSERT INTO game.accounts AS existing (user_name, password_hash, role) VALUES ($1, $2, $3) ON CONFLICT (lower(user_name)) DO UPDATE SET user_name = existing.user_name WHERE (existing.role & $3) != 0 RETURNING user_id), ", "SELECT EXISTS (SELECT 1 FROM account) AS valid"), &[&ACCOUNT_NAME, &password, &ROLE_SERVICE]).await?;
    Ok(Ownership::from_row(&row).valid)
}

#[cfg(test)] mod tests;
