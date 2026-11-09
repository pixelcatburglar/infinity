//! Exact transaction-local PLEX vault persistence for the account vault service.
//! The client reads the vault through vgs and vaultManager; ledger rows are server-owned.
//! Uses game.accounts and game.plex_vault_transactions. Transaction and notification policy
//! belong to the caller; account creation supplies the initial balance.
use crate::db::{self, executor::Executor};
use tokio_postgres::Row;

struct Exact { value: String }
impl Exact { fn from_row(row: &Row) -> Self { Self { value: row.get("value") } } }

pub(crate) async fn balance(ex: &(impl Executor + ?Sized), user: i64) -> Result<String, db::Error> {
    Ok(Exact::from_row(&ex.query_one("SELECT plex_balance::text AS value FROM game.accounts WHERE user_id = $1", &[&user]).await?).value)
}

pub(crate) async fn update(ex: &(impl Executor + ?Sized), user: i64, before: &str, after: &str) -> Result<bool, db::Error> {
    Ok(ex.execute("UPDATE game.accounts SET plex_balance = $3::text::numeric WHERE user_id = $1 AND plex_balance = $2::text::numeric", &[&user, &before, &after]).await? == 1)
}

pub(crate) async fn opening(ex: &(impl Executor + ?Sized), user: i64, balance: &str, created: i64) -> Result<(), db::Error> {
    ex.execute("INSERT INTO game.plex_vault_transactions (user_id, delta, balance_after, kind, created) VALUES ($1, $2::text::numeric, $2::text::numeric, 'opening', $3) ON CONFLICT (user_id) WHERE kind = 'opening' DO NOTHING", &[&user, &balance, &created]).await?;
    Ok(())
}

pub(crate) async fn append(ex: &(impl Executor + ?Sized), user: i64, actor: Option<i64>, delta: &str, balance: &str, kind: &str, created: i64, item: Option<i64>, reference: Option<i64>, order_id: Option<i64>, trade_id: Option<i64>) -> Result<(), db::Error> {
    ex.execute("INSERT INTO game.plex_vault_transactions (user_id, actor_char_id, delta, balance_after, kind, created, item_id, reference, order_id, trade_id) VALUES ($1, $2, $3::text::numeric, $4::text::numeric, $5, $6, $7, $8, $9, $10)", &[&user, &actor, &delta, &balance, &kind, &created, &item, &reference, &order_id, &trade_id]).await?;
    Ok(())
}

pub(crate) async fn reference(ex: &(impl Executor + ?Sized)) -> Result<i64, db::Error> {
    struct Reference { value: i64 }
    impl Reference { fn from_row(row: &Row) -> Self { Self { value: row.get("value") } } }
    Ok(Reference::from_row(&ex.query_one("SELECT nextval('game.plex_vault_reference_seq') AS value", &[]).await?).value)
}

#[cfg(test)] pub(crate) mod tests;
