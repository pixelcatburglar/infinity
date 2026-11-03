//! Login credential lookup, account registration and last-login writes in `game.accounts`.
//! Registration also creates the account's unlimited Omega authority in `game.account_clone_grades`.
//! This DAO has no direct client mirror or wire contract; owning services supply those.
//! Password comparison, registration policy, transactions, refusals, notifications and World are caller concerns.
use crate::db::{self, executor::Executor};
use tokio_postgres::Row;

pub(crate) struct Credentials { pub user_id: i64, pub password_hash: Vec<u8>, pub role: i64, pub user_type: i32 }
impl Credentials {
    fn from_row(r: &Row) -> Self { Self { user_id: r.get("user_id"), password_hash: r.get("password_hash"), role: r.get("role"), user_type: r.get("user_type") } }
}
pub(crate) struct Registered { pub user_id: i64, pub user_type: i32 }
impl Registered {
    fn from_row(r: &Row) -> Self { Self { user_id: r.get("user_id"), user_type: r.get("user_type") } }
}
pub(crate) async fn find(ex: &(impl Executor + ?Sized), name: &str) -> Result<Option<Credentials>, db::Error> {
    Ok(ex.query_opt("SELECT user_id, password_hash, role, user_type FROM game.accounts WHERE lower(user_name) = lower($1)", &[&name]).await?.as_ref().map(Credentials::from_row))
}
pub(crate) async fn register(ex: &(impl Executor + ?Sized), name: &str, password: &[u8], role: i64, starting_plex: i64) -> Result<Registered, db::Error> {
    Ok(Registered::from_row(&ex.query_one("WITH account AS (INSERT INTO game.accounts (user_name, password_hash, role, plex_balance) VALUES ($1, $2, $3, $4::bigint::numeric) RETURNING user_id, user_type), authority AS (INSERT INTO game.account_clone_grades (user_id, grade) SELECT user_id, 1 FROM account) SELECT user_id, user_type FROM account", &[&name, &password, &role, &starting_plex]).await?))
}
pub(crate) async fn last_login(ex: &(impl Executor + ?Sized), user: i64) -> Result<u64, db::Error> {
    ex.execute("UPDATE game.accounts SET last_login = now() WHERE user_id = $1", &[&user]).await
}
