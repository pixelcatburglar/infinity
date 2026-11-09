//! Character and corporation wallet authority and journal storage in game tables.
//! Client presentation is handled by services; no float is used for wallet authority.
//! Corporation authorization remains in the corporation DAO.

pub(crate) mod characters;
pub(crate) mod corporations;
pub(crate) mod loyalty;
pub(crate) mod accounts;
pub(crate) mod journal;

use std::fmt;
use crate::{db, isk::{AmountError, IskAmount}};

/// PostgreSQL numeric(36,2) maximum, used in guarded wallet writes.
const MAX_BALANCE: &str = "9999999999999999999999999999999999.99";

#[derive(Debug)]
pub(crate) enum WalletError { Db(db::Error), Amount(AmountError) }
impl fmt::Display for WalletError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self { Self::Db(e) => e.fmt(f), Self::Amount(e) => e.fmt(f) }
    }
}
impl std::error::Error for WalletError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self { Self::Db(e) => Some(e), Self::Amount(e) => Some(e) }
    }
}
impl From<db::Error> for WalletError { fn from(e: db::Error) -> Self { Self::Db(e) } }
impl From<AmountError> for WalletError { fn from(e: AmountError) -> Self { Self::Amount(e) } }

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum WalletMutation { Applied(IskAmount), Missing, Insufficient(IskAmount), Overflow }
fn amount(row: &tokio_postgres::Row) -> Result<IskAmount, WalletError> {
    Ok(IskAmount::parse_decimal(row.get::<_, &str>("balance"))?)
}
fn mutation(row: &tokio_postgres::Row) -> Result<WalletMutation, WalletError> {
    match row.get::<_, &str>("status") {
        "applied" => Ok(WalletMutation::Applied(amount(row)?)),
        "missing" => Ok(WalletMutation::Missing),
        "insufficient" => Ok(WalletMutation::Insufficient(amount(row)?)),
        "overflow" => Ok(WalletMutation::Overflow),
        _ => Err(AmountError.into()),
    }
}

#[cfg(test)]
mod tests;
