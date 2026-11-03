//! Server-owned simulated-pilot account persistence in `game.accounts`.
//! This server facility has no client counterpart and does not support client login.
//! The service role identifies server-owned accounts; ordinary registration never grants it.
//! Startup rejects a conflicting player account without modifying it.

use crate::consts::ROLE_SERVICE;
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
    ex.execute("INSERT INTO game.accounts (user_name, password_hash, role) VALUES ($1, $2, $3) ON CONFLICT DO NOTHING", &[&ACCOUNT_NAME, &password, &ROLE_SERVICE]).await?;
    Ok(ex.query_opt("SELECT (role & $2) != 0 AS valid FROM game.accounts WHERE lower(user_name) = lower($1)", &[&ACCOUNT_NAME, &ROLE_SERVICE]).await?.as_ref().is_some_and(|row| Ownership::from_row(row).valid))
}
