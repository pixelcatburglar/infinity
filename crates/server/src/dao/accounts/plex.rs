//! Exact transaction-local PLEX vault persistence for the account vault service.
//! The client reads vault history through the plex vault gateway handlers; this module
//! owns the real ledger in game.plex_vault_transactions and balances in game.accounts.
//! The kind = 'opening' baseline row is internal and excluded from every ledger read API.
//! Transaction and notification policy belong to the caller; this module builds no wire
//! payloads. Account creation supplies the initial balance.
use crate::db::{self, executor::Executor};
use tokio_postgres::Row;

pub(crate) async fn balance(ex: &(impl Executor + ?Sized), user: i64) -> Result<String, db::Error> {
    Ok(ex.query_one("SELECT plex_balance::text AS value FROM game.accounts WHERE user_id = $1", &[&user]).await?.get("value"))
}

pub(crate) async fn update(ex: &(impl Executor + ?Sized), user: i64, before: &str, after: &str) -> Result<bool, db::Error> {
    Ok(ex.execute("UPDATE game.accounts SET plex_balance = $3::text::numeric WHERE user_id = $1 AND plex_balance = $2::text::numeric", &[&user, &before, &after]).await? == 1)
}

pub(crate) async fn opening(ex: &(impl Executor + ?Sized), user: i64, balance: &str, created: i64) -> Result<(), db::Error> {
    ex.execute("INSERT INTO game.plex_vault_transactions (user_id, delta, balance_after, kind, created) VALUES ($1, $2::text::numeric, $2::text::numeric, 'opening', $3) ON CONFLICT (user_id) WHERE kind = 'opening' DO NOTHING", &[&user, &balance, &created]).await?;
    Ok(())
}

pub(crate) struct LoggedTransaction {
    pub created: i64,
    pub delta: String,
    pub balance_after: String,
}
impl LoggedTransaction {
    fn from_row(row: &Row) -> Self {
        Self {
            created: row.get("created"),
            delta: row.get("delta"),
            balance_after: row.get("balance_after"),
        }
    }
}

pub(crate) struct VaultStatistics {
    pub earliest_created: Option<i64>,
    pub incomes: String,
    pub expenses: String,
    pub transactions_count: i64,
}
impl VaultStatistics {
    fn from_row(row: &Row) -> Self {
        Self {
            earliest_created: row.get("earliest_created"),
            incomes: row.get("incomes"),
            expenses: row.get("expenses"),
            transactions_count: row.get("transactions_count"),
        }
    }
}

pub(crate) async fn logged_ids(ex: &(impl Executor + ?Sized), user: i64) -> Result<Vec<i64>, db::Error> {
    Ok(ex.query("SELECT transaction_id FROM game.plex_vault_transactions WHERE user_id = $1 AND kind <> 'opening' ORDER BY created DESC, transaction_id DESC", &[&user]).await?.iter().map(|row| row.get("transaction_id")).collect())
}

pub(crate) async fn logged_transaction(ex: &(impl Executor + ?Sized), user: i64, transaction_id: i64) -> Result<Option<LoggedTransaction>, db::Error> {
    Ok(ex.query_opt("SELECT created, delta::text AS delta, balance_after::text AS balance_after FROM game.plex_vault_transactions WHERE user_id = $1 AND transaction_id = $2 AND kind <> 'opening'", &[&user, &transaction_id]).await?.as_ref().map(LoggedTransaction::from_row))
}

pub(crate) async fn statistics(ex: &(impl Executor + ?Sized), user: i64) -> Result<VaultStatistics, db::Error> {
    Ok(VaultStatistics::from_row(&ex.query_one("SELECT MIN(created) AS earliest_created, COALESCE(SUM(delta) FILTER (WHERE delta > 0), 0)::text AS incomes, COALESCE(SUM(delta) FILTER (WHERE delta < 0), 0)::text AS expenses, COUNT(*) AS transactions_count FROM game.plex_vault_transactions WHERE user_id = $1 AND kind <> 'opening'", &[&user]).await?))
}

pub(crate) async fn append(ex: &(impl Executor + ?Sized), user: i64, actor: Option<i64>, delta: &str, balance: &str, kind: &str, created: i64, item: Option<i64>, reference: Option<i64>, order_id: Option<i64>, trade_id: Option<i64>) -> Result<i64, db::Error> {
    Ok(ex.query_one("INSERT INTO game.plex_vault_transactions (user_id, actor_char_id, delta, balance_after, kind, created, item_id, reference, order_id, trade_id) VALUES ($1, $2, $3::text::numeric, $4::text::numeric, $5, $6, $7, $8, $9, $10) RETURNING transaction_id", &[&user, &actor, &delta, &balance, &kind, &created, &item, &reference, &order_id, &trade_id]).await?.get("transaction_id"))
}

pub(crate) async fn reference(ex: &(impl Executor + ?Sized)) -> Result<i64, db::Error> {
    Ok(ex.query_one("SELECT nextval('game.plex_vault_reference_seq') AS value", &[]).await?.get("value"))
}

#[cfg(all(test, any(feature = "tests-economy", feature = "tests-gateway", feature = "tests-inventory", feature = "tests-slash", feature = "tests-storage")))] pub(crate) mod tests;
