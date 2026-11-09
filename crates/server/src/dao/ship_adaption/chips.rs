//! Guarded chip balance statements for the adaptation subject ledger.
//! The client service controls authorization, transaction and ordering; nodes and
//! missions are not represented by these rows.

use crate::db::{self, executor::Executor};

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Chip {
    pub type_id: i32,
    pub balance: u32,
}

impl Chip {
    pub(super) fn from_row(row: &tokio_postgres::Row) -> Self {
        let balance: i64 = row.get("balance");
        Self { type_id: row.get("type_id"), balance: u32::try_from(balance).expect("chip balance is constrained by schema") }
    }
}

/// A false result means the uint32 ceiling would be exceeded.
pub(crate) async fn credit(ex: &(impl Executor + ?Sized), character: i64, type_id: i32, amount: u32) -> Result<bool, db::Error> {
    let amount = i64::from(amount);
    Ok(ex.execute("INSERT INTO game.ship_adaption_chips (character_id, type_id, balance) VALUES ($1, $2, $3) ON CONFLICT (character_id, type_id) DO UPDATE SET balance = game.ship_adaption_chips.balance + EXCLUDED.balance WHERE game.ship_adaption_chips.balance <= 4294967295 - EXCLUDED.balance", &[&character, &type_id, &amount]).await? == 1)
}

/// Missing rows and insufficient balances return false; exhausted rows remain.
pub(crate) async fn debit(ex: &(impl Executor + ?Sized), character: i64, type_id: i32, amount: u32) -> Result<bool, db::Error> {
    let amount = i64::from(amount);
    Ok(ex.execute("UPDATE game.ship_adaption_chips SET balance = balance - $3 WHERE character_id = $1 AND type_id = $2 AND balance >= $3", &[&character, &type_id, &amount]).await? == 1)
}
