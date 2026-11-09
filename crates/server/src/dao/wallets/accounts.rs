//! Canonical wallet-account identities in game.wallet_accounts; balances remain in
//! character and corporation wallets. No client service or client module directly
//! mirrors this helper, and it does not perform authorization or balance mutations.
use crate::db::{self, executor::Executor};
use tokio_postgres::Row;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Account { pub(crate) id: i64 }
impl Account { fn from_row(row: &Row) -> Self { Self { id: row.get("account_id") } } }

pub(crate) async fn ensure(ex: &(impl Executor + ?Sized), owner_id: i64, key: i32) -> Result<Account, db::Error> {
    Ok(Account::from_row(&ex.query_one(
        "INSERT INTO game.wallet_accounts (owner_id, account_key) VALUES ($1, $2)
         ON CONFLICT (owner_id, account_key) DO UPDATE SET owner_id = EXCLUDED.owner_id
         RETURNING account_id",
        &[&owner_id, &key],
    ).await?))
}

#[cfg(test)]
mod tests;
